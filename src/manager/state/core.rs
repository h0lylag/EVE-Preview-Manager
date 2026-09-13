use std::collections::HashSet;
use std::process::Child;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};

use crate::common::constants::manager_ui::*;
use crate::common::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage};
use crate::common::types::{Position, SourceIdentity};
use crate::config::DaemonConfig;
use crate::config::profile::Config;
use ipc_channel::ipc::{IpcReceiver, IpcSender};

use super::{DaemonStatus, StatusMessage};

/// Determines the behavior of `save_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// Explicitly requested full save ("Save & Apply").
    /// Saves EVERYTHING currently in memory, including window positions.
    Explicit,
    /// Implicit save (e.g. Exit, Settings Change).
    /// Saves settings but REVERTS window positions to their last saved state
    /// if "Auto-Save" is disabled for the profile.
    Implicit,
    /// Save only spatial fields for profiles with pending automatic position saves.
    AutoPositions,
    /// Save only spatial fields across all profiles, regardless of auto-save settings.
    Positions,
}

// Core application state shared between Manager and Tray
pub struct SharedState {
    pub config: Config,
    pub config_load_error: Option<String>,
    pub(super) config_path: std::path::PathBuf,
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
    pub(super) spatial_dirty_profiles: HashSet<String>,

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
    #[cfg(test)]
    pub(crate) fn at_path(config: Config, path: &std::path::Path) -> Self {
        let mut state = Self::new(config, false);
        state.config_path = path.to_path_buf();
        state
    }

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
            config_load_error: None,
            config_path: Config::path(),
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
            spatial_dirty_profiles: HashSet::new(),

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
        let mut selected_profile = self
            .config
            .get_active_profile()
            .cloned()
            .unwrap_or_default();

        // Restore saved geometry when auto-save is disabled, keeping current customizations
        // and identities. Refresh and profile switching must not revive stale settings.
        if !selected_profile.thumbnail_auto_save_position
            && let Ok(disk_config) = Config::read_from(&self.config_path)
            && let Some(disk_profile) = disk_config
                .profiles
                .iter()
                .find(|p| p.profile_name == selected_profile.profile_name)
        {
            info!("Auto-save disabled: using explicit disk positions for daemon startup");
            selected_profile.restore_saved_thumbnail_spatial(disk_profile);
        }

        let character_thumbnails = selected_profile.character_thumbnails.clone();
        let custom_source_thumbnails = selected_profile.custom_source_thumbnails.clone();

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

    pub fn has_unsaved_changes(&self) -> bool {
        self.settings_changed || !self.spatial_dirty_profiles.is_empty()
    }

    pub(super) fn has_automatic_position_changes(&self) -> bool {
        self.config.profiles.iter().any(|profile| {
            profile.thumbnail_auto_save_position
                && self.spatial_dirty_profiles.contains(&profile.profile_name)
        })
    }

