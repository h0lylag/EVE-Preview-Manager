//! Configuration management
//!
//! Handles profile-based configuration with JSON persistence.
//! Supports multiple profiles, each with visual settings, hotkey bindings,
//! and per-character thumbnail positions.

pub mod hotkey_binding;
pub mod profile;
pub mod runtime;

pub use hotkey_binding::HotkeyBinding;
pub use profile::HotkeyBackendType;
pub use runtime::{DaemonConfig, DisplayConfig};
