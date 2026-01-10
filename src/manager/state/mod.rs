use std::process::Child;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::common::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage};
use crate::config::profile::Config;
use ipc_channel::ipc::{IpcReceiver, IpcSender};

pub mod config;
pub mod daemon;
pub mod types;

pub use types::*;

// Core application state shared between GUI and Tray
pub struct SharedState {
    pub config: Config,
    pub daemon: Option<Child>,
    pub daemon_status: DaemonStatus,
    pub last_health_check: Instant,
    pub status_message: Option<StatusMessage>,
    pub config_status_message: Option<StatusMessage>,
    pub settings_changed: bool,
    pub selected_profile_idx: usize,
    pub should_quit: bool,
    pub last_save_attempt: Instant,

    // IPC
    pub ipc_config_tx: Option<IpcSender<ConfigMessage>>,
    pub ipc_status_rx: Option<IpcReceiver<DaemonMessage>>,
    pub bootstrap_rx: Option<Receiver<BootstrapMessage>>,
    pub daemon_status_rx: Option<Receiver<DaemonMessage>>,
}

impl SharedState {
    pub fn new(config: Config) -> Self {
        let selected_profile_idx = config
            .profiles
            .iter()
            .position(|p| p.profile_name == config.global.selected_profile)
            .unwrap_or(0);

        Self {
            config,
            daemon: None,
            daemon_status: DaemonStatus::Stopped,
            last_health_check: Instant::now(),
            status_message: None,
            config_status_message: None,
            settings_changed: false,
            selected_profile_idx,
            should_quit: false,
            last_save_attempt: Instant::now(),

            ipc_config_tx: None,
            ipc_status_rx: None,
            bootstrap_rx: None,
            daemon_status_rx: None,
        }
    }
}
