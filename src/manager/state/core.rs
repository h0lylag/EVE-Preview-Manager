use std::process::Child;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{debug, error, info};

use crate::common::constants::manager_ui::*;
use crate::common::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage};
use crate::config::DaemonConfig;
use crate::config::profile::Config;
use ipc_channel::ipc::{IpcReceiver, IpcSender};

use super::{DaemonStatus, StatusMessage};

// Core application state shared between Manager and Tray
pub struct SharedState {
    pub config: Config,
    pub debug_mode: bool,
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

    // IPC health monitoring
    pub ipc_healthy: bool,
    pub last_heartbeat: Instant,
    pub missed_heartbeats: u32,
}

impl SharedState {
    pub fn new(config: Config, debug_mode: bool) -> Self {
        let selected_profile_idx = config
            .profiles
            .iter()
            .position(|p| p.profile_name == config.global.selected_profile)
            .unwrap_or(0);

        Self {
            config,
            debug_mode,
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

            ipc_healthy: false,
            last_heartbeat: Instant::now(),
            missed_heartbeats: 0,
        }
    }

    pub fn sync_to_daemon(&self) -> Result<()> {
        if let Some(ref tx) = self.ipc_config_tx {
            let selected_profile = self
                .config
                .get_active_profile()
                .cloned()
                .unwrap_or_default();

            let mut character_thumbnails = selected_profile.character_thumbnails.clone();
            let mut custom_source_thumbnails = selected_profile.custom_source_thumbnails.clone();

            // Filter based on custom rules in profile.
            let rules = &selected_profile.custom_windows;
            let mut move_keys = Vec::new();
            for key in character_thumbnails.keys() {
                if rules.iter().any(|r| r.alias == *key) {
                    move_keys.push(key.clone());
                }
            }

            for key in move_keys {
                if let Some(val) = character_thumbnails.remove(&key) {
                    custom_source_thumbnails.insert(key, val);
                }
            }

            // Build hotkeys for profile switching (requires looking at all profiles)
            let mut profile_hotkeys = std::collections::HashMap::new();
            for profile in &self.config.profiles {
                if let Some(ref binding) = profile.hotkey_profile_switch {
                    profile_hotkeys.insert(binding.clone(), profile.profile_name.clone());
                }
            }

            let daemon_config = DaemonConfig {
                profile: selected_profile,
                character_thumbnails,
                custom_source_thumbnails,
                profile_hotkeys,
                runtime_hidden: false,
            };

            if let Err(e) = tx.send(ConfigMessage::Update(daemon_config)) {
                error!(error = %e, "Failed to send config update to daemon");
                return Err(anyhow::anyhow!("Failed to send config to daemon: {}", e));
            } else {
                debug!("Sent config update to daemon");
            }
        }
        Ok(())
    }

    pub fn save_config(&mut self) -> Result<()> {
        // Write current state to disk - Manager maintains authoritative state via IPC synchronization
        self.config.save()?;

        // Sync with daemon via IPC
        self.sync_to_daemon()?;

        // Re-sync selected_profile_idx with the potentially reloaded profile list
        self.selected_profile_idx = self
            .config
            .profiles
            .iter()
            .position(|p| p.profile_name == self.config.global.selected_profile)
            .unwrap_or(0);

        self.settings_changed = false;
        self.config_status_message = Some(StatusMessage {
            text: "Configuration saved successfully".to_string(),
            color: COLOR_SUCCESS,
        });
        info!("Configuration saved to disk");
        Ok(())
    }

    pub fn switch_profile(&mut self, idx: usize) {
        let profile_name = self
            .config
            .profiles
            .get(idx)
            .map(|p| p.profile_name.as_str())
            .unwrap_or("Unknown");
        info!(profile_idx = idx, profile_name = %profile_name, "Profile switch requested");

        if idx < self.config.profiles.len() {
            self.config.global.selected_profile = self.config.profiles[idx].profile_name.clone();
            self.selected_profile_idx = idx;

            // Save config with new selection
            if let Err(err) = self.save_config() {
                error!(error = ?err, "Failed to save config after profile switch");
                self.status_message = Some(StatusMessage {
                    text: format!("Profile switch failed: {err}"),
                    color: STATUS_STOPPED,
                });
            } else {
                // Reload daemon with new profile
                self.reload_daemon_config();
            }
        }
    }

    pub fn discard_changes(&mut self) {
        self.config = Config::load().unwrap_or_default();

        // Re-find selected profile index after reload
        self.selected_profile_idx = self
            .config
            .profiles
            .iter()
            .position(|p| p.profile_name == self.config.global.selected_profile)
            .unwrap_or(0);

        self.settings_changed = false;
        self.config_status_message = Some(StatusMessage {
            text: "Changes discarded".to_string(),
            color: COLOR_ERROR,
        });
        info!("Configuration changes discarded");
    }

    pub fn save_thumbnail_positions(&mut self) -> Result<()> {
        self.save_config().context("Failed to save configuration")?;

        self.status_message = Some(StatusMessage {
            text: "Thumbnail positions saved".to_string(),
            color: STATUS_RUNNING,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::SharedState;
    use crate::config::profile::{Config, Profile};

    #[test]
    fn test_shared_state_initialization() {
        // Use default config
        let config = Config::default();
        let state = SharedState::new(config.clone(), false);

        // Verify default health state
        assert!(!state.ipc_healthy);
        assert_eq!(state.missed_heartbeats, 0);
        assert_eq!(state.selected_profile_idx, 0);
        assert!(state.daemon.is_none());
        assert!(!state.settings_changed);
    }

    #[test]
    fn test_shared_state_profile_selection() {
        let mut config = Config::default();
        // Add a second profile
        config.profiles.push(Profile::default_with_name(
            "Second".to_string(),
            "Desc".to_string(),
        ));

        // Select the second profile
        config.global.selected_profile = "Second".to_string();

        let state = SharedState::new(config, false);

        // Should find index 1
        assert_eq!(state.selected_profile_idx, 1);
    }

    #[test]
    fn test_heartbeat_processing() {
        use crate::common::ipc::DaemonMessage;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let config = Config::default();
        let mut state = SharedState::new(config, false);

        // Simulate a state where we haven't heard from daemon in a while
        state.ipc_healthy = false;
        state.missed_heartbeats = 5;
        state.last_heartbeat = Instant::now() - Duration::from_secs(20);

        // Inject a channel to simulate daemon messages
        let (tx, rx) = mpsc::channel();
        state.daemon_status_rx = Some(rx);

        // Send a heartbeat
        tx.send(DaemonMessage::Heartbeat).unwrap();

        // Process messages
        state.poll_daemon();

        // Verify state reset
        assert!(
            state.ipc_healthy,
            "Heartbeat should set ipc_healthy to true"
        );
        assert_eq!(
            state.missed_heartbeats, 0,
            "Heartbeat should reset missed count"
        );
        assert!(
            state.last_heartbeat.elapsed() < Duration::from_secs(1),
            "Heartbeat should update timestamp"
        );
    }
}
