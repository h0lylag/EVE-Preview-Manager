use anyhow::{Context, Result};
use tracing::{error, info};

use crate::common::constants::manager_ui::*;
use crate::common::ipc::ConfigMessage;
use crate::config::DaemonConfig;
use crate::config::profile::Config;

use super::SharedState;
use super::StatusMessage;

impl SharedState {
    pub fn save_config(&mut self) -> Result<()> {
        // Write current state to disk - Manager maintains authoritative state via IPC synchronization
        self.config.save()?;

        // Sync with daemon via IPC
        if let Some(ref tx) = self.ipc_config_tx {
            let selected_profile = self
                .config
                .get_active_profile()
                .cloned()
                .unwrap_or_default();

            let mut character_thumbnails = selected_profile.character_thumbnails.clone();
            let mut custom_source_thumbnails = std::collections::HashMap::new();

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
            } else {
                info!("Sent config update to daemon");
            }
        }

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
        info!(profile_idx = idx, "Profile switch requested");

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
