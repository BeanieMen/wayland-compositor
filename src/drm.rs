use std::{fs::OpenOptions, os::unix::io::OwnedFd, path::Path};

use smithay::{
    backend::{
        drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmSurface},
        renderer::damage::OutputDamageTracker,
        renderer::pixman::PixmanRenderer,
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        drm::control::{connector, crtc, Mode as DrmMode, Device as ControlDevice},
        wayland_server::{protocol::wl_output::WlOutput, DisplayHandle, GlobalDispatch},
    },
    utils::{DevPath, DeviceFd, Size},
    wayland::output::WlOutputData,
};

pub fn init_output<D>(
    node: &Path,
    dh: &DisplayHandle,
) -> Result<(Output, Mode, DrmDevice, DrmDeviceNotifier, DrmSurface, PixmanRenderer, OutputDamageTracker), Box<dyn std::error::Error>>
where
    D: GlobalDispatch<WlOutput, WlOutputData> + 'static,
{
    let file = OpenOptions::new().read(true).write(true).open(node)?;
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));
    let (mut device, notifier) = DrmDevice::new(drm_fd.clone(), true)?;

    let (name, physical, mode, crtc_handle, connector_handle, drm_mode) = first_connected_mode(&drm_fd)?;

    let surface = device.create_surface(crtc_handle, drm_mode, &[connector_handle])?;
    let renderer = PixmanRenderer::new()?;

    let output = Output::new(name, physical);
    output.add_mode(mode);
    output.set_preferred(mode);
    output.change_current_state(Some(mode), None, None, None);
    output.create_global::<D>(dh);

    let damage_tracker = OutputDamageTracker::from_output(&output);

    Ok((output, mode, device, notifier, surface, renderer, damage_tracker))
}

/// First connected connector on `fd` with at least one mode.
fn first_connected_mode(
    fd: &DrmDeviceFd,
) -> Result<(String, PhysicalProperties, Mode, crtc::Handle, connector::Handle, DrmMode), Box<dyn std::error::Error>> {
    let res = fd.resource_handles()?;
    let crtc_handle = *res.crtcs().first().ok_or("no CRTC handles available on DRM device")?;

    for handle in res.connectors() {
        let info = fd.get_connector(*handle, false)?;
        if info.state() != connector::State::Connected {
            continue;
        }
        if let Some(&drm_mode) = info.modes().first() {
            let (w_mm, h_mm) = info.size().unwrap_or((0, 0));
            let physical = PhysicalProperties {
                size: Size::from((w_mm as i32, h_mm as i32)),
                subpixel: Subpixel::Unknown,
                make: "smithay".into(),
                model: info.interface().as_str().into(),
            };
            let conn_handle = *handle;
            return Ok((
                info.to_string(),
                physical,
                Mode::from(drm_mode),
                crtc_handle,
                conn_handle,
                drm_mode,
            ));
        }
    }
    Err(format!("no connected connector with a mode on {:?}", fd.dev_path()).into())
}
