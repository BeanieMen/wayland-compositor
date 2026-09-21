mod drm;
mod input;
mod udev;

use std::{os::unix::io::OwnedFd, sync::Arc, time::Duration};

use smithay::{
    backend::{
        drm::DrmDevice,
        input::{
            AbsolutePositionEvent, Event, InputEvent, KeyState, KeyboardKeyEvent,
            PointerButtonEvent, PointerMotionEvent,
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Color32F, ImportAll,
            element::{
                Id, Kind, render_elements,
                solid::SolidColorRenderElement,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            utils::on_commit_buffer_handler,
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
    },
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
    desktop::{Space, Window},
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, LedState},
        pointer::{ButtonEvent, MotionEvent},
    },
    output::Output,
    reexports::{
        calloop::{
            EventLoop,
            timer::{TimeoutAction, Timer},
        },
        input::{Device as LibinputDevice, DeviceCapability, Libinput},
        wayland_server::{Display, protocol::wl_seat},
    },
    utils::{IsAlive, Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Scale, Serial, Size},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;
use wayland_server::{
    Client,
    backend::{ClientData, ClientId, DisconnectReason},
    protocol::{wl_buffer, wl_surface::WlSurface},
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
        self.space.map_element(window.clone(), (0, 0), true);
        self.tile_order.push(window.clone());
        if let Some(keyboard) = self.seat.get_keyboard() {
            focus_window(self, &keyboard, Some(&window), SERIAL_COUNTER.next_serial());
        }
        self.layout_dirty = true;
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let surface = surface.wl_surface();
        if let Some(window) = self
            .tile_order
            .iter()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|top| top.wl_surface() == surface)
            })
            .cloned()
        {
            self.space.unmap_elem(&window);
            self.tile_order.retain(|window| {
                !window
                    .toplevel()
                    .is_some_and(|top| top.wl_surface() == surface)
            });
        }
        if self.focused_surface.as_ref() == Some(surface) {
            self.focused_surface = None;
        }
        self.layout_dirty = true;
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
    /// Stable layout order. `Space` is a stacking/input structure and must not
    /// decide tile positions, otherwise focusing a window reorders the layout.
    tile_order: Vec<Window>,
    focused_surface: Option<WlSurface>,
    layout_dirty: bool,
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
    session_active: bool,
    session: LibSeatSession,
}

fn dwindle_rectangles(
    count: usize,
    bounds: Rectangle<i32, Logical>,
) -> Vec<Rectangle<i32, Logical>> {
    fn split(
        index: usize,
        remaining: usize,
        bounds: Rectangle<i32, Logical>,
        depth: usize,
        rectangles: &mut Vec<Rectangle<i32, Logical>>,
    ) {
        if remaining == 1 {
            rectangles[index] = bounds;
            return;
        }

        // First split left/right, then top/bottom, and alternate thereafter.
        // The first window owns one half; all later windows recursively dwindle
        // into the other half.
        // Keep compatibility with the Rust 2024 minimum supported compiler.
        #[allow(clippy::manual_is_multiple_of)]
        let split_vertically = depth % 2 == 0;
        let (first, rest) = if split_vertically {
            let first_width = bounds.size.w / 2;
            (
                Rectangle::new(bounds.loc, Size::from((first_width, bounds.size.h))),
                Rectangle::new(
                    (bounds.loc.x + first_width, bounds.loc.y).into(),
                    Size::from((bounds.size.w - first_width, bounds.size.h)),
                ),
            )
        } else {
            let first_height = bounds.size.h / 2;
            (
                Rectangle::new(bounds.loc, Size::from((bounds.size.w, first_height))),
                Rectangle::new(
                    (bounds.loc.x, bounds.loc.y + first_height).into(),
                    Size::from((bounds.size.w, bounds.size.h - first_height)),
                ),
            )
        };
        rectangles[index] = first;
        split(index + 1, remaining - 1, rest, depth + 1, rectangles);
    }

    let mut rectangles = vec![Rectangle::default(); count];
    if count != 0 {
        split(0, count, bounds, 0, &mut rectangles);
    }
    rectangles
}

fn focus_window(
    app: &mut App,
    keyboard: &smithay::input::keyboard::KeyboardHandle<App>,
    window: Option<&Window>,
    serial: Serial,
) {
    let target = window.and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
    if app.focused_surface == target {
        return;
    }

    app.focused_surface = target.clone();
    keyboard.set_focus(app, target.clone(), serial);
    for element in app.space.elements() {
        if let Some(toplevel) = element.toplevel() {
            let active = Some(toplevel.wl_surface()) == target.as_ref();
            toplevel.with_pending_state(|state| {
                if active {
                    state.states.set(xdg_toplevel::State::Activated);
                } else {
                    state.states.unset(xdg_toplevel::State::Activated);
                }
            });
            let _ = toplevel.send_pending_configure();
        }
    }
}

