//! Manager module - administrative interface for profile and daemon lifecycle

mod app;
pub mod components;
mod key_capture;
pub mod state;
pub mod utils;
mod window_lifecycle;
pub mod x11_utils;

pub use app::run_manager;
