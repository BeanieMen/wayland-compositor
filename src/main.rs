mod drm;
mod input;
mod udev;

use std::{os::unix::io::OwnedFd, sync::Arc, time::Duration};

use smithay::{
    backend::{
        drm::DrmDevice,
        input::{AbsolutePositionEvent, Event, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent, PointerMotionEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::{
                solid::SolidColorRenderElement,
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                render_elements, Id, Kind,
            },
            gles::GlesRenderer,
            utils::on_commit_buffer_handler,
            Color32F, ImportAll,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
    },
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
    desktop::{Space, Window},
    input::{
        keyboard::{FilterResult, LedState},
        pointer::{ButtonEvent, MotionEvent},
        Seat, SeatHandler, SeatState,
    },
    output::Output,
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        input::{Device as LibinputDevice, Libinput},
        wayland_server::{protocol::wl_seat, Display},
    },
    utils::{Physical, Point, Rectangle, Scale, Serial, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState,
        },
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;
use wayland_server::{
    backend::{ClientData, ClientId, DisconnectReason},
    protocol::{
        wl_buffer,
        wl_surface::WlSurface,
    },
    Client,
};

use drm::BeanDrmCompositor;

render_elements! {
    pub CustomRenderElement<R> where R: ImportAll;
    Surface=WaylandSurfaceRenderElement<R>,
    Solid=SolidColorRenderElement,
}


impl BufferHandler for App {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl XdgShellHandler for App {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        eprintln!("[XDG] New toplevel surface created");
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();

        let window = Window::new_wayland_window(surface);
        self.space.map_element(window, (0, 0), true);
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}
    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl SelectionHandler for App {
    type SelectionUserData = ();
}

impl DataDeviceHandler for App {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for App {}
impl ServerDndGrabHandler for App {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl CompositorHandler for App {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Advance buffer release / damage tracking — does NOT touch Space.
        on_commit_buffer_handler::<Self>(surface);
    }
}

impl ShmHandler for App {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl OutputHandler for App {}

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        eprintln!("[LED] LED state changed: {:?}", led_state);
        for dev in &mut self.input_devices {
            input::forward_leds(dev, led_state);
        }
    }
}


struct App {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    seat: Seat<Self>,
    running: bool,
    input_devices: Vec<LibinputDevice>,
    space: Space<Window>,
}

struct State {
    app: App,
    display: Display<App>,
    output: Output,
    drm: DrmDevice,
    renderer: GlesRenderer,
    compositor: BeanDrmCompositor,
    start: std::time::Instant,
    pointer_location: Point<f64, smithay::utils::Logical>,
}


fn tile_offset(index: usize, count: usize, win_w: i32, win_h: i32) -> (i32, i32) {
    if count == 0 {
        return (0, 0);
    }
    let cols = (count as f64).sqrt().ceil() as i32;
    let rows = count.div_ceil(cols as usize) as i32;
    let tile_w = win_w / cols.max(1);
    let tile_h = win_h / rows.max(1);
    ((index as i32 % cols) * tile_w, (index as i32 / cols) * tile_h)
}


fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[INIT] Starting beanwm-v2 compositor...");
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display: Display<App> = Display::new()?;
    let dh = display.handle();

    let compositor_state = CompositorState::new::<App>(&dh);
    let shm_state = ShmState::new::<App>(&dh, vec![]);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "seat0");
    let _output_manager = OutputManagerState::new_with_xdg_output::<App>(&dh);

    let space = Space::default();

    let mut app = App {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<App>(&dh),
        shm_state,
        seat_state,
        data_device_state: DataDeviceState::new::<App>(&dh),
        seat,
        running: true,
        input_devices: Vec::new(),
        space,
    };

    let keyboard = app.seat.add_keyboard(input::xkb_config(), 200, 200)?;
    let pointer = app.seat.add_pointer();

    let (session, session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    eprintln!("[SESSION] Connected to libseat on seat: {}", seat_name);

    let (_, nodes) = udev::scan(&seat_name)?;
    let node = nodes.into_iter().next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no DRM nodes for seat")
    })?;
    eprintln!("[DRM] Initializing DRM output on node: {}", node.display());

    let (output, _mode, drm, drm_notifier, renderer, compositor) =
        drm::init_output::<App>(&node, &dh)?;
    eprintln!("[DRM] GBM+GLES compositor initialised successfully");

    app.space.map_output(&output, (0, 0));

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.into());
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "udev_assign_seat failed"))?;
    let libinput_backend = LibinputInputBackend::new(libinput);

    let mut state = State {
        app,
        display,
        output,
        drm,
        renderer,
        compositor,
        start: std::time::Instant::now(),
        pointer_location: Point::from((0.0, 0.0)),
    };

    let handle = event_loop.handle();

    handle.insert_source(session_notifier, |event, _, state: &mut State| match event {
        SessionEvent::PauseSession => {
            eprintln!("[SESSION] Session paused (VT switch away)");
            state.drm.pause();
        }
        SessionEvent::ActivateSession => {
            eprintln!("[SESSION] Session activated (VT switch back)");
            if let Err(e) = state.drm.activate(true) {
                eprintln!("[SESSION] activate error: {:?}", e);
            }
        }
    })?;

    handle.insert_source(drm_notifier, |_, _, state: &mut State| {
        if let Err(e) = state.compositor.frame_submitted() {
            eprintln!("[DRM] frame_submitted error: {:?}", e);
        }
    })?;

    handle.insert_source(libinput_backend, move |event, _, state: &mut State| match event {
        InputEvent::DeviceAdded { device } => {
            eprintln!("[INPUT] Device added: {}", device.name());
            state.app.input_devices.push(device);
        }
        InputEvent::Keyboard { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let mut killswitch = false;
            let mut launch_term = false;
            let mut close_focused = false;

            if event.state() == KeyState::Pressed {
                keyboard.input::<(), _>(
                    &mut state.app,
                    event.key_code(),
                    event.state(),
                    serial,
                    event.time_msec(),
                    |_, mods, handle| {
                        let sym = handle.modified_sym();
                        eprintln!("[INPUT] Key press: sym={:?}, logo={}", sym, mods.logo);
                        if input::is_killswitch(mods, sym) {
                            killswitch = true;
                            FilterResult::Intercept(())
                        } else if input::is_terminal_shortcut(mods, sym) {
                            launch_term = true;
                            FilterResult::Intercept(())
                        } else if input::is_close_shortcut(mods, sym) {
                            close_focused = true;
                            FilterResult::Intercept(())
                        } else {
                            FilterResult::Forward
                        }
                    },
                );
            } else {
                keyboard.input::<(), _>(
                    &mut state.app,
                    event.key_code(),
                    event.state(),
                    serial,
                    event.time_msec(),
                    |_, _, _| FilterResult::Forward,
                );
            }

            if killswitch {
                eprintln!("[INPUT] Failsafe Ctrl+Alt+Backspace pressed. Exiting...");
                state.app.running = false;
                return;
            }
            if launch_term {
                input::spawn_terminal();
                return;
            }
            if close_focused {
                eprintln!("[INPUT] Super+Q/Super+W – closing focused window");
                if let Some(window) = state.app.space.elements().next().cloned() {
                    if let Some(toplevel) = window.toplevel() {
                        toplevel.send_close();
                    }
                }
                return;
            }

            let focus_surface: Option<WlSurface> = state
                .app
                .space
                .element_under(state.pointer_location)
                .and_then(|(w, _)| w.toplevel().map(|t| t.wl_surface().clone()))
                .or_else(|| {
                    state
                        .app
                        .space
                        .elements()
                        .next()
                        .and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()))
                });
            if let Some(wl_surface) = focus_surface {
                keyboard.set_focus(&mut state.app, Some(wl_surface), serial);
            }
        }
        InputEvent::PointerMotion { event } => {
            let delta = Point::from((event.delta_x(), event.delta_y()));
            state.pointer_location += delta;

            let (w, h) = state
                .output
                .current_mode()
                .map(|m| (m.size.w as f64, m.size.h as f64))
                .unwrap_or((1920.0, 1080.0));
            state.pointer_location.x = state.pointer_location.x.clamp(0.0, w);
            state.pointer_location.y = state.pointer_location.y.clamp(0.0, h);

            let serial = SERIAL_COUNTER.next_serial();
            let under_window = state
                .app
                .space
                .element_under(state.pointer_location)
                .map(|(w, p)| (w.clone(), p));

            let under = under_window.as_ref().and_then(|(w, pos)| {
                w.toplevel().map(|t| {
                    let pos_f64: Point<f64, smithay::utils::Logical> =
                        Point::from((pos.x as f64, pos.y as f64));
                    (t.wl_surface().clone(), pos_f64)
                })
            });

            // Focus follows pointer: set keyboard focus and update Activated xdg state
            if let Some((ref window, _)) = under_window {
                state.app.space.raise_element(window, true);
                let target_surface = window.toplevel().map(|t| t.wl_surface().clone());
                if let Some(ref surface) = target_surface {
                    keyboard.set_focus(&mut state.app, Some(surface.clone()), serial);
                }
                for element in state.app.space.elements() {
                    if let Some(toplevel) = element.toplevel() {
                        let active = Some(toplevel.wl_surface()) == target_surface.as_ref();
                        toplevel.with_pending_state(|s| {
                            if active {
                                s.states.set(xdg_toplevel::State::Activated);
                            } else {
                                s.states.unset(xdg_toplevel::State::Activated);
                            }
                        });
                        toplevel.send_configure();
                    }
                }
            } else {
                keyboard.set_focus(&mut state.app, Option::<WlSurface>::None, serial);
            }

            pointer.motion(
                &mut state.app,
                under,
                &MotionEvent {
                    location: state.pointer_location,
                    serial,
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let (w, h) = state
                .output
                .current_mode()
                .map(|m| (m.size.w as f64, m.size.h as f64))
                .unwrap_or((1920.0, 1080.0));
            state.pointer_location = Point::from((
                event.x_transformed(w as i32),
                event.y_transformed(h as i32),
            ));

            let serial = SERIAL_COUNTER.next_serial();
            let under_window = state
                .app
                .space
                .element_under(state.pointer_location)
                .map(|(w, p)| (w.clone(), p));

            let under = under_window.as_ref().and_then(|(w, pos)| {
                w.toplevel().map(|t| {
                    let pos_f64: Point<f64, smithay::utils::Logical> =
                        Point::from((pos.x as f64, pos.y as f64));
                    (t.wl_surface().clone(), pos_f64)
                })
            });

            // Focus follows pointer: set keyboard focus and update Activated xdg state
            if let Some((ref window, _)) = under_window {
                state.app.space.raise_element(window, true);
                let target_surface = window.toplevel().map(|t| t.wl_surface().clone());
                if let Some(ref surface) = target_surface {
                    keyboard.set_focus(&mut state.app, Some(surface.clone()), serial);
                }
                for element in state.app.space.elements() {
                    if let Some(toplevel) = element.toplevel() {
                        let active = Some(toplevel.wl_surface()) == target_surface.as_ref();
                        toplevel.with_pending_state(|s| {
                            if active {
                                s.states.set(xdg_toplevel::State::Activated);
                            } else {
                                s.states.unset(xdg_toplevel::State::Activated);
                            }
                        });
                        toplevel.send_configure();
                    }
                }
            } else {
                keyboard.set_focus(&mut state.app, Option::<WlSurface>::None, serial);
            }

            pointer.motion(
                &mut state.app,
                under,
                &MotionEvent {
                    location: state.pointer_location,
                    serial,
                    time: event.time_msec(),
                },
            );
        }
        InputEvent::PointerButton { event } => {
            let serial = SERIAL_COUNTER.next_serial();
            let under_window = state
                .app
                .space
                .element_under(state.pointer_location)
                .map(|(w, p)| (w.clone(), p));

            if let Some((ref window, _)) = under_window {
                state.app.space.raise_element(window, true);
                let target_surface = window.toplevel().map(|t| t.wl_surface().clone());
                if let Some(ref surface) = target_surface {
                    keyboard.set_focus(&mut state.app, Some(surface.clone()), serial);
                }
                for element in state.app.space.elements() {
                    if let Some(toplevel) = element.toplevel() {
                        let active = Some(toplevel.wl_surface()) == target_surface.as_ref();
                        toplevel.with_pending_state(|s| {
                            if active {
                                s.states.set(xdg_toplevel::State::Activated);
                            } else {
                                s.states.unset(xdg_toplevel::State::Activated);
                            }
                        });
                        toplevel.send_configure();
                    }
                }
            }

            pointer.button(
                &mut state.app,
                &ButtonEvent {
                    button: event.button_code(),
                    state: event.state(),
                    serial,
                    time: event.time_msec(),
                },
            );
        }
        _ => {}
    })?;

    // Wayland socket
    let socket_source = ListeningSocketSource::new_auto()?;
    let socket_name = socket_source.socket_name().to_string_lossy().into_owned();
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    }
    eprintln!("[WAYLAND] Listening on socket: {}", socket_name);

    handle.insert_source(socket_source, |stream, _, state: &mut State| {
        let mut handle = state.display.handle();
        let _ = handle.insert_client(stream, Arc::new(ClientState::default()));
    })?;

    handle.insert_source(
        Timer::from_duration(Duration::from_millis(16)),
        |_, _, state: &mut State| {
            let _ = state.display.dispatch_clients(&mut state.app);

            let elapsed = state.start.elapsed();

            let (w, h) = state
                .output
                .current_mode()
                .map(|m| (m.size.w, m.size.h))
                .unwrap_or((1920, 1080));

            let count = state.app.space.elements().count();
            let windows: Vec<Window> = state.app.space.elements().cloned().collect();
            for (i, window) in windows.iter().enumerate() {
                let (x, y) = tile_offset(i, count, w, h);
                state.app.space.map_element(window.clone(), (x, y), false);
                // Notify clients it's safe to render the next frame
                window.send_frame(
                    &state.output,
                    elapsed,
                    Some(Duration::ZERO),
                    |_, _| None,
                );
            }

            let scale = Scale::from(1.0_f64);
            let mut elements: Vec<CustomRenderElement<GlesRenderer>> = windows
                .iter()
                .flat_map(|window| {
                    let loc_logical = state
                        .app
                        .space
                        .element_location(window)
                        .unwrap_or_default();
                    let loc_phys: Point<i32, Physical> =
                        loc_logical.to_physical_precise_round(scale);
                    if let Some(toplevel) = window.toplevel() {
                        let surface_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                            render_elements_from_surface_tree(
                                &mut state.renderer,
                                toplevel.wl_surface(),
                                loc_phys,
                                scale,
                                1.0,
                                Kind::Unspecified,
                            );
                        surface_elements
                            .into_iter()
                            .map(CustomRenderElement::Surface)
                            .collect()
                    } else {
                        vec![]
                    }
                })
                .collect();

            let pointer_phys: Point<i32, Physical> =
                state.pointer_location.to_physical_precise_round(scale);
            let cursor_rect = Rectangle::new(pointer_phys, (12, 12).into());
            let cursor_element = SolidColorRenderElement::new(
                Id::new(),
                cursor_rect,
                0usize,
                Color32F::new(1.0, 1.0, 1.0, 1.0),
                Kind::Unspecified,
            );
            elements.push(CustomRenderElement::Solid(cursor_element));

            const CLEAR: [f32; 4] = [0.05, 0.05, 0.1, 1.0];
            match state
                .compositor
                .render_frame::<GlesRenderer, CustomRenderElement<GlesRenderer>>(
                    &mut state.renderer,
                    &elements,
                    CLEAR,
                    smithay::backend::drm::compositor::FrameFlags::DEFAULT,
                ) {
                Ok(result) => {
                    if !result.is_empty {
                        if let Err(e) = state.compositor.queue_frame(()) {
                            eprintln!("[RENDER] queue_frame error: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[RENDER] render_frame error: {:?}", e);
                }
            }

            let _ = state.display.flush_clients();
            TimeoutAction::ToDuration(Duration::from_millis(16))
        },
    )?;

    eprintln!(
        "[INIT] Event loop running. \
         Super+Enter = terminal, Super+Q/W = close window, Ctrl+Alt+Backspace = exit."
    );

    while state.app.running {
        if let Err(err) = event_loop.dispatch(Some(Duration::from_millis(16)), &mut state) {
            let io_err = std::io::Error::from(err);
            match io_err.kind() {
                std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::UnexpectedEof => {
                    continue;
                }
                _ => {
                    eprintln!("[EVENT LOOP ERROR] {:?}", io_err);
                }
            }
        }
    }

    eprintln!("[SHUTDOWN] Exiting beanwm-v2 cleanly.");
    Ok(())
}

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

delegate_xdg_shell!(App);
delegate_compositor!(App);
delegate_shm!(App);
delegate_output!(App);
delegate_seat!(App);
delegate_data_device!(App);
