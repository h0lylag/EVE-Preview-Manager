//! X11 utilities and cached state
//!
//! Provides helper functions for X11 window management, atom caching,
//! and EVE Online window detection.

mod context;
mod window;

pub use context::{AppContext, CachedAtoms, CachedFormats, to_fixed};
pub use window::{
    activate_window, get_window_class, is_eve_window_class, is_eve_window_focused, is_window_eve,
    is_window_minimized, minimize_window,
};
