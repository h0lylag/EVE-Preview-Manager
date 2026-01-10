//! Application manager - primary interface for configuration and daemon control

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use eframe::{NativeOptions, egui};
use tracing::{error, info};

#[cfg(target_os = "linux")]
use ksni::TrayMethods;

use super::components;
use crate::common::constants::manager_ui::*;
use crate::config::backup::BackupManager;
use crate::config::profile::Config;
use crate::manager::components::profile_selector::{ProfileAction, ProfileSelector};
#[cfg(target_os = "linux")]
use crate::manager::components::tray::AppTray;
use crate::manager::state::{ManagerTab, SharedState, StatusMessage};
use crate::manager::utils::load_window_icon;

struct ManagerApp {
    state: Arc<Mutex<SharedState>>,

    // UI-only state (doesn't need to be shared deeply)
    profile_selector: ProfileSelector,
    behavior_settings_state: components::behavior_settings::BehaviorSettingsState,
    hotkey_settings_state: components::hotkey_settings::HotkeySettingsState,
    visual_settings_state: components::visual_settings::VisualSettingsState,
    characters_state: components::characters::CharactersState,
    sources_state: components::sources::SourcesTab,
    #[cfg(target_os = "linux")]
    shutdown_signal: std::sync::Arc<tokio::sync::Notify>,
    #[cfg(target_os = "linux")]
    update_signal: std::sync::Arc<tokio::sync::Notify>,

    active_tab: ManagerTab,
}

impl ManagerApp {
    fn new(cc: &eframe::CreationContext<'_>, config: Config, debug_mode: bool) -> Self {
        info!("Initializing Manager (debug_mode={})", debug_mode);

        // Run auto-backup if enabled
        if config.global.backup_enabled {
            if BackupManager::should_run_auto_backup(config.global.backup_interval_days) {
                info!("Auto-backup triggered due to interval expiration");
                match BackupManager::create_backup(false) {
                    Ok(_) => {
                        if let Err(e) =
                            BackupManager::prune_backups(config.global.backup_retention_count)
                        {
                            error!("Failed to prune backups: {}", e);
                        }
                    }
                    Err(e) => error!("Failed to create auto-backup: {}", e),
                }
            } else {
                // Determine if we need to prune anyway (e.g. retention count changed)
                // Just in case, run prune on startup to enforce policy
                if let Err(e) = BackupManager::prune_backups(config.global.backup_retention_count) {
                    error!("Failed to prune backups: {}", e);
                }
            }
        }

        // Initialize SharedState
        let mut state = SharedState::new(config.clone(), debug_mode);
        if let Err(err) = state.start_daemon() {
            error!(error = ?err, "Failed to start preview daemon");
            state.status_message = Some(StatusMessage {
                text: format!("Failed to start daemon: {err}"),
                color: STATUS_STOPPED,
            });
        }
        let state = Arc::new(Mutex::new(state));
        let state_clone = state.clone();

        #[cfg(target_os = "linux")]
        let shutdown_signal = std::sync::Arc::new(tokio::sync::Notify::new());
        #[cfg(target_os = "linux")]
        let shutdown_clone = shutdown_signal.clone();
        #[cfg(target_os = "linux")]
        let update_signal = std::sync::Arc::new(tokio::sync::Notify::new());
        #[cfg(target_os = "linux")]
        let update_clone = update_signal.clone();
        #[cfg(target_os = "linux")]
        let ctx = cc.egui_ctx.clone();

        #[cfg(target_os = "linux")]
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime for tray");

            runtime.block_on(async move {
                let is_flatpak = std::env::var("FLATPAK_ID").is_ok();
                let tray = AppTray {
                    state: state_clone,
                    ctx,
                    is_flatpak,
                };

                let result = if is_flatpak {
                    info!("Running in Flatpak: spawning tray without D-Bus name");
                    tray.disable_dbus_name(true).spawn().await
                } else {
                    tray.spawn().await
                };

                match result {
                    Ok(handle) => {
                        info!("Tray icon created via ksni/D-Bus");
                        // Event loop for tray management
                        // We use select! to handle both shutdown and update requests
                        loop {
                            tokio::select! {
                                _ = shutdown_clone.notified() => {
                                    handle.shutdown().await;
                                    break;
                                }
                                _ = update_clone.notified() => {
                                    // Trigger menu refresh
                                    // KSNI's update method takes a closure to modify the service/icon,
                                    // but we just need it to trigger a "PropertiesChanged" signal or similar
                                    // to make the system tray re-read our menu structure.
                                    handle.update(|_| {}).await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = ?e, "Failed to create tray icon (D-Bus unavailable?)");
                    }
                }
            });
        });

        let selected_profile_idx = config
            .profiles
            .iter()
            .position(|p| p.profile_name == config.global.selected_profile)
            .unwrap_or(0);

        let behavior_settings_state =
            components::behavior_settings::BehaviorSettingsState::default();
        let hotkey_settings_state = components::hotkey_settings::HotkeySettingsState::default();
        let visual_settings_state = components::visual_settings::VisualSettingsState::default();

        let mut characters_state = components::characters::CharactersState::default();
        characters_state.load_from_profile(&config.profiles[selected_profile_idx]);

        #[cfg(target_os = "linux")]
        let app = Self {
            state,
            shutdown_signal,
            update_signal,
            profile_selector: ProfileSelector::new(),
            behavior_settings_state,
            hotkey_settings_state,
            visual_settings_state,
            characters_state,
            sources_state: components::sources::SourcesTab::default(),
            active_tab: ManagerTab::Behavior,
        };

        #[cfg(not(target_os = "linux"))]
        let app = Self {
            state,
            profile_selector: ProfileSelector::new(),
            behavior_settings_state,
            hotkey_settings_state,
            visual_settings_state,
            characters_state,
            sources_state: components::sources::SourcesTab::default(),
            active_tab: ManagerTab::Behavior,
        };

        app
    }
}

