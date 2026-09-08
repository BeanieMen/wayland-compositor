mod drm;
mod input;
mod udev;

use std::{os::unix::io::OwnedFd, sync::Arc};

use smithay::{
    backend::{
        drm::DrmDevice,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::utils::on_commit_buffer_handler,
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
    },
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_shell,
    input::{Seat, SeatHandler, SeatState},
    output::Output,
    reexports::{
        calloop::EventLoop,
        input::Libinput,
        wayland_server::{protocol::wl_seat, Display},
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
        shm::{ShmHandler, ShmState},
    },
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;
use wayland_server::{
    backend::{ClientData, ClientId, DisconnectReason},
    protocol::{wl_buffer, wl_surface::WlSurface},
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
}

struct App {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    seat: Seat<Self>,
}

#[allow(dead_code)]
struct State {
    app: App,
    display: Display<App>,
    output: Output,
    drm: DrmDevice,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display: Display<App> = Display::new()?;
    let dh = display.handle();

    let compositor_state = CompositorState::new::<App>(&dh);
    let shm_state = ShmState::new::<App>(&dh, vec![]);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "seat0");
    let _output_manager = OutputManagerState::new_with_xdg_output::<App>(&dh);

    let app = App {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<App>(&dh),
        shm_state,
        seat_state,
        data_device_state: DataDeviceState::new::<App>(&dh),
        seat,
    };

    let (session, session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();

    let (udev_backend, nodes) = udev::scan(&seat_name)?;
    let node = nodes.into_iter().next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no DRM nodes for seat")
    })?;
    let (output, _mode, drm, drm_notifier) = drm::init_output::<App>(&node, &dh)?;

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.into());
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "udev_assign_seat failed"))?;

    let state = State {
        app,
        display,
        output,
        drm,
    };

    let handle = event_loop.handle();

    handle.insert_source(session_notifier, |event, _, state: &mut State| match event {
        SessionEvent::PauseSession => {
            state.drm.pause();
        }
        SessionEvent::ActivateSession => {
            let _ = state.drm.activate(true);
        }
    })?;

    handle.insert_source(udev_backend, |event, _, _: &mut State| {
        if let Some(path) = udev::added_path(&event) {
            eprintln!("hotplug added: {}", path.display());
        }
    })?;

    handle.insert_source(drm_notifier, |_, _, _: &mut State| {})?;

    let _ = state;
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
