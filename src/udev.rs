use std::{io, path::PathBuf};

pub use smithay::backend::udev::{UdevBackend, UdevEvent};

pub fn scan(seat: &str) -> io::Result<(UdevBackend, Vec<PathBuf>)> {
    let backend = UdevBackend::new(seat)?;
    let nodes = backend
        .device_list()
        .map(|(_, path)| path.to_path_buf())
        .collect();
    Ok((backend, nodes))
}

pub fn added_path(event: &UdevEvent) -> Option<PathBuf> {
    match event {
        UdevEvent::Added { path, .. } => Some(path.clone()),
        UdevEvent::Changed { .. } | UdevEvent::Removed { .. } => None,
    }
}