impl eframe::App for ManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Lock shared state
        // Clone Arc to separate borrow from self
        let state_arc = self.state.clone();
        let mut state_guard = match state_arc.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to lock shared state: {:?}", e);
                return;
            }
        };
        let state = &mut *state_guard;

        let old_profile_idx = state.selected_profile_idx;
        state.poll_daemon();

        #[cfg(target_os = "linux")]
        if state.selected_profile_idx != old_profile_idx {
            self.update_signal.notify_one();
        }

        // Track window geometry changes and update config
        // Clone viewport info to avoid lifetime issues
        let viewport_info = ctx.input(|i| i.viewport().clone());

        // Try to get window size from viewport inner_rect first, fall back to content_rect
        let (new_width, new_height) = if let Some(inner_rect) = viewport_info.inner_rect {
            (inner_rect.width() as u16, inner_rect.height() as u16)
        } else {
            // Fallback for platforms where inner_rect is None (e.g., Wayland)
            // Use the content rect as window size
            let content_rect = ctx.content_rect();
            (content_rect.width() as u16, content_rect.height() as u16)
        };

        // Update config if size changed (will be saved on exit)
        if new_width > 0
            && new_height > 0
            && (new_width != state.config.global.window_width
                || new_height != state.config.global.window_height)
        {
            state.config.global.window_width = new_width;
            state.config.global.window_height = new_height;
        }

        // Handle quit request from tray menu

        if state.should_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        let mut action = ProfileAction::None;

        // Global Header Panel (Fixed at top)
        egui::TopBottomPanel::top("global_header").show(ctx, |ui| {
            action = components::header::render(
                ctx,
                ui,
                state,
                &mut self.active_tab,
                &mut self.profile_selector,
                #[cfg(target_os = "linux")]
                &self.update_signal,
            );
        });

        // Handle Actions
        match action {
            ProfileAction::SwitchProfile => {
                let current_profile = &state.config.profiles[state.selected_profile_idx];
                self.characters_state.load_from_profile(current_profile);

                if let Err(err) = state.save_config() {
                    error!(error = ?err, "Failed to save config after profile switch");
                    state.status_message = Some(StatusMessage {
                        text: format!("Save failed: {err}"),
                        color: COLOR_ERROR,
                    });
                } else {
                    state.reload_daemon_config();
                    #[cfg(target_os = "linux")]
                    self.update_signal.notify_one();
                }
            }
            ProfileAction::ProfileCreated
            | ProfileAction::ProfileDeleted
            | ProfileAction::ProfileUpdated => {
                if let Err(err) = state.save_config() {
                    error!(error = ?err, "Failed to save config after profile action");
                    state.status_message = Some(StatusMessage {
                        text: format!("Save failed: {err}"),
                        color: COLOR_ERROR,
                    });
                } else {
                    state.reload_daemon_config();
                    #[cfg(target_os = "linux")]
                    self.update_signal.notify_one();
                }
            }
            ProfileAction::None => {}
        }

        // Main Content Body
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let current_profile = &mut state.config.profiles[state.selected_profile_idx];

                match self.active_tab {
                    ManagerTab::Behavior => {
                        use components::behavior_settings::BehaviorSettingsAction;
                        match components::behavior_settings::ui(
                            ui,
                            current_profile,
                            &mut state.config.global,
                            &mut self.behavior_settings_state,
                        ) {
                            BehaviorSettingsAction::SettingsChanged => {
                                state.settings_changed = true;
                                state.config_status_message = None;
                            }
                            BehaviorSettingsAction::RestoreTriggered => {
                                // Reload config from disk (disk was just updated by restore)
                                state.discard_changes();
                                // Sync new config to daemon
                                state.reload_daemon_config();
                                // Override the "Changes discarded" message from discard_changes
                                state.config_status_message = Some(StatusMessage {
                                    text: "Configuration restored and reloaded".to_string(),
                                    color: COLOR_SUCCESS,
                                });
                            }
                            BehaviorSettingsAction::None => {}
                        }
                    }
                    ManagerTab::Appearance => {
                        if components::visual_settings::ui(
                            ui,
                            current_profile,
                            &mut self.visual_settings_state,
                        ) {
                            state.settings_changed = true;
                            state.config_status_message = None;
                        }
                    }
                    ManagerTab::Hotkeys => {
                        if components::hotkey_settings::ui(
                            ui,
                            current_profile,
                            &mut self.hotkey_settings_state,
                        ) {
                            state.settings_changed = true;
                            state.config_status_message = None;
                        }
                    }
                    ManagerTab::Characters => {
                        if components::characters::ui(
                            ui,
                            current_profile,
                            &mut self.characters_state,
                            &mut self.hotkey_settings_state,
                        ) {
                            state.settings_changed = true;
                            state.config_status_message = None;
                        }
                    }
                    ManagerTab::Sources => {
                        if self.sources_state.ui(ui, current_profile) {
                            state.settings_changed = true;
                            state.config_status_message = None;
                        }
                    }
                }
            });
        });

        ctx.request_repaint_after(Duration::from_millis(DAEMON_CHECK_INTERVAL_MS));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Ok(mut state) = self.state.lock() {
            if let Err(err) = state.stop_daemon() {
                error!(error = ?err, "Failed to stop daemon during shutdown");
            }
            // Save config (merging daemon positions if needed, though daemon is stopped)
            // Just saving is enough as update loop keeps state.config fresh
            if let Err(err) = state.save_config() {
                error!(error = ?err, "Failed to save window geometry on exit");
            } else {
                info!("Window geometry saved on exit");
            }
        }

        // Signal tray thread to shutdown
        #[cfg(target_os = "linux")]
        {
            self.shutdown_signal.notify_one();
            info!("Signaled tray thread to shutdown");
        }

        info!("Manager exiting");
    }
}

