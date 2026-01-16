//! Manager module - administrative interface for profile and daemon lifecycle

mod app_slint;
pub mod components;
mod key_capture;
pub mod state;
pub mod utils;
pub mod x11_utils;

pub use app_slint::run_manager;
