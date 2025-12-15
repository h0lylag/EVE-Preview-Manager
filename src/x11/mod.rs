//! X11 u window detection.

mod context;
mod window;

pub use context::{AppContext, CachedAtoms, CachedFormats, to_fixed};
pub use window::{
    activate_window, get_active_eve_window, get_window_class, is_eve_window_focused, is_window_eve,
    is_window_minimized, minimize_window,
};