impl State {
    /// Resolve focus against compositor-owned tile bounds, not a client's
    /// surface/input region. A client may temporarily render smaller than its
    /// assigned tile while it processes a configure, but the whole tile should
    /// still select that client.
    fn tiled_window_under(&self, point: Point<f64, Logical>) -> Option<Window> {
        let (width, height) = self
            .output
            .current_mode()
            .map(|mode| (mode.size.w, mode.size.h))
            .unwrap_or((1920, 1080));
        let bounds = Rectangle::new((0, 0).into(), Size::from((width, height)));
        self.app
            .tile_order
            .iter()
            .zip(dwindle_rectangles(self.app.tile_order.len(), bounds))
            .find(|(_, tile)| tile.to_f64().contains(point))
            .map(|(window, _)| window.clone())
    }

    fn apply_tiling(&mut self) {
        self.app.tile_order.retain(|window| window.alive());
        let count = self.app.tile_order.len();
        if count == 0 {
            self.app.layout_dirty = false;
            return;
        }
        let (width, height) = self
            .output
            .current_mode()
            .map(|mode| (mode.size.w, mode.size.h))
            .unwrap_or((1920, 1080));

        let rectangles = dwindle_rectangles(
            count,
            Rectangle::new((0, 0).into(), Size::from((width, height))),
        );
        for (window, rect) in self.app.tile_order.clone().into_iter().zip(rectangles) {
            self.app.space.map_element(window.clone(), rect.loc, false);
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some(rect.size);
                    state.bounds = Some(Size::from((width, height)));
                });
                let _ = toplevel.send_pending_configure();
            }
        }
        self.app.layout_dirty = false;
    }

    fn move_focused_tile(&mut self, dx: i32, dy: i32) {
        let Some(focused) = self.app.focused_surface.as_ref() else {
            return;
        };
        let Some(index) = self.app.tile_order.iter().position(|window| {
            window
                .toplevel()
                .is_some_and(|top| top.wl_surface() == focused)
        }) else {
            return;
        };
        let count = self.app.tile_order.len();
        let (width, height) = self
            .output
            .current_mode()
            .map(|mode| (mode.size.w, mode.size.h))
            .unwrap_or((1920, 1080));
        let rectangles = dwindle_rectangles(
            count,
            Rectangle::new((0, 0).into(), Size::from((width, height))),
        );
        let center = |rect: Rectangle<i32, Logical>| {
            (rect.loc.x * 2 + rect.size.w, rect.loc.y * 2 + rect.size.h)
        };
        let (x, y) = center(rectangles[index]);
        let destination = rectangles
            .iter()
            .enumerate()
            .filter(|(candidate, _)| *candidate != index)
            .filter_map(|(candidate, rect)| {
                let (other_x, other_y) = center(*rect);
                let primary = if dx != 0 {
                    (other_x - x) * dx
                } else {
                    (other_y - y) * dy
                };
                let secondary = if dx != 0 {
                    (other_y - y).abs()
                } else {
                    (other_x - x).abs()
                };
                (primary > 0).then_some((candidate, primary, secondary))
            })
            .min_by_key(|(_, primary, secondary)| (*primary, *secondary))
            .map(|(candidate, _, _)| candidate);
        if let Some(destination) = destination {
            self.app.tile_order.swap(index, destination);
            self.app.layout_dirty = true;
        }
    }
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
        tile_order: Vec::new(),
        focused_surface: None,
        layout_dirty: false,
    };

    let keyboard = app.seat.add_keyboard(input::xkb_config(), 200, 200)?;
    // One Wayland seat pointer receives motion from every libinput pointer
    // device added below (mice, touchpads, trackballs, and tablets).
    let pointer = app.seat.add_pointer();

    let (mut session, session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    eprintln!("[SESSION] Connected to libseat on seat: {}", seat_name);

    let (_, nodes) = udev::scan(&seat_name)?;
    let node = nodes.into_iter().next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no DRM nodes for seat")
    })?;
    eprintln!("[DRM] Initializing DRM output on node: {}", node.display());

    let drm_fd = session.open(
        &node,
        smithay::reexports::rustix::fs::OFlags::RDWR
            | smithay::reexports::rustix::fs::OFlags::CLOEXEC,
    )?;
    let (output, _mode, drm, drm_notifier, renderer, compositor) =
        drm::init_output::<App>(drm_fd, &dh)?;
    eprintln!("[DRM] GBM+GLES compositor initialised successfully");

    app.space.map_output(&output, (0, 0));

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| std::io::Error::other("udev_assign_seat failed"))?;
    let libinput_backend = LibinputInputBackend::new(libinput);
    let initial_pointer_location = output
        .current_mode()
        .map(|mode| Point::from((mode.size.w as f64 / 2.0, mode.size.h as f64 / 2.0)))
        .unwrap_or_else(|| Point::from((960.0, 540.0)));

    let mut state = State {
        app,
        display,
        output,
        drm,
        renderer,
        compositor,
        start: std::time::Instant::now(),
        pointer_location: initial_pointer_location,
        session_active: true,
        session: session.clone(),
    };

    let handle = event_loop.handle();

    handle.insert_source(
        session_notifier,
        |event, _, state: &mut State| match event {
            SessionEvent::PauseSession => {
                eprintln!("[SESSION] Session paused (VT switch away)");
                state.session_active = false;
                state.drm.pause();
            }
            SessionEvent::ActivateSession => {
                eprintln!("[SESSION] Session activated (VT switch back)");
                if let Err(e) = state.drm.activate(true) {
                    eprintln!("[SESSION] activate error: {:?}", e);
                } else {
                    state.compositor.reset_buffers();
                    state.app.layout_dirty = true;
                    state.session_active = true;
                }
            }
        },
    )?;

    handle.insert_source(drm_notifier, |_, _, state: &mut State| {
        if let Err(e) = state.compositor.frame_submitted() {
            eprintln!("[DRM] frame_submitted error: {:?}", e);
        }
    })?;

    handle.insert_source(
        libinput_backend,
        move |event, _, state: &mut State| match event {
            InputEvent::DeviceAdded { device } => {
                eprintln!("[INPUT] Device added: {}", device.name());
                if device.has_capability(DeviceCapability::Pointer) {
                    eprintln!("[INPUT] Pointing device ready: {}", device.name());
                }
                state.app.input_devices.push(device);
            }
            InputEvent::Keyboard { event } => {
                let serial = SERIAL_COUNTER.next_serial();
                let mut killswitch = false;
                let mut launch_term = false;
                let mut close_focused = false;
                let mut move_tile = None;
                let mut switch_tty = None;

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
                            } else if let Some(direction) = input::tile_move_direction(mods, sym) {
                                move_tile = Some(direction);
                                FilterResult::Intercept(())
                            } else if let Some(tty) = input::tty_switch_destination(mods, sym)
                                .or_else(|| input::tty_switch_fallback_destination(mods, sym))
                            {
                                switch_tty = Some(tty);
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
                    if let Some(surface) = state.app.focused_surface.as_ref()
                        && let Some(window) = state.app.tile_order.iter().find(|window| {
                            window
                                .toplevel()
                                .is_some_and(|top| top.wl_surface() == surface)
                        })
                        && let Some(toplevel) = window.toplevel()
                    {
                        toplevel.send_close();
                    }
                    return;
                }
                if let Some((dx, dy)) = move_tile {
                    state.move_focused_tile(dx, dy);
                }
                if let Some(tty) = switch_tty {
                    eprintln!("[SESSION] Requesting switch to VT {}", tty);
                    if let Err(error) = state.session.change_vt(tty) {
                        eprintln!("[SESSION] Could not switch to VT {}: {:?}", tty, error);
                    }
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
                let focus_target = state.tiled_window_under(state.pointer_location);

                focus_window(&mut state.app, &keyboard, focus_target.as_ref(), serial);

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
                state.pointer_location =
                    Point::from((event.x_transformed(w as i32), event.y_transformed(h as i32)));

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
                let focus_target = state.tiled_window_under(state.pointer_location);

                focus_window(&mut state.app, &keyboard, focus_target.as_ref(), serial);

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
                let focus_target = state.tiled_window_under(state.pointer_location);

                focus_window(&mut state.app, &keyboard, focus_target.as_ref(), serial);

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
        },
    )?;

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

            if !state.session_active {
                return TimeoutAction::ToDuration(Duration::from_millis(16));
            }

            let elapsed = state.start.elapsed();

            if state.app.layout_dirty {
                state.apply_tiling();
            }

            let windows = state.app.tile_order.clone();
            for window in &windows {
                // Notify clients it's safe to render the next frame
                window.send_frame(&state.output, elapsed, Some(Duration::ZERO), |_, _| None);
            }

            let scale = Scale::from(1.0_f64);
            let pointer_phys: Point<i32, Physical> =
                state.pointer_location.to_physical_precise_round(scale);
            // `DrmCompositor` expects elements front-to-back. Put the cursor
            // first so opaque client surfaces cannot occlude it.
            let cursor_rect = Rectangle::new(pointer_phys, (28, 28).into());
            let cursor_element = SolidColorRenderElement::new(
                Id::new(),
                cursor_rect,
                0usize,
                Color32F::new(0.2, 1.0, 0.2, 1.0),
                Kind::Cursor,
            );
            let mut elements = vec![CustomRenderElement::Solid(cursor_element)];
            elements.extend(windows.iter().flat_map(|window| {
                let loc_logical = state.app.space.element_location(window).unwrap_or_default();
                let loc_phys: Point<i32, Physical> = loc_logical.to_physical_precise_round(scale);
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
            }));

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
                    if !result.is_empty
                        && let Err(e) = state.compositor.queue_frame(())
                    {
                        eprintln!("[RENDER] queue_frame error: {:?}", e);
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
