//! Daemon main loop and runtime initialization

mod cycle_state;
mod dispatcher;
pub mod font;
mod main_loop;

pub mod handlers;
mod overlay;
mod renderer;
mod session_state;
mod snapping;
mod thumbnail;
pub mod window_detection;

pub use crate::input::listener::list_input_devices;
pub use font::{list_fonts, select_best_default_font};
pub use main_loop::run_daemon;