    /// Persist Manager-owned configuration without sending a daemon command.
    /// Runtime application is handled by explicit daemon restart or narrow IPC deltas.
    pub(super) fn persist_config(&mut self, mode: SaveMode) -> Result<()> {
        if let Some(error) = &self.config_load_error {
            anyhow::bail!("{error}");
        }
        self.validate_config()?;
        let disk_config = self.read_config_for_reload()?;

        if matches!(mode, SaveMode::AutoPositions | SaveMode::Positions) {
            let mut config_to_save = disk_config;
            let mut saved_profiles = Vec::new();
            for profile in &self.config.profiles {
                if mode == SaveMode::AutoPositions
                    && (!profile.thumbnail_auto_save_position
                        || !self.spatial_dirty_profiles.contains(&profile.profile_name))
                {
                    continue;
                }
                let disk_profile = config_to_save
                    .profiles
                    .iter_mut()
                    .find(|saved| saved.profile_name == profile.profile_name)
                    .with_context(|| {
                        format!(
                            "Cannot save positions: profile '{}' is not saved",
                            profile.profile_name
                        )
                    })?;
                for (custom, thumbnails) in [
                    (false, &profile.character_thumbnails),
                    (true, &profile.custom_source_thumbnails),
                ] {
                    for (name, settings) in thumbnails {
                        if custom
                            && !disk_profile.custom_source_thumbnails.contains_key(name)
                            && !disk_profile
                                .custom_windows
                                .iter()
                                .any(|rule| rule.alias == *name)
                        {
                            anyhow::bail!(
                                "Cannot save positions: custom source '{name}' in profile '{}' is not saved",
                                profile.profile_name
                            );
                        }
                        let source = if custom {
                            SourceIdentity::custom(name)
                        } else {
                            SourceIdentity::eve(name)
                        };
                        disk_profile.update_thumbnail_spatial(
                            &source,
                            Position::new(settings.x, settings.y),
                            settings.dimensions,
                        );
                    }
                }
                saved_profiles.push(profile.profile_name.clone());
            }
            if !saved_profiles.is_empty() {
                config_to_save.save_to(&self.config_path)?;
            }
            for name in saved_profiles {
                self.spatial_dirty_profiles.remove(&name);
            }
            self.pending_position_save = self.has_automatic_position_changes();
            return Ok(());
        }

        // Prepare config for saving
        // If mode is IMPLICIT (e.g. on exit or settings change),
        // we must ensure we don't accidentally persist transient window movements for profiles
        // that have "Auto Save Positions" disabled.
        let mut config_to_save = self.config.clone();

        if mode == SaveMode::Implicit {
            // Restore last explicitly saved positions from disk to prevent persistence of transient moves.
            for profile in config_to_save.profiles.iter_mut() {
                if !profile.thumbnail_auto_save_position
                    && let Some(disk_profile) = disk_config
                        .profiles
                        .iter()
                        .find(|p| p.profile_name == profile.profile_name)
                {
                    profile.restore_saved_thumbnail_spatial(disk_profile);
                }
            }
        }

        // Write current state to disk. The Manager applies structural changes by restarting
        // the daemon; live position acknowledgements use ConfigMessage::ThumbnailMoves.
        config_to_save.save_to(&self.config_path)?;

        // Re-sync selected_profile_idx with the potentially reloaded profile list
        self.selected_profile_idx = self
            .config
            .profiles
            .iter()
            .position(|p| p.profile_name == self.config.global.selected_profile)
            .unwrap_or(0);

        self.settings_changed = false;
        self.pending_position_save = false;
        self.spatial_dirty_profiles.clear();
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

    fn read_config_for_reload(&mut self) -> Result<Config> {
        Config::read_from(&self.config_path).map_err(|error| {
            let message = format!(
                "Saving is blocked: {error:#}. Repair or restore the configuration, then use Discard Changes to reload it. Your in-memory edits have been kept."
            );
            self.config_load_error = Some(message.clone());
            error!(error = %message, "Failed to read configuration");
            error.context(message)
        })
    }

    pub fn discard_changes(&mut self) -> Result<()> {
        let config = self.read_config_for_reload()?;
        self.config = config;
        self.config_load_error = None;

        // Re-find selected profile index after reload
        self.selected_profile_idx = self
            .config
            .profiles
            .iter()
            .position(|p| p.profile_name == self.config.global.selected_profile)
            .unwrap_or(0);

        self.settings_changed = false;
        self.pending_position_save = false;
        self.spatial_dirty_profiles.clear();
        self.config_status_message = Some(StatusMessage {
            text: "Changes discarded".to_string(),
            color: COLOR_ERROR,
        });
        info!("Configuration changes discarded");
        Ok(())
    }

    pub fn save_thumbnail_positions(&mut self) -> Result<()> {
        self.persist_config(SaveMode::Positions)
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let config = Config::default();
        config.save_to(&path).unwrap();
        let mut state = SharedState::at_path(config, &path);
        let (sender, receiver) = ipc_channel::ipc::channel().unwrap();
        state.ipc_config_tx = Some(sender);

        state.save_config(SaveMode::Explicit).unwrap();
        assert!(receiver.try_recv().is_err());

        drop(receiver);
        state.save_config(SaveMode::Explicit).unwrap();
    }

    #[test]
    fn failed_discard_preserves_edits_and_blocks_saves_until_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = Config::default();
        config
            .profiles
            .push(Profile::default_with_name("Second".into(), String::new()));
        config.global.selected_profile = "Second".into();
        config.save_to(&path).unwrap();
        let mut state = SharedState::at_path(config.clone(), &path);
        state.config.global.window_width = 999;
        state.settings_changed = true;
        state.pending_position_save = true;
        state.spatial_dirty_profiles.insert("Second".into());
        let edited = serde_json::to_value(&state.config).unwrap();
        std::fs::write(&path, b"{broken").unwrap();

        assert!(state.discard_changes().is_err());
        assert_eq!(serde_json::to_value(&state.config).unwrap(), edited);
        assert_eq!(state.selected_profile_idx, 1);
        assert_eq!(state.spatial_dirty_profiles, ["Second".into()].into());
        assert!(state.settings_changed && state.pending_position_save);
        assert!(
            state
                .config_load_error
                .as_ref()
                .unwrap()
                .contains("Saving is blocked")
        );
        for mode in [
            SaveMode::Explicit,
            SaveMode::Implicit,
            SaveMode::AutoPositions,
            SaveMode::Positions,
        ] {
            assert!(state.save_config(mode).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
        }

        config.save_to(&path).unwrap();
        let repaired = std::fs::read(&path).unwrap();
        for mode in [
            SaveMode::Explicit,
            SaveMode::Implicit,
            SaveMode::AutoPositions,
            SaveMode::Positions,
        ] {
            assert!(state.save_config(mode).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), repaired);
        }
        state.discard_changes().unwrap();
        assert!(state.config_load_error.is_none());
        assert!(!state.settings_changed && !state.pending_position_save);
        assert!(state.spatial_dirty_profiles.is_empty());
        assert_eq!(
            serde_json::to_value(&state.config).unwrap(),
            serde_json::to_value(config).unwrap()
        );
        state.config.global.window_width = 777;
        state.save_config(SaveMode::Explicit).unwrap();
        assert_eq!(Config::read_from(&path).unwrap().global.window_width, 777);
    }

