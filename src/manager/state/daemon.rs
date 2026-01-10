use anyhow::{Context, Result};
use ipc_channel::ipc::IpcOneShotServer;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use crate::common::constants::gui::*;
use crate::common::ipc::{BootstrapMessage, DaemonMessage};
use crate::config::profile::SaveStrategy;
use crate::manager::utils::spawn_daemon;

use super::DaemonStatus;
use super::SharedState;

impl SharedState {
    pub fn start_daemon(&mut self) -> Result<()> {
        if self.daemon.is_some() {
            return Ok(());
        }

        // 1. Create IPC OneShot Server
        let (server, server_name) =
            IpcOneShotServer::<BootstrapMessage>::new().context("Failed to create IPC server")?;

        // 2. Spawn Daemon with server name
        let child = spawn_daemon(&server_name)?;
        let pid = child.id();
        info!(pid, server_name = %server_name, "Started daemon process");

        // 3. Spawn thread to wait for connection (avoid blocking GUI)
        let (tx, rx) = mpsc::channel();
        self.bootstrap_rx = Some(rx);

        std::thread::spawn(move || {
            info!("Waiting for daemon IPC connection...");
            match server.accept() {
                Ok((_, bootstrap_msg)) => {
                    info!("Daemon connected via IPC");
                    let _ = tx.send(bootstrap_msg);
                }
                Err(e) => {
                    error!(error = %e, "Failed to accept IPC connection");
                }
            }
        });

        self.daemon = Some(child);
        self.daemon_status = DaemonStatus::Starting;
        Ok(())
    }

    pub fn stop_daemon(&mut self) -> Result<()> {
        if let Some(mut child) = self.daemon.take() {
            info!(pid = child.id(), "Stopping daemon process");

            if let Err(e) = child.kill() {
                error!(pid = child.id(), error = %e, "Failed to send SIGKILL to daemon");
            } else {
                debug!(pid = child.id(), "SIGKILL sent successfully");
            }

            match child.wait() {
                Ok(status) => {
                    info!(pid = child.id(), status = ?status, "Daemon exited");
                    self.daemon_status = if status.success() {
                        DaemonStatus::Stopped
                    } else {
                        DaemonStatus::Crashed(status.code())
                    };
                }
                Err(e) => {
                    error!(pid = child.id(), error = %e, "Failed to wait for daemon exit");
                    self.daemon_status = DaemonStatus::Crashed(None);
                }
            }
            // Clear IPC channels immediately to prevent "Broken pipe" errors if save_config is called (e.g. on exit)
            self.ipc_config_tx = None;
            self.ipc_status_rx = None;
            self.daemon_status_rx = None;
        }
        Ok(())
    }

    pub fn restart_daemon(&mut self) {
        info!("Restart requested");
        if let Err(err) = self.stop_daemon().and_then(|_| self.start_daemon()) {
            error!(error = ?err, "Failed to restart daemon");
            self.status_message = Some(super::types::StatusMessage {
                text: format!("Restart failed: {err}"),
                color: STATUS_STOPPED,
            });
        }
    }

    pub fn reload_daemon_config(&mut self) {
        info!("Config reload requested - restarting daemon");
        self.restart_daemon();
    }

    pub fn poll_daemon(&mut self) {
        // 1. Check for Bootstrap handshake
        if let Some(ref rx) = self.bootstrap_rx
            && let Ok(msg) = rx.try_recv()
        {
            info!("Received IPC channels from daemon");
            let (config_tx, status_rx) = msg;
            self.ipc_config_tx = Some(config_tx);

            // Bridge status_rx to Manager thread
            let (manager_tx, manager_rx) = mpsc::channel();
            self.daemon_status_rx = Some(manager_rx);

            std::thread::spawn(move || {
                while let Ok(msg) = status_rx.recv() {
                    if manager_tx.send(msg).is_err() {
                        break; // Manager dropped
                    }
                }
            });

            // Sync config to daemon
            let _ = self.save_config();

            self.bootstrap_rx = None; // Done
            self.daemon_status = DaemonStatus::Running;
        }

        // 2. Poll Status Messages
        let mut profile_switch_request = None;
        if let Some(ref rx) = self.daemon_status_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    DaemonMessage::Log { level, message } => {
                        info!(level = %level, "Daemon: {}", message);
                    }
                    DaemonMessage::Error(e) => {
                        error!("Daemon Error: {}", e);
                    }
                    DaemonMessage::PositionChanged {
                        name,
                        x,
                        y,
                        width,
                        height,
                    } => {
                        if let Some(profile) = self.config.get_active_profile_mut() {
                            profile
                                .character_thumbnails
                                .entry(name.clone())
                                .and_modify(|s| {
                                    s.x = x;
                                    s.y = y;
                                    s.dimensions.width = width;
                                    s.dimensions.height = height;
                                })
                                .or_insert_with(|| {
                                    crate::common::types::CharacterSettings::new(
                                        x, y, width, height,
                                    )
                                });
                        }

                        let auto_save = self
                            .config
                            .get_active_profile()
                            .map(|p| p.thumbnail_auto_save_position)
                            .unwrap_or(false);

                        if auto_save {
                            // Debounce save: only write to disk if it's been at least 1 second since last attempt
                            if self.last_save_attempt.elapsed() > Duration::from_secs(1) {
                                let _ = self.config.save_with_strategy(SaveStrategy::Overwrite);
                                self.last_save_attempt = Instant::now();
                                debug!("Debounced auto-save triggered");
                            } else {
                                self.settings_changed = true; // Mark as dirty for final save
                            }
                        }
                    }
                    DaemonMessage::CharacterDetected(name) => {
                        info!("Daemon detected character: {}", name);
                    }
                    DaemonMessage::RequestProfileSwitch(name) => {
                        info!("Daemon requested profile switch: {}", name);
                        profile_switch_request = Some(name);
                    }
                    _ => {}
                }
            }
        }

        if let Some(name) = profile_switch_request {
            if let Some(idx) = self
                .config
                .profiles
                .iter()
                .position(|p| p.profile_name == name)
            {
                self.switch_profile(idx);
            } else {
                warn!("Requested profile '{}' not found", name);
            }
        }

        if self.last_health_check.elapsed() < Duration::from_millis(DAEMON_CHECK_INTERVAL_MS) {
            return;
        }
        self.last_health_check = Instant::now();

        if let Some(child) = self.daemon.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!(pid = child.id(), exit = ?status.code(), "Daemon exited unexpectedly");
                    self.daemon = None;
                    self.daemon_status = if status.success() {
                        DaemonStatus::Stopped
                    } else {
                        DaemonStatus::Crashed(status.code())
                    };
                    self.ipc_config_tx = None;
                    self.ipc_status_rx = None;
                    self.daemon_status_rx = None;
                }
                Ok(None) => {}
                Err(err) => {
                    error!(error = ?err, "Failed to query daemon status");
                }
            }
        }
    }
}
