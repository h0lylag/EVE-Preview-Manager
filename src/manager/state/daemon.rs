use anyhow::{Context, Result};
use ipc_channel::ipc::IpcOneShotServer;
use std::process::{Child, ExitStatus};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use crate::common::constants::manager_ui::*;
use crate::common::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage, ThumbnailSpatialUpdate};

use super::core::SaveMode;
use crate::manager::utils::spawn_daemon;

use super::DaemonStatus;
use super::SharedState;

const GRACEFUL_DAEMON_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(25);

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = child
            .try_wait()
            .context("Failed to query daemon status while stopping")?
        {
            return Ok(Some(status));
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }

        std::thread::sleep((deadline - now).min(DAEMON_SHUTDOWN_POLL_INTERVAL));
    }
}

impl SharedState {
    pub fn start_daemon(&mut self) -> Result<()> {
        if self.daemon.is_some() {
            return Ok(());
        }

        if let Err(err) = self.validate_config() {
            warn!(error = ?err, "Daemon start blocked by invalid configuration");
            self.daemon_status = DaemonStatus::Stopped;
            self.status_message = Some(super::types::StatusMessage {
                text: format!("Daemon not started: {err}"),
                color: STATUS_STOPPED,
            });
            self.config_status_message = Some(super::types::StatusMessage {
                text: "Fix profile or custom source names before applying".to_string(),
                color: COLOR_ERROR,
            });
            return Ok(());
        }

        // 1. Create IPC OneShot Server
        let (server, server_name) =
            IpcOneShotServer::<BootstrapMessage>::new().context("Failed to create IPC server")?;

        // 2. Spawn Daemon with server name
        let child = spawn_daemon(&server_name, self.debug_mode)?;
        let pid = child.id();
        debug!(pid, server_name = %server_name, "Started daemon process");

        // 3. Spawn thread to wait for connection (avoid blocking Manager)
        let (tx, rx) = mpsc::channel();
        self.bootstrap_rx = Some(rx);

        std::thread::spawn(move || {
            debug!("Waiting for daemon IPC connection...");
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
            let pid = child.id();
            info!(pid, "Stopping daemon process");

            let had_ipc_channel = if let Some(tx) = self.ipc_config_tx.take() {
                match tx.send(ConfigMessage::Shutdown) {
                    Ok(()) => {
                        debug!(pid, "Sent graceful shutdown request to daemon");
                    }
                    Err(e) => {
                        warn!(
                            pid,
                            error = %e,
                            "Failed to send graceful shutdown request; waiting for daemon anyway"
                        );
                    }
                }
                true
            } else {
                false
            };

            let status = if had_ipc_channel {
                match wait_for_child_exit(&mut child, GRACEFUL_DAEMON_SHUTDOWN_TIMEOUT)? {
                    Some(status) => status,
                    None => {
                        warn!(
                            pid,
                            timeout_ms = GRACEFUL_DAEMON_SHUTDOWN_TIMEOUT.as_millis(),
                            "Daemon did not exit gracefully in time; sending SIGKILL"
                        );
                        if let Err(e) = child.kill() {
                            error!(pid, error = %e, "Failed to send SIGKILL to daemon");
                        } else {
                            debug!(pid, "SIGKILL sent successfully");
                        }
                        child
                            .wait()
                            .context("Failed to wait for daemon after SIGKILL")?
                    }
                }
            } else {
                warn!(pid, "No daemon IPC channel available; sending SIGKILL");
                if let Err(e) = child.kill() {
                    error!(pid, error = %e, "Failed to send SIGKILL to daemon");
                } else {
                    debug!(pid, "SIGKILL sent successfully");
                }
                child
                    .wait()
                    .context("Failed to wait for daemon after SIGKILL")?
            };

            info!(pid, status = ?status, "Daemon exited");
            self.daemon_status = if status.success() {
                DaemonStatus::Stopped
            } else {
                DaemonStatus::Crashed(status.code())
            };

            // Clear IPC channels immediately to prevent "Broken pipe" errors if save_config is called (e.g. on exit)
            self.ipc_status_rx = None;
            self.daemon_status_rx = None;
            self.bootstrap_rx = None;
            self.ipc_healthy = false;
            self.missed_heartbeats = 0;
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

    fn apply_thumbnail_positions(&mut self, updates: &[ThumbnailSpatialUpdate]) -> bool {
        let Some(profile) = self.config.get_active_profile_mut() else {
            return false;
        };

        let mut changed = false;
        for update in updates {
            changed |= profile.update_thumbnail_spatial(
                &update.source,
                update.position,
                update.dimensions,
            );
        }
        changed
    }

    fn position_save_due(&self) -> bool {
        self.pending_position_save
            && self.last_save_attempt.elapsed() >= Duration::from_millis(AUTO_SAVE_DELAY_MS)
    }

    fn flush_pending_position_save(&mut self) {
        if !self.position_save_due() {
            return;
        }

        self.last_save_attempt = Instant::now();
        if let Err(error) = self.persist_config(SaveMode::Explicit) {
            error!(error = %error, "Failed to auto-save thumbnail positions");
            self.pending_position_save = true;
            self.settings_changed = true;
        } else {
            debug!("Deferred thumbnail position auto-save completed");
        }
    }

    pub fn poll_daemon(&mut self) {
        // 1. Check for Bootstrap handshake
        if let Some(ref rx) = self.bootstrap_rx
            && let Ok(msg) = rx.try_recv()
        {
            debug!("Received IPC channels from daemon");
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

            // Send the startup-only configuration snapshot.
            if let Err(err) = self.send_initial_config_to_daemon() {
                error!(error = ?err, "Failed to send initial config to daemon");
                self.status_message = Some(super::types::StatusMessage {
                    text: format!("Initial config failed: {err}"),
                    color: STATUS_STOPPED,
                });
            }

            self.bootstrap_rx = None; // Done
            self.daemon_status = DaemonStatus::Running;

            // initialize heartbeats
            self.ipc_healthy = true;
            self.last_heartbeat = Instant::now();
            self.missed_heartbeats = 0;
        }

        // 2. Poll Status Messages
        let mut profile_switch_request = None;

        // Collect messages first to avoid holding an immutable borrow on self while calling mutable methods (save_config)
        let messages: Vec<DaemonMessage> = if let Some(ref rx) = self.daemon_status_rx {
            let mut msgs = Vec::new();
            while let Ok(msg) = rx.try_recv() {
                msgs.push(msg);
            }
            msgs
        } else {
            Vec::new()
        };

        for msg in messages {
            match msg {
                DaemonMessage::Log { level, message } => {
                    info!(level = %level, "Daemon: {}", message);
                }
                DaemonMessage::Error(e) => {
                    error!("Daemon Error: {}", e);
                }
                DaemonMessage::Status(msg) => {
                    info!("Daemon Status: {}", msg);
                    self.status_message = Some(crate::manager::state::StatusMessage {
                        text: msg,
                        color: crate::common::constants::manager_ui::STATUS_RUNNING,
                    });
                }
                DaemonMessage::PositionsChanged { updates } => {
                    let changed = self.apply_thumbnail_positions(&updates);
                    if !changed {
                        continue;
                    }

                    let auto_save = self
                        .config
                        .get_active_profile()
                        .map(|p| p.thumbnail_auto_save_position)
                        .unwrap_or(false);

                    debug!("Position changed: auto_save={}", auto_save);
                    self.settings_changed = true;
                    self.config_status_message = None;

                    if auto_save {
                        // Confirm the complete batch. The daemon will skip its own coordinates
                        // through the existing idempotency check.
                        if let Some(ref tx) = self.ipc_config_tx
                            && let Err(error) = tx.send(ConfigMessage::ThumbnailMoves { updates })
                        {
                            warn!(error = %error, "Failed to acknowledge thumbnail position batch");
                        }
                        self.pending_position_save = true;
                        self.flush_pending_position_save();
                    }
                }
                DaemonMessage::CharacterDetected { name, is_custom } => {
                    if is_custom {
                        info!("Daemon detected custom source: {}", name);
                    } else {
                        info!("Daemon detected character: {}", name);
                    }
                }
                DaemonMessage::RequestProfileSwitch(name) => {
                    info!("Daemon requested profile switch: {}", name);
                    profile_switch_request = Some(name);
                }
                DaemonMessage::Heartbeat => {
                    self.ipc_healthy = true;
                    self.last_heartbeat = Instant::now();
                    self.missed_heartbeats = 0;
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

        self.flush_pending_position_save();

        // IPC Health Check
        // If connected but no heartbeat for 15s (5s grace * 3), assume hung process
        if self.daemon.is_some()
            && self.ipc_healthy
            && self.last_heartbeat.elapsed() > Duration::from_secs(5)
        {
            // Only count missed beats if we are expecting them
            if self.daemon_status == DaemonStatus::Running {
                self.missed_heartbeats += 1;

                // We poll roughly every DAEMON_CHECK_INTERVAL_MS (500ms).
                // So wait 30 ticks (15s) or just use time elapsed.
                // Actually, simpler to just check total elapsed time since last beat.
                if self.last_heartbeat.elapsed() > Duration::from_secs(15) {
                    warn!("IPC appears unhealthy (no heartbeat for 15s), restarting daemon");
                    self.ipc_healthy = false;
                    self.restart_daemon();
                    return; // Restart will reset everything
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::{Dimensions, Position, SourceIdentity};
    use crate::config::profile::Config;
    use std::process::{Command, Stdio};

    fn spawn_shell(script: &str) -> Child {
        Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn shell child for daemon wait test")
    }

    fn sample_spatial_update() -> ThumbnailSpatialUpdate {
        ThumbnailSpatialUpdate::new(
            SourceIdentity::eve("Character"),
            Position::new(10, 20),
            Dimensions::new(300, 200),
        )
    }

    fn deliver_positions(state: &mut SharedState, updates: Vec<ThumbnailSpatialUpdate>) {
        let (sender, receiver) = mpsc::channel();
        state.daemon_status_rx = Some(receiver);
        sender
            .send(DaemonMessage::PositionsChanged { updates })
            .expect("position batch should enter the manager queue");
        state.poll_daemon();
    }

    #[test]
    fn wait_for_child_exit_returns_completed_status() {
        let mut child = spawn_shell("exit 0");

        let status = wait_for_child_exit(&mut child, Duration::from_secs(1))
            .expect("wait helper should not error")
            .expect("child should exit before timeout");

        assert!(status.success());
    }

    #[test]
    fn wait_for_child_exit_returns_none_on_timeout() {
        let mut child = spawn_shell("sleep 5");

        let status = wait_for_child_exit(&mut child, Duration::from_millis(10))
            .expect("wait helper should not error");

        assert!(status.is_none());
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn applies_entire_spatial_batch_before_reporting_change() {
        let mut state = SharedState::new(Config::default(), false);
        let updates = vec![
            ThumbnailSpatialUpdate::new(
                SourceIdentity::eve("Shared Name"),
                Position::new(10, 20),
                Dimensions::new(300, 200),
            ),
            ThumbnailSpatialUpdate::new(
                SourceIdentity::custom("Shared Name"),
                Position::new(40, 50),
                Dimensions::new(640, 360),
            ),
        ];

        assert!(state.apply_thumbnail_positions(&updates));
        let profile = state
            .config
            .get_active_profile()
            .expect("default config should have an active profile");
        assert_eq!(profile.character_thumbnails["Shared Name"].x, 10);
        assert_eq!(profile.character_thumbnails["Shared Name"].y, 20);
        assert_eq!(
            profile.character_thumbnails["Shared Name"].dimensions,
            Dimensions::new(300, 200)
        );
        assert_eq!(profile.custom_source_thumbnails["Shared Name"].x, 40);
        assert_eq!(profile.custom_source_thumbnails["Shared Name"].y, 50);
        assert_eq!(
            profile.custom_source_thumbnails["Shared Name"].dimensions,
            Dimensions::new(640, 360)
        );
        assert!(!state.apply_thumbnail_positions(&updates));
    }

    #[test]
    fn pending_position_save_becomes_due_after_debounce() {
        let mut state = SharedState::new(Config::default(), false);

        state.pending_position_save = true;
        assert!(!state.position_save_due());

        state.last_save_attempt = Instant::now() - Duration::from_millis(AUTO_SAVE_DELAY_MS);
        assert!(state.position_save_due());
    }

    #[test]
    fn auto_save_off_keeps_position_changes_pending_for_manual_save() {
        let mut state = SharedState::new(Config::default(), false);
        state
            .config
            .get_active_profile_mut()
            .expect("default config should have an active profile")
            .thumbnail_auto_save_position = false;

        deliver_positions(&mut state, vec![sample_spatial_update()]);

        assert!(state.settings_changed);
        assert!(!state.pending_position_save);
    }

    #[test]
    fn auto_save_on_schedules_a_deferred_position_save() {
        let mut state = SharedState::new(Config::default(), false);
        state
            .config
            .get_active_profile_mut()
            .expect("default config should have an active profile")
            .thumbnail_auto_save_position = true;
        state.last_save_attempt = Instant::now();

        deliver_positions(&mut state, vec![sample_spatial_update()]);

        assert!(state.settings_changed);
        assert!(state.pending_position_save);
        assert!(!state.position_save_due());
    }
}