    #[test]
    fn saves_detect_unreadable_or_missing_config_before_writing() {
        for mode in [
            SaveMode::Explicit,
            SaveMode::Implicit,
            SaveMode::AutoPositions,
            SaveMode::Positions,
        ] {
            for missing in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("config.json");
                if !missing {
                    std::fs::write(&path, b"{broken").unwrap();
                }
                let mut state = SharedState::at_path(Config::default(), &path);
                state.settings_changed = true;
                state.pending_position_save = true;
                assert!(state.save_config(mode).is_err());
                assert!(state.config_load_error.is_some());
                assert!(state.settings_changed && state.pending_position_save);
                if missing {
                    assert!(!path.exists());
                } else {
                    assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
                }
            }
        }
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

    #[test]
    fn position_merge_preserves_saved_settings_and_source_identities() {
        use crate::common::types::{CharacterSettings, Dimensions};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = Config::default();
        let profile = &mut config.profiles[0];
        profile.thumbnail_auto_save_position = true;
        profile
            .custom_windows
            .push(serde_json::from_value(serde_json::json!({"alias": "New Custom"})).unwrap());
        let saved = CharacterSettings {
            alias: Some("Saved alias".into()),
            notes: Some("Saved notes".into()),
            exempt_from_minimize: true,
            ..CharacterSettings::new(1, 2, 30, 40)
        };
        profile
            .character_thumbnails
            .insert("Shared".into(), saved.clone());
        profile
            .custom_source_thumbnails
            .insert("Shared".into(), saved.clone());
        config.save_to(&path).unwrap();
        let mut state = SharedState::at_path(config.clone(), &path);
        state.config.global.window_width += 100;
        let profile = &mut state.config.profiles[0];
        profile.thumbnail_opacity = 42;
        let draft = CharacterSettings {
            alias: Some("Draft alias".into()),
            notes: Some("Draft notes".into()),
            exempt_from_minimize: false,
            ..CharacterSettings::new(10, 20, 300, 400)
        };
        profile
            .character_thumbnails
            .insert("Shared".into(), draft.clone());
        profile.custom_source_thumbnails.insert(
            "Shared".into(),
            CharacterSettings {
                x: 50,
                ..draft.clone()
            },
        );
        profile
            .character_thumbnails
            .insert("New EVE".into(), draft.clone());
        profile
            .custom_source_thumbnails
            .insert("New Custom".into(), draft);
        state.settings_changed = true;
        let memory = serde_json::to_value(&state.config).unwrap();
        state.save_thumbnail_positions().unwrap();

        let expected_profile = &mut config.profiles[0];
        let existing = expected_profile
            .character_thumbnails
            .get_mut("Shared")
            .unwrap();
        existing.x = 10;
        existing.y = 20;
        existing.dimensions = Dimensions::new(300, 400);
        let existing = expected_profile
            .custom_source_thumbnails
            .get_mut("Shared")
            .unwrap();
        existing.x = 50;
        existing.y = 20;
        existing.dimensions = Dimensions::new(300, 400);
        expected_profile
            .character_thumbnails
            .insert("New EVE".into(), CharacterSettings::new(10, 20, 300, 400));
        expected_profile.custom_source_thumbnails.insert(
            "New Custom".into(),
            CharacterSettings::new(10, 20, 300, 400),
        );
        assert_eq!(
            serde_json::to_value(Config::read_from(&path).unwrap()).unwrap(),
            serde_json::to_value(config).unwrap()
        );
        assert_eq!(serde_json::to_value(&state.config).unwrap(), memory);
        assert!(state.settings_changed);
    }

