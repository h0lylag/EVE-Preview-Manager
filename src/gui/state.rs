use std::process::Child;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use eframe::egui;
use tracing::{error, info, warn};

use crate::config::profile::{Config, SaveStrategy};
use crate::constants::gui::*;
use crate::gui::utils::spawn_preview_daemon;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum GuiTab {
    Behavior,
    Appearance,
    Hotkeys,
    Characters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStatus {
    Starting,
    Running,
    Stopped,
    Crashed(Option<i32>),
}

impl DaemonStatus {
    pub fn color(&self) -> egui::Color32 {
        match self {
            DaemonStatus::Running => STATUS_RUNNING,
            DaemonStatus::Starting => STATUS_STARTING,
            _ => STATUS_STOPPED,
        }
    }

    pub fn label(&self) -> String {
        match self {
            DaemonStatus::Running => "Preview daemon running".to_string(),
            DaemonStatus::Starting => "Preview daemon starting...".to_string(),
            DaemonStatus::Stopped => "Preview daemon stopped".to_string(),
            DaemonStatus::Crashed(code) => match code {
                Some(code) => format!("Preview daemon crashed (exit {code})"),
                None => "Preview daemon crashed".to_string(),
            },
        }
    }
}

pub struct StatusMessage {
    pub text: String,
    pub color: egui::Color32,
}

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
    pub last_config_mtime: Option<std::time::SystemTime>,
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
            last_config_mtime: std::fs::metadata(Config::path())
                .ok()
                .and_then(|m| m.modified().ok()),
        }
    }

    pub fn start_daemon(&mut self) -> Result<()> {
        if self.daemon.is_some() {
            return Ok(());
        }

        let child = spawn_preview_daemon()?;
        let pid = child.id();
        info!(pid, "Started preview daemon");

        self.daemon = Some(child);
        self.daemon_status = DaemonStatus::Starting;
        Ok(())
    }

    pub fn stop_daemon(&mut self) -> Result<()> {
        if let Some(mut child) = self.daemon.take() {
            info!(pid = child.id(), "Stopping preview daemon");
            let _ = child.kill();
            let status = child
                .wait()
                .context("Failed to wait for preview daemon exit")?;
            self.daemon_status = if status.success() {
                DaemonStatus::Stopped
            } else {
                DaemonStatus::Crashed(status.code())
            };
        }
        Ok(())
    }

    pub fn restart_daemon(&mut self) {
        info!("Restart requested");
        if let Err(err) = self.stop_daemon().and_then(|_| self.start_daemon()) {
            error!(error = ?err, "Failed to restart daemon");
            self.status_message = Some(StatusMessage {
                text: format!("Restart failed: {err}"),
                color: STATUS_STOPPED,
            });
        }
    }

    pub fn reload_daemon_config(&mut self) {
        info!("Config reload requested - restarting daemon");
        self.restart_daemon();
    }

    pub fn save_config(&mut self) -> Result<()> {
        // Load fresh config from disk (has all characters including daemon's additions)
        let disk_config = Config::load().unwrap_or_else(|_| self.config.clone());

        // Merge strategy: Start with GUI's profile list (handles deletions), merge character positions from disk
        let mut merged_profiles = Vec::new();

        for gui_profile in &self.config.profiles {
            let mut merged_profile = gui_profile.clone();

            // Find matching profile in disk config to get daemon's character positions
            if let Some(disk_profile) = disk_config
                .profiles
                .iter()
                .find(|p| p.profile_name == gui_profile.profile_name)
            {
                // Merge character positions: start with GUI's, add disk characters, preserve disk positions
                for (char_name, disk_settings) in &disk_profile.character_thumbnails {
                    if let Some(gui_settings) =
                        merged_profile.character_thumbnails.get_mut(char_name)
                    {
                        // Character exists in both: keep GUI dimensions, use disk position (x, y)
                        gui_settings.x = disk_settings.x;
                        gui_settings.y = disk_settings.y;
                        gui_settings.dimensions = disk_settings.dimensions;
                    } else if !char_name.is_empty() {
                        // Character only in disk (daemon added it): preserve it completely
                        merged_profile
                            .character_thumbnails
                            .insert(char_name.clone(), disk_settings.clone());
                    }
                }
            }

            merged_profiles.push(merged_profile);
        }

        // Build final config with merged profiles and GUI's global settings
        let final_config = Config {
            profiles: merged_profiles,
            global: self.config.global.clone(),
        };

        // Save the merged config
        // Default: preserve character positions from disk (daemon's source of truth)
        final_config
            .save_with_strategy(SaveStrategy::Preserve)
            .context("Failed to save configuration")?;

        // Update in-memory config immediately (no need to reload from disk)
        self.config = final_config;

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

    pub fn reload_character_list(&mut self) {
        // Load fresh config from disk to get daemon's new characters
        if let Ok(disk_config) = Config::load() {
            // Merge new characters from disk into GUI config without losing GUI changes
            for (profile_idx, gui_profile) in self.config.profiles.iter_mut().enumerate() {
                if let Some(disk_profile) = disk_config.profiles.get(profile_idx)
                    && disk_profile.profile_name == gui_profile.profile_name
                {
                    // Add any new characters from disk that GUI doesn't know about
                    for (char_name, char_settings) in &disk_profile.character_thumbnails {
                        if !gui_profile.character_thumbnails.contains_key(char_name)
                            && !char_name.is_empty()
                        {
                            gui_profile
                                .character_thumbnails
                                .insert(char_name.clone(), char_settings.clone());
                            info!(character = %char_name, profile = %gui_profile.profile_name, "Detected new character from daemon");
                        }
                    }
                }
            }
        }
    }

    pub fn save_thumbnail_positions(&mut self) -> Result<()> {
        // If we have a running daemon, send SIGUSR1 signal to trigger save
        if let Some(ref daemon) = self.daemon {
            let pid = daemon.id();
            #[cfg(target_os = "linux")]
            {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;

                signal::kill(Pid::from_raw(pid as i32), Signal::SIGUSR1)
                    .context("Failed to send SIGUSR1 to daemon")?;

                self.status_message = Some(StatusMessage {
                    text: "Thumbnail positions saved".to_string(),
                    color: STATUS_RUNNING,
                });
            }
            #[cfg(not(target_os = "linux"))]
            {
                anyhow::bail!("Signal-based save only supported on Linux");
            }
            Ok(())
        } else {
            anyhow::bail!("Cannot save positions: daemon is not running")
        }
    }

    pub fn poll_daemon(&mut self) {
        if self.last_health_check.elapsed() < Duration::from_millis(DAEMON_CHECK_INTERVAL_MS) {
            return;
        }
        self.last_health_check = Instant::now();

        // NOTE: Efficient file watching for Immediate Mode GUI
        // We poll the file modification time every 500ms (synced with daemon health check).
        // This avoids race conditions by treating the file system as the synchronization source,
        // and is cheap enough to run in the update loop without blocking the UI.
        let config_path = Config::path();
        if let Ok(metadata) = std::fs::metadata(&config_path)
            && let Ok(mtime) = metadata.modified()
            && self.last_config_mtime.is_none_or(|last| mtime > last)
        {
            info!("Config file modified externally, reloading character list");
            self.reload_character_list();
            self.last_config_mtime = Some(mtime);
        }

        if let Some(child) = self.daemon.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(pid = child.id(), exit = ?status.code(), "Preview daemon exited");
                    self.daemon = None;
                    self.daemon_status = if status.success() {
                        DaemonStatus::Stopped
                    } else {
                        DaemonStatus::Crashed(status.code())
                    };
                }
                Ok(None) => {
                    if matches!(self.daemon_status, DaemonStatus::Starting) {
                        self.daemon_status = DaemonStatus::Running;
                        self.reload_character_list();
                    }
                }
                Err(err) => {
                    error!(error = ?err, "Failed to query daemon status");
                }
            }
        }
    }
}