pub fn run_manager(debug_mode: bool) -> Result<()> {
    // Load config to get window dimensions
    let config = Config::load().unwrap_or_default();
    let window_width = config.global.window_width as f32;
    let window_height = config.global.window_height as f32;

    #[cfg(target_os = "linux")]
    let icon = match load_window_icon() {
        Ok(icon_data) => {
            info!(
                "Loaded application icon ({} bytes, {}x{})",
                icon_data.rgba.len(),
                icon_data.width,
                icon_data.height
            );
            Some(icon_data)
        }
        Err(e) => {
            error!("Failed to load window icon: {}", e);
            None
        }
    };

    #[cfg(not(target_os = "linux"))]
    let icon = None;

    let mut viewport_builder = egui::ViewportBuilder::default()
        .with_inner_size([window_width, window_height])
        .with_title("EVE Preview Manager - v".to_string() + env!("CARGO_PKG_VERSION"));

    if let Some(icon_data) = icon {
        viewport_builder = viewport_builder.with_icon(icon_data);
    }

    let options = NativeOptions {
        viewport: viewport_builder,
        ..Default::default()
    };

    eframe::run_native(
        &format!("EVE Preview Manager - v{}", env!("CARGO_PKG_VERSION")),
        options,
        Box::new(move |cc| Ok(Box::new(ManagerApp::new(cc, config, debug_mode)))),
    )
    .map_err(|err| anyhow!("Failed to launch Manager: {err}"))
}