    #[test]
    fn automatic_position_saves_only_clear_eligible_profiles() {
        use crate::common::types::CharacterSettings;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = Config {
            profiles: ["First", "Second", "Third"]
                .into_iter()
                .map(|name| {
                    let mut profile = Profile::default_with_name(name.into(), String::new());
                    profile.thumbnail_auto_save_position = name != "Second";
                    profile
                })
                .collect(),
            ..Config::default()
        };
        config.global.selected_profile = "First".into();
        config.save_to(&path).unwrap();
        let mut state = SharedState::at_path(config, &path);
        state.config.global.selected_profile = "Second".into();
        state.selected_profile_idx = 1;
        for profile in &mut state.config.profiles {
            profile
                .character_thumbnails
                .insert("Character".into(), CharacterSettings::new(10, 20, 30, 40));
        }
        state
            .spatial_dirty_profiles
            .extend(["First".into(), "Second".into()]);
        state.pending_position_save = true;
        state.persist_config(SaveMode::AutoPositions).unwrap();
        let disk = Config::read_from(&path).unwrap();
        assert_eq!(disk.global.selected_profile, "First");
        assert_eq!(state.selected_profile_idx, 1);
        assert_eq!(disk.profiles[0].character_thumbnails["Character"].x, 10);
        assert!(disk.profiles[1].character_thumbnails.is_empty());
        assert!(disk.profiles[2].character_thumbnails.is_empty());
        assert_eq!(state.spatial_dirty_profiles, ["Second".into()].into());
        assert!(!state.pending_position_save);
        assert!(state.has_unsaved_changes());
        state.save_thumbnail_positions().unwrap();
        let disk = Config::read_from(&path).unwrap();
        assert!(
            disk.profiles
                .iter()
                .all(|p| p.character_thumbnails["Character"].x == 10)
        );
        assert_eq!(disk.global.selected_profile, "First");
        assert!(!state.has_unsaved_changes());
    }

