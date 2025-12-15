//! GUI module - egui-based management interface with system tray control

pub mod components;
mod key_capture;
mod manager;
pub mod state;
pub mod utils;

pub use manager::run_gui;
