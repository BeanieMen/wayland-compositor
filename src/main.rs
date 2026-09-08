mod drm;
mod input;
mod udev;

use std::{os::unix::io::OwnedFd, sync::Arc, time::Duration};

use smithay::{
    backend::{
        drm::{DrmDevice, DrmSurface},
        input::{Event, InputEvent, KeyState, KeyboardKeyEvent, PointerButtonEvent, PointerMotionEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            damage::OutputDamageTracker,
            element::{
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind,
            },
            pixman::PixmanRenderer,
            utils::on_commit_buffer_handler,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
    },
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
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
    utils::{Point, Serial, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            with_surface_tree_downward, CompositorClientState, CompositorHandler, CompositorState,
            SurfaceAttributes, TraversalAction,
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
        wl_surface::{self, WlSurface},
    },
    Client,
};

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

struct State {
    app: App,
    display: Display<App>,
    output: Output,
    drm: DrmDevice,
    _drm_surface: DrmSurface,
    renderer: PixmanRenderer,
    _damage_tracker: OutputDamageTracker,
    start: std::time::Instant,
    pointer_location: Point<f64, smithay::utils::Logical>,
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

    let mut app = App {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<App>(&dh),
        shm_state,
        seat_state,
        data_device_state: DataDeviceState::new::<App>(&dh),
        seat,
        running: true,
        input_devices: Vec::new(),
    };
    let keyboard = app.seat.add_keyboard(input::xkb_config(), 200, 200)?;
    let pointer = app.seat.add_pointer();

    let (session, session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    eprintln!("[SESSION] Connected to libseat on seat: {}", seat_name);

    let (udev_backend, nodes) = udev::scan(&seat_name)?;
    let node = nodes.into_iter().next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no DRM nodes for seat")
    })?;
    eprintln!("[DRM] Initializing DRM output on node: {}", node.display());
    let (output, _mode, drm, drm_notifier, drm_surface, renderer, damage_tracker) =
        drm::init_output::<App>(&node, &dh)?;
    eprintln!("[DRM] Surface and PixmanRenderer initialized successfully");

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
        _drm_surface: drm_surface,
        renderer,
        _damage_tracker: damage_tracker,
        start: std::time::Instant::now(),
        pointer_location: Point::from((0.0, 0.0)),
    };

    let handle = event_loop.handle();

    // Session pause/resume with DRM and Libinput synchronization to prevent race conditions on VT switch.
    handle.insert_source(session_notifier, |event, _, state: &mut State| match event {
        SessionEvent::PauseSession => {
            eprintln!("[SESSION] Session paused (VT switch away)");
            state.drm.pause();
        }
        SessionEvent::ActivateSession => {
            eprintln!("[SESSION] Session activated (VT switch back)");
            let _ = state.drm.activate(true);
        }
    })?;

    handle.insert_source(udev_backend, |event, _, _: &mut State| {
        if let Some(path) = udev::added_path(&event) {
            eprintln!("[UDEV] Hotplug device added: {}", path.display());
        }
    })?;

    // DRM page-flip completions
    handle.insert_source(drm_notifier, |_, _, _: &mut State| {})?;

    // Input processing with keybindings (Super+Enter to spawn terminal, Super+Q / Super+W to close window, Ctrl+Alt+Backspace killswitch)
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
                eprintln!("[INPUT] Failsafe Ctrl+Alt+Backspace pressed. Exiting compositor...");
                state.app.running = false;
                return;
            }

            if launch_term {
                input::spawn_terminal();
                return;
            }

            if close_focused {
                eprintln!("[INPUT] Super+Q / Super+W pressed! Closing active focused window...");
                if let Some(surface) = state
                    .app
                    .xdg_shell_state
                    .toplevel_surfaces()
                    .iter()
                    .next()
                    .cloned()
                {
                    surface.send_close();
                }
                return;
            }

            if let Some(surface) = state
                .app
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .next()
                .cloned()
            {
                let wl_surface = surface.wl_surface().clone();
                keyboard.set_focus(&mut state.app, Some(wl_surface), serial);
            }
        }
        InputEvent::PointerMotion { event } => {
            let delta = Point::from((event.delta_x(), event.delta_y()));
            state.pointer_location += delta;
            let serial = SERIAL_COUNTER.next_serial();
            let under = state
                .app
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .next()
                .map(|s| (s.wl_surface().clone(), Point::from((0.0, 0.0))));
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
            let time = state.start.elapsed().as_millis() as u32;
            let (w, h) = state
                .output
                .current_mode()
                .map(|mode| (mode.size.w, mode.size.h))
                .unwrap_or((800, 600));

            let surfaces = state.app.xdg_shell_state.toplevel_surfaces();
            let count = surfaces.len();

            let _elements: Vec<WaylandSurfaceRenderElement<PixmanRenderer>> = surfaces
                .iter()
                .enumerate()
                .flat_map(|(i, surface)| {
                    let (x, y) = tile_offset(i, count, w, h);
                    render_elements_from_surface_tree(
                        &mut state.renderer,
                        surface.wl_surface(),
                        (x, y),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect();

            for surface in surfaces.iter() {
                send_frames_surface_tree(surface.wl_surface(), time);
            }
            let _ = state.display.flush_clients();
            TimeoutAction::ToDuration(Duration::from_millis(16))
        },
    )?;

    eprintln!("[INIT] Event loop starting. Press Super+Enter to open terminal, Super+Q/Super+W to close window, Ctrl+Alt+Backspace to exit.");
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

pub fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
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