    #[test]
    fn missing_position_targets_abort_the_entire_save() {
        use crate::common::types::CharacterSettings;
        for mode in [SaveMode::AutoPositions, SaveMode::Positions] {
            for missing_profile in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("config.json");
                let mut config = Config::default();
                config.profiles[0].thumbnail_auto_save_position = true;
                config.save_to(&path).unwrap();
                let original = std::fs::read(&path).unwrap();
                let mut state = SharedState::at_path(config, &path);
                if missing_profile {
                    let mut profile = Profile::default_with_name("Unsaved".into(), String::new());
                    profile.thumbnail_auto_save_position = true;
                    state.config.profiles.push(profile);
                } else {
                    state.config.profiles[0].custom_windows.push(
                        serde_json::from_value(serde_json::json!({"alias": "Unsaved"})).unwrap(),
                    );
                    state.config.profiles[0]
                        .custom_source_thumbnails
                        .insert("Unsaved".into(), CharacterSettings::new(1, 2, 3, 4));
                }
                for profile in &mut state.config.profiles {
                    profile
                        .character_thumbnails
                        .insert("Valid".into(), CharacterSettings::new(10, 20, 30, 40));
                    state
                        .spatial_dirty_profiles
                        .insert(profile.profile_name.clone());
                }
                state.settings_changed = true;
                state.pending_position_save = true;
                let dirty = state.spatial_dirty_profiles.clone();
                assert!(state.persist_config(mode).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert_eq!(state.spatial_dirty_profiles, dirty);
                assert!(state.settings_changed && state.pending_position_save);
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn write_failure_retains_work_for_retry() {
        use crate::common::types::CharacterSettings;
        use std::os::fd::AsRawFd;
        for mode in [
            SaveMode::AutoPositions,
            SaveMode::Positions,
            SaveMode::Implicit,
            SaveMode::Explicit,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let mut config = Config::default();
            config.profiles[0].thumbnail_auto_save_position = true;
            config.save_to(&path).unwrap();
            let original = std::fs::read(&path).unwrap();
            let file = std::fs::File::open(&path).unwrap();
            // procfs permits reading this descriptor but cannot host an atomic-write tempfile.
            let readonly_path =
                std::path::PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
            let mut state = SharedState::at_path(config, &readonly_path);
            state.config.profiles[0]
                .character_thumbnails
                .insert("Character".into(), CharacterSettings::new(10, 20, 30, 40));
            state
                .spatial_dirty_profiles
                .insert(state.config.global.selected_profile.clone());
            state.pending_position_save = true;
            state.settings_changed = true;
            state.config.profiles[0]
                .character_thumbnails
                .get_mut("Character")
                .unwrap()
                .alias = Some("Main".into());
            let memory = serde_json::to_value(&state.config).unwrap();
            assert!(state.persist_config(mode).is_err());
            assert!(state.config_load_error.is_none());
            assert_eq!(serde_json::to_value(&state.config).unwrap(), memory);
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(state.settings_changed && state.pending_position_save);
            assert!(!state.spatial_dirty_profiles.is_empty());
            state.config_path = path.clone();
            state.persist_config(mode).unwrap();
            assert_eq!(
                Config::read_from(&path).unwrap().profiles[0].character_thumbnails["Character"].x,
                10
            );
            assert_eq!(
                state.settings_changed,
                matches!(mode, SaveMode::AutoPositions | SaveMode::Positions)
            );
            assert!(!state.pending_position_save);
            assert!(state.spatial_dirty_profiles.is_empty());
        }
    }

    // Return disk, edited, and expected implicit-save configurations. The expected value
    // is built directly so the regression does not share the production merge helper.
    fn customization_configs() -> (Config, Config, Config) {
        use crate::common::types::{CharacterSettings, Dimensions, PreviewMode};
        use crate::config::profile::{CycleGroup, CycleSlot};
        let mut saved = Config::default();
        let profile = &mut saved.profiles[0];
        profile.thumbnail_auto_save_position = false;
        profile
            .character_thumbnails
            .insert("Alice".into(), CharacterSettings::new(10, 20, 300, 200));
        profile
            .custom_source_thumbnails
            .insert("Alice".into(), CharacterSettings::new(30, 40, 500, 400));
        profile
            .character_thumbnails
            .insert("Deleted".into(), CharacterSettings::new(1, 2, 3, 4));
        profile
            .custom_source_thumbnails
            .insert("Deleted".into(), CharacterSettings::new(1, 2, 3, 4));
        profile
            .custom_source_thumbnails
            .insert("Old".into(), CharacterSettings::new(5, 6, 7, 8));
        profile.custom_windows = ["Alice", "Old"]
            .into_iter()
            .map(|alias| serde_json::from_value(serde_json::json!({"alias": alias})).unwrap())
            .collect();
        profile.cycle_groups = vec![CycleGroup {
            cycle_list: vec![
                CycleSlot::Source("Old".into()),
                CycleSlot::Eve("Alice".into()),
            ],
            ..CycleGroup::default_group()
        }];
        let mut enabled = profile.clone();
        enabled.profile_name = "Enabled".into();
        enabled.thumbnail_auto_save_position = true;
        saved.profiles.push(enabled);

        let draft_settings = CharacterSettings {
            x: 101,
            y: 202,
            dimensions: Dimensions::new(600, 450),
            alias: Some("Main".into()),
            notes: Some("Keep these notes".into()),
            override_active_border_color: Some("#112233".into()),
            override_inactive_border_color: Some("#445566".into()),
            override_active_border_size: Some(7),
            override_inactive_border_size: Some(3),
            override_text_color: Some("#778899".into()),
            preview_mode: PreviewMode::Static {
                color: "#ABCDEF".into(),
            },
            exempt_from_minimize: true,
            override_render_preview: Some(false),
        };
        let mut edited = saved.clone();
        for profile in &mut edited.profiles {
            profile
                .character_thumbnails
                .insert("Alice".into(), draft_settings.clone());
            profile
                .custom_source_thumbnails
                .insert("Alice".into(), draft_settings.clone());
            profile.character_thumbnails.remove("Deleted");
            profile.custom_source_thumbnails.remove("Deleted");
            // Exact keys: this new identity must not match the saved "Alice".
            profile
                .character_thumbnails
                .insert("alice".into(), draft_settings.clone());
            profile
                .custom_source_thumbnails
                .insert("Old".into(), draft_settings.clone());
            profile.rename_custom_source_alias(1, "Renamed").unwrap();
        }
        let mut new_profile = edited.profiles[0].clone();
        new_profile.profile_name = "New Profile".into();
        edited.profiles.push(new_profile);
        edited.profiles.swap(0, 1);
        let mut expected = edited.clone();
        let profile = &mut expected.profiles[1];
        let character = profile.character_thumbnails.get_mut("Alice").unwrap();
        character.x = 10;
        character.y = 20;
        character.dimensions = Dimensions::new(300, 200);
        let custom = profile.custom_source_thumbnails.get_mut("Alice").unwrap();
        custom.x = 30;
        custom.y = 40;
        custom.dimensions = Dimensions::new(500, 400);
        (saved, edited, expected)
    }

    #[test]
    fn implicit_save_preserves_customizations_and_current_identities() {
        for mode in [SaveMode::Implicit, SaveMode::Explicit] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let (saved, edited, expected) = customization_configs();
            saved.save_to(&path).unwrap();
            let mut state = SharedState::at_path(edited.clone(), &path);
            state.settings_changed = true;
            state
                .spatial_dirty_profiles
                .insert(edited.global.selected_profile.clone());
            state.pending_position_save = true;
            state.save_config(mode).unwrap();
            let disk = Config::read_from(&path).unwrap();
            assert_eq!(
                disk.get_active_profile().unwrap().character_thumbnails["Alice"]
                    .alias
                    .as_deref(),
                Some("Main")
            );
            let expected = if mode == SaveMode::Implicit {
                expected
            } else {
                edited.clone()
            };
            assert_eq!(
                serde_json::to_value(&disk).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            assert_eq!(
                serde_json::to_value(&state.config).unwrap(),
                serde_json::to_value(&edited).unwrap()
            );
            assert!(!state.has_unsaved_changes());
            assert!(!state.pending_position_save);
            state.discard_changes().unwrap();
            assert_eq!(
                serde_json::to_value(&state.config).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
        }
    }

    #[test]
    fn startup_snapshot_preserves_customizations_with_consistent_saved_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let (saved, edited, expected) = customization_configs();
        saved.save_to(&path).unwrap();
        let disk_bytes = std::fs::read(&path).unwrap();
        let mut state = SharedState::at_path(edited, &path);
        let (tx, rx) = ipc_channel::ipc::channel().unwrap();
        state.ipc_config_tx = Some(tx);
        for profile in &expected.profiles {
            state.config.global.selected_profile = profile.profile_name.clone();
            let memory = serde_json::to_value(&state.config).unwrap();
            state.send_initial_config_to_daemon().unwrap();
            let ConfigMessage::InitialConfig(snapshot) = rx.recv().unwrap() else {
                panic!("expected initial configuration");
            };
            assert_eq!(
                serde_json::to_value(&snapshot.profile).unwrap(),
                serde_json::to_value(profile).unwrap()
            );
            assert_eq!(snapshot.character_thumbnails, profile.character_thumbnails);
            assert_eq!(
                snapshot.custom_source_thumbnails,
                profile.custom_source_thumbnails
            );
            assert_eq!(serde_json::to_value(&state.config).unwrap(), memory);
        }
        assert_eq!(std::fs::read(&path).unwrap(), disk_bytes);
    }

    #[test]
    fn startup_snapshot_keeps_current_settings_when_disk_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let (_, edited, _) = customization_configs();
        std::fs::write(&path, b"{broken").unwrap();
        let mut state = SharedState::at_path(edited.clone(), &path);
        let (tx, rx) = ipc_channel::ipc::channel().unwrap();
        state.ipc_config_tx = Some(tx);
        state.send_initial_config_to_daemon().unwrap();
        let ConfigMessage::InitialConfig(snapshot) = rx.recv().unwrap() else {
            panic!("expected initial configuration");
        };
        let profile = edited.get_active_profile().unwrap();
        assert_eq!(
            serde_json::to_value(&snapshot.profile).unwrap(),
            serde_json::to_value(profile).unwrap()
        );
        assert_eq!(snapshot.character_thumbnails, profile.character_thumbnails);
        assert_eq!(
            snapshot.custom_source_thumbnails,
            profile.custom_source_thumbnails
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
    }
}
