//! Hotkey backend abstraction layer
//!
//! Provides a trait-based interface for different hotkey input backends.
//! Currently supports:
//! - X11 XGrabKey (default, secure, no permissions)
//! - evdev raw input (optional, requires input group)

use anyhow::Result;
use std::thread::JoinHandle;
use tokio::sync::mpsc::Sender;

use crate::config::HotkeyBinding;
use crate::input::listener::TimestampedCommand;

/// Capabilities and limitations of a hotkey backend
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BackendCapabilities {
    /// Can detect modifiers from different physical devices
    /// (e.g., Shift on keyboard + Mouse4 on mouse)
    pub supports_cross_device_modifiers: bool,

    /// Can filter to specific input devices
    pub supports_device_filtering: bool,

    /// Requires special system permissions
    pub requires_permissions: bool,

    /// Human-readable description of permission requirements
    pub permission_description: Option<String>,
}

/// Configuration for hotkey bindings
#[derive(Debug, Clone)]
pub struct HotkeyConfiguration {
    pub forward_key: Option<HotkeyBinding>,
    pub backward_key: Option<HotkeyBinding>,
    pub character_hotkeys: Vec<HotkeyBinding>,
    pub profile_hotkeys: Vec<HotkeyBinding>,
    pub toggle_skip_key: Option<HotkeyBinding>,
}

/// Hotkey backend trait
///
/// Each backend must implement this trait to be used by the daemon
pub trait HotkeyBackend: Sized {
    /// Spawn the backend's listening threads
    ///
    /// Returns handles to spawned threads for cleanup on shutdown
    fn spawn(
        sender: Sender<TimestampedCommand>,
        config: HotkeyConfiguration,
        device_id: Option<String>,
        require_eve_focus: bool,
    ) -> Result<Vec<JoinHandle<()>>>;

    /// Check if this backend is available on the current system
    fn is_available() -> bool;

    /// Get human-readable backend name
    #[allow(dead_code)]
    fn name() -> &'static str;

    /// Get backend capabilities and limitations
    #[allow(dead_code)]
    fn capabilities() -> BackendCapabilities;
}
