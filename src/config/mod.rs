//! Configuration management
//!
//! Handles profile-based configuration with JSON persistence.
//! Supports multiple profiles, each with visual settings, hotkey bindings,
//! and per-source thumbnail positions.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

pub mod backup;
pub mod hotkey_binding;
pub mod profile;
pub mod runtime;
pub mod serialization;

pub use hotkey_binding::HotkeyBinding;
pub use profile::HotkeyBackendType;
pub use runtime::{DaemonConfig, DisplayConfig};

pub(crate) fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create config directory {:?}", parent))?;

    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Failed to create temporary config file in {:?}", parent))?;

    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => temp
            .as_file()
            .set_permissions(metadata.permissions())
            .with_context(|| format!("Failed to preserve permissions for {:?}", path))?,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to inspect existing config at {:?}", path));
        }
    }

    temp.write_all(contents)
        .with_context(|| format!("Failed to stage config for {:?}", path))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("Failed to sync staged config for {:?}", path))?;

    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("Failed to atomically replace config at {:?}", path))?;

    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("Failed to sync config directory {:?}", parent))?;

    Ok(())
}
