use std::{collections::HashSet, fs::OpenOptions, os::unix::io::OwnedFd, path::Path};

use drm_fourcc::DrmFourcc;
use smithay::{
    backend::{
        allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        drm::{
            compositor::DrmCompositor,
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmDeviceNotifier,
        },
        egl::{context::ContextPriority, EGLContext, EGLDisplay},
        renderer::gles::GlesRenderer,
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        drm::control::{connector, crtc, Device as ControlDevice, Mode as DrmMode},
        wayland_server::{protocol::wl_output::WlOutput, DisplayHandle, GlobalDispatch},
    },
    utils::{DevPath, DeviceFd, Size},
    wayland::output::WlOutputData,
};

pub type BeanDrmCompositor = DrmCompositor<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

pub fn init_output<D>(
    node: &Path,
    dh: &DisplayHandle,
) -> Result<
    (Output, Mode, DrmDevice, DrmDeviceNotifier, GlesRenderer, BeanDrmCompositor),
    Box<dyn std::error::Error>,
>
where
    D: GlobalDispatch<WlOutput, WlOutputData> + 'static,
{
    // ── 1. Open the DRM node ─────────────────────────────────────────────────
    let file = OpenOptions::new().read(true).write(true).open(node)?;
    let drm_fd = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));
    let (mut drm_device, notifier) = DrmDevice::new(drm_fd.clone(), true)?;

    let (name, physical, mode, crtc_handle, connector_handle, drm_mode) =
        first_connected_mode(&drm_fd)?;

    let drm_surface = drm_device.create_surface(crtc_handle, drm_mode, &[connector_handle])?;

    let gbm_device: GbmDevice<DrmDeviceFd> = GbmDevice::new(drm_fd.clone())?;


    let egl_display = unsafe { EGLDisplay::new(gbm_device.clone())? };
    let egl_context = EGLContext::new_with_priority(&egl_display, ContextPriority::High)?;

    
    let renderer_formats: HashSet<_> = egl_context
        .dmabuf_texture_formats()
        .iter()
        .copied()
        .collect();

    let renderer = unsafe { GlesRenderer::new(egl_context)? };

    let allocator = GbmAllocator::new(gbm_device.clone(), GbmBufferFlags::RENDERING);
    let exporter = GbmFramebufferExporter::new(gbm_device.clone(), None);

    let output = Output::new(name, physical);
    output.add_mode(mode);
    output.set_preferred(mode);
    output.change_current_state(Some(mode), None, None, None);
    output.create_global::<D>(dh);

    let color_formats = [DrmFourcc::Xrgb8888, DrmFourcc::Argb8888];

    let drm_compositor = BeanDrmCompositor::new(
        &output,
        drm_surface,
        None, // no overlay planes for now
        allocator,
        exporter,
        color_formats,
        renderer_formats,
        drm_device.cursor_size(),
        Some(gbm_device),
    )?;

    Ok((output, mode, drm_device, notifier, renderer, drm_compositor))
}

fn first_connected_mode(
    fd: &DrmDeviceFd,
) -> Result<
    (String, PhysicalProperties, Mode, crtc::Handle, connector::Handle, DrmMode),
    Box<dyn std::error::Error>,
> {
    let res = fd.resource_handles()?;
    let crtc_handle = *res
        .crtcs()
        .first()
        .ok_or("no CRTC handles available on DRM device")?;

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
                make: "beanwm".into(),
                model: info.interface().as_str().into(),
            };
            return Ok((
                format!("{:?}", info.interface()),
                physical,
                Mode::from(drm_mode),
                crtc_handle,
                *handle,
                drm_mode,
            ));
        }
    }
    Err(format!("no connected connector with a mode on {:?}", fd.dev_path()).into())
}
