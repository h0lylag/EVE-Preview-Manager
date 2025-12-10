//! Preview daemon - runs in background showing EVE window thumbnails

mod cycle_state;
mod daemon;
mod event_handler;
pub mod font;

mod session_state;
mod snapping;
mod thumbnail;
pub mod window_detection;

pub use crate::input::listener::list_input_devices;
pub use daemon::run_preview_daemon;
pub use font::{list_fonts, select_best_default_font};
