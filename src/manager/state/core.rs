use std::process::Child;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};

use crate::common::constants::manager_ui::*;
use crate::common::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage};
use crate::config::DaemonConfig;
use crate::config::profile::Config;
use ipc_channel::ipc::{IpcReceiver, IpcSender};

use super::{DaemonStatus, StatusMessage};

/// Determines the behavior of `save_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// Explicitly requested save (e.g. "Save Thumbnail Positions").
    /// Saves EVERYTHING currently in memory, including window positions.
    Explicit,
    /// Implicit save (e.g. Exit, Settings Change).
    /// Saves settings but REVERTS window positions to their last saved state
    /// if "Auto-Save" is disabled for the profile.
    Implicit,
}

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
    pub(super) pending_position_save: bool,

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
    pub fn validate_config(&self) -> Result<()> {
        self.config
            .validate_profile_names()
            .map_err(|err| anyhow::anyhow!(err))
            .context("Configuration has invalid profile names")?;

        if let Some(profile) = self.config.get_active_profile() {
            profile
                .validate_custom_source_aliases()
                .map_err(|err| anyhow::anyhow!(err))
                .with_context(|| {
                    format!(
                        "Profile '{}' has invalid custom source aliases",
                        profile.profile_name
                    )
                })?;
        }

        Ok(())
    }

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
            pending_position_save: false,

            ipc_config_tx: None,
            ipc_status_rx: None,
            bootstrap_rx: None,
            daemon_status_rx: None,

            ipc_healthy: false,
            last_heartbeat: Instant::now(),
            missed_heartbeats: 0,
        }
    }

    /// Send the daemon's startup configuration snapshot.
    ///
    /// Success means the snapshot was transmitted over an active IPC command channel.
    pub fn send_initial_config_to_daemon(&self) -> Result<()> {
        self.validate_config()?;

        let tx = self
            .ipc_config_tx
            .as_ref()
            .context("Daemon IPC command channel is unavailable")?;
        let selected_profile = self
            .config
            .get_active_profile()
            .cloned()
            .unwrap_or_default();

        let mut character_thumbnails = selected_profile.character_thumbnails.clone();
        let mut custom_source_thumbnails = selected_profile.custom_source_thumbnails.clone();

        // If "Auto Save" is disabled, the startup snapshot must use the LAST SAVED state,
        // not the current transient in-memory state. This ensures that actions like "Refresh"
        // or "Profile Switch" revert to the saved positions as expected.
        if !selected_profile.thumbnail_auto_save_position
            && let Ok(disk_config) = crate::config::profile::Config::load()
            && let Some(disk_profile) = disk_config
                .profiles
                .iter()
                .find(|p| p.profile_name == selected_profile.profile_name)
        {
            info!("Auto-save disabled: using explicit disk positions for daemon startup");
            character_thumbnails = disk_profile.character_thumbnails.clone();
            custom_source_thumbnails = disk_profile.custom_source_thumbnails.clone();
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

        if let Err(e) = tx.send(ConfigMessage::InitialConfig(Box::new(daemon_config))) {
            error!(error = %e, "Failed to send initial config to daemon");
            return Err(anyhow::anyhow!(
                "Failed to send initial config to daemon: {}",
                e
            ));
        }

        debug!("Sent initial config to daemon");
        Ok(())
    }

    pub fn save_config(&mut self, mode: SaveMode) -> Result<()> {
        self.persist_config(mode)?;

        self.config_status_message = Some(StatusMessage {
            text: "Configuration saved successfully".to_string(),
            color: COLOR_SUCCESS,
        });
        info!("Configuration saved to disk");
        Ok(())
    }

    /// Persist Manager-owned configuration without sending a daemon command.
    /// Runtime application is handled by explicit daemon restart or narrow IPC deltas.
    pub(super) fn persist_config(&mut self, mode: SaveMode) -> Result<()> {
        self.validate_config()?;

        // Prepare config for saving
        // If mode is IMPLICIT (e.g. on exit or settings change),
        // we must ensure we don't accidentally persist transient window movements for profiles
        // that have "Auto Save Positions" disabled.
        let mut config_to_save = self.config.clone();

        if mode == SaveMode::Implicit {
            // Restore last explicitly saved positions from disk to prevent persistence of transient moves.
            if let Ok(disk_config) = crate::config::profile::Config::load() {
                for profile in config_to_save.profiles.iter_mut() {
                    if !profile.thumbnail_auto_save_position
                        && let Some(disk_profile) = disk_config
                            .profiles
                            .iter()
                            .find(|p| p.profile_name == profile.profile_name)
                    {
                        profile.character_thumbnails = disk_profile.character_thumbnails.clone();
                        profile.custom_source_thumbnails =
                            disk_profile.custom_source_thumbnails.clone();
                    }
                }
            } else {
                warn!("Failed to load disk config for position revert - saving current state");
            }
        }

        // Write current state to disk. The Manager applies structural changes by restarting
        // the daemon; live position acknowledgements use ConfigMessage::ThumbnailMoves.
        config_to_save.save()?;

        // Re-sync selected_profile_idx with the potentially reloaded profile list
        self.selected_profile_idx = self
            .config
            .profiles
            .iter()
            .position(|p| p.profile_name == self.config.global.selected_profile)
            .unwrap_or(0);

        self.settings_changed = false;
        self.pending_position_save = false;
        debug!("Configuration persisted without daemon synchronization");
        Ok(())
    }

    pub fn switch_profile(&mut self, idx: usize) -> bool {
        if let Err(err) = self.validate_config() {
            warn!(error = ?err, "Profile switch blocked by invalid configuration");
            self.status_message = Some(StatusMessage {
                text: format!("Profile switch blocked: {err}"),
                color: STATUS_STOPPED,
            });
            return false;
        }

        let Some(profile) = self.config.profiles.get(idx) else {
            return false;
        };
        if let Err(err) = profile
            .validate_custom_source_aliases()
            .map_err(|err| anyhow::anyhow!(err))
            .with_context(|| {
                format!(
                    "Profile '{}' has invalid custom source aliases",
                    profile.profile_name
                )
            })
        {
            warn!(error = ?err, "Profile switch blocked by invalid target profile");
            self.status_message = Some(StatusMessage {
                text: format!("Profile switch blocked: {err}"),
                color: STATUS_STOPPED,
            });
            return false;
        }

        let profile_name = profile.profile_name.clone();
        info!(profile_idx = idx, profile_name = %profile_name, "Profile switch requested");

        let previous_profile_name = self.config.global.selected_profile.clone();
        let previous_profile_idx = self.selected_profile_idx;
        self.config.global.selected_profile = profile_name;
        self.selected_profile_idx = idx;

        // Save config with new selection
        if let Err(err) = self.save_config(SaveMode::Implicit) {
            error!(error = ?err, "Failed to save config after profile switch");
            self.config.global.selected_profile = previous_profile_name;
            self.selected_profile_idx = previous_profile_idx;

            let rollback_error = self.persist_config(SaveMode::Implicit).err();
            if let Some(rollback_error) = &rollback_error {
                error!(error = ?rollback_error, "Failed to restore profile selection on disk");
            }
            self.status_message = Some(StatusMessage {
                text: if let Some(rollback_error) = rollback_error {
                    format!(
                        "Profile switch failed: {err}; failed to restore previous selection: {rollback_error}"
                    )
                } else {
                    format!("Profile switch failed: {err}")
                },
                color: STATUS_STOPPED,
            });
            false
        } else {
            // Reload daemon with new profile
            self.reload_daemon_config();
            true
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
        self.pending_position_save = false;
        self.config_status_message = Some(StatusMessage {
            text: "Changes discarded".to_string(),
            color: COLOR_ERROR,
        });
        info!("Configuration changes discarded");
    }

    pub fn save_thumbnail_positions(&mut self) -> Result<()> {
        self.save_config(SaveMode::Explicit)
            .context("Failed to save configuration")?;

        self.config_status_message = Some(StatusMessage {
            text: "Thumbnail positions saved".to_string(),
            color: STATUS_RUNNING,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{SaveMode, SharedState};
    use crate::common::ipc::ConfigMessage;
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
    fn invalid_profile_names_block_profile_switch() {
        let mut config = Config::default();
        config.profiles[0].profile_name = "Mining".to_string();
        config.profiles.push(Profile::default_with_name(
            "MINING".to_string(),
            String::new(),
        ));
        config.global.selected_profile = "Mining".to_string();
        let mut state = SharedState::new(config, false);

        assert!(!state.switch_profile(1));

        assert_eq!(state.selected_profile_idx, 0);
        assert_eq!(state.config.global.selected_profile, "Mining");
        assert!(
            state
                .status_message
                .as_ref()
                .is_some_and(|message| message.text.starts_with("Profile switch blocked:"))
        );
    }

    #[test]
    fn profile_name_switch_rejects_invalid_target_profile() {
        let mut config = Config::default();
        let mut target = Profile::default_with_name("Target".to_string(), String::new());
        for alias in ["Browser", " browser "] {
            target.custom_windows.push(
                serde_json::from_value(serde_json::json!({ "alias": alias }))
                    .expect("test custom source rule should deserialize"),
            );
        }
        config.profiles.push(target);
        let mut state = SharedState::new(config, false);

        assert!(!state.switch_profile(1));

        assert_eq!(state.selected_profile_idx, 0);
        assert_eq!(state.config.global.selected_profile, "default");
        assert!(
            state
                .status_message
                .as_ref()
                .is_some_and(|message| message.text.starts_with("Profile switch blocked:"))
        );
    }

    #[test]
    fn send_initial_config_to_daemon_emits_initial_config() {
        let config = Config::default();
        let mut state = SharedState::new(config, false);
        let (sender, receiver) = ipc_channel::ipc::channel().unwrap();
        state.ipc_config_tx = Some(sender);

        state.send_initial_config_to_daemon().unwrap();

        let ConfigMessage::InitialConfig(config) = receiver.recv().unwrap() else {
            panic!("expected startup path to send InitialConfig");
        };
        assert_eq!(config.profile.profile_name, "default");
    }

    #[test]
    fn send_initial_config_to_daemon_requires_command_channel() {
        let state = SharedState::new(Config::default(), false);

        let error = state.send_initial_config_to_daemon().unwrap_err();

        assert_eq!(
            error.to_string(),
            "Daemon IPC command channel is unavailable"
        );
    }

    #[test]
    fn save_config_is_disk_only_even_with_a_disconnected_daemon_channel() {
        let config = Config::default();
        let mut state = SharedState::new(config, false);
        let (sender, receiver) = ipc_channel::ipc::channel().unwrap();
        state.ipc_config_tx = Some(sender);

        state.save_config(SaveMode::Explicit).unwrap();
        assert!(receiver.try_recv().is_err());

        drop(receiver);
        state.save_config(SaveMode::Explicit).unwrap();
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
