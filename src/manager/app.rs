//! Application manager - primary interface for configuration and daemon control

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use eframe::{NativeOptions, egui};
use tracing::{debug, error, info};

#[cfg(target_os = "linux")]
use ksni::TrayMethods;

use super::components;
use crate::common::constants::manager_ui::*;
use crate::config::backup::BackupManager;
use crate::config::profile::Config;
use crate::manager::components::profile_selector::{ProfileAction, ProfileSelector};
#[cfg(target_os = "linux")]
use crate::manager::components::tray::AppTray;
use crate::manager::state::core::SaveMode;
use crate::manager::state::{ManagerTab, SharedState, StatusMessage};
use crate::manager::utils::load_window_icon;
use crate::manager::window_lifecycle::{StartupMode, WindowConditions, WindowLifecycle};

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
    #[cfg(target_os = "linux")]
    tray_ready: Arc<AtomicBool>,

    active_tab: ManagerTab,
    window_lifecycle: WindowLifecycle,
}

fn window_startup_mode(config: &Config) -> StartupMode {
    if config.global.minimize_to_tray && config.global.start_minimized_to_tray {
        StartupMode::HideWhenTrayReady
    } else {
        StartupMode::Show
    }
}

impl ManagerApp {
    fn new(cc: &eframe::CreationContext<'_>, config: Config, debug_mode: bool) -> Self {
        debug!("Initializing Manager (debug_mode={})", debug_mode);

        let startup_mode = window_startup_mode(&config);
        let window_lifecycle = WindowLifecycle::new(startup_mode);
        #[cfg(target_os = "linux")]
        let show_window_signal = window_lifecycle.show_signal();

        // Run auto-backup if enabled
        if config.global.backup_enabled {
            if BackupManager::should_run_auto_backup(config.global.backup_interval_days, None) {
                info!("Auto-backup triggered due to interval expiration");
                match BackupManager::create_backup(false, None) {
                    Ok(_) => {
                        if let Err(e) =
                            BackupManager::prune_backups(config.global.backup_retention_count, None)
                        {
                            error!("Failed to prune backups: {}", e);
                        }
                    }
                    Err(e) => error!("Failed to create auto-backup: {}", e),
                }
            } else {
                // Determine if we need to prune anyway (e.g. retention count changed)
                // Just in case, run prune on startup to enforce policy
                if let Err(e) =
                    BackupManager::prune_backups(config.global.backup_retention_count, None)
                {
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
        let tray_ready = Arc::new(AtomicBool::new(false));
        #[cfg(target_os = "linux")]
        let tray_ready_clone = tray_ready.clone();
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
                    show_window_signal,
                };

                let result = if is_flatpak {
                    info!("Running in Flatpak: spawning tray without D-Bus name");
                    tray.disable_dbus_name(true).spawn().await
                } else {
                    tray.spawn().await
                };

                match result {
                    Ok(handle) => {
                        tray_ready_clone.store(true, Ordering::Release);
                        debug!("Tray icon created via ksni/D-Bus");
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
            tray_ready,
            profile_selector: ProfileSelector::new(),
            behavior_settings_state,
            hotkey_settings_state,
            visual_settings_state,
            characters_state,
            sources_state: components::sources::SourcesTab::default(),
            active_tab: ManagerTab::Behavior,
            window_lifecycle,
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
            window_lifecycle,
        };

        app
    }
}

impl ManagerApp {
    // Eframe still calls `logic` for repaint requests while the UI is hidden, so
    // daemon polling and viewport transitions remain here.
    fn update_logic(&mut self, ctx: &egui::Context) {
        let mut state_guard = match self.state.lock() {
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

        // Read the native state used for lifecycle and geometry updates.
        let (is_minimized, inner_rect) = ctx.input(|input| {
            let viewport = input.viewport();
            (viewport.minimized.unwrap_or(false), viewport.inner_rect)
        });

        #[cfg(target_os = "linux")]
        let tray_ready = self.tray_ready.load(Ordering::Acquire);
        #[cfg(not(target_os = "linux"))]
        let tray_ready = false;

        self.window_lifecycle.update(
            ctx,
            WindowConditions {
                minimize_to_tray_enabled: state.config.global.minimize_to_tray,
                start_hidden_enabled: state.config.global.start_minimized_to_tray,
                tray_ready,
                is_minimized,
            },
        );

        // Try to get window size from viewport inner_rect first, fall back to content_rect
        let (new_width, new_height) = if let Some(inner_rect) = inner_rect {
            (inner_rect.width() as u16, inner_rect.height() as u16)
        } else {
            // Fall back when native window geometry is unavailable.
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

        ctx.request_repaint_after(Duration::from_millis(DAEMON_CHECK_INTERVAL_MS));
    }
}

impl eframe::App for ManagerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_logic(ctx);
    }

    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root_ui.ctx().clone();

        let mut state_guard = match self.state.lock() {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to lock shared state: {:?}", e);
                return;
            }
        };
        let state = &mut *state_guard;

        let mut action = ProfileAction::None;

        // Global Header Panel (Fixed at top)
        egui::Panel::top("global_header").show(root_ui, |ui| {
            action = components::header::render(
                &ctx,
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

                if let Err(err) = state.save_config(SaveMode::Implicit) {
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
                if let Err(err) = state.save_config(SaveMode::Implicit) {
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
        egui::CentralPanel::default().show(root_ui, |ui| {
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
                        if self.sources_state.ui(
                            ui,
                            current_profile,
                            &mut self.hotkey_settings_state,
                        ) {
                            state.settings_changed = true;
                            state.config_status_message = None;
                        }
                    }
                }
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Ok(mut state) = self.state.lock() {
            if let Err(err) = state.stop_daemon() {
                error!(error = ?err, "Failed to stop daemon during shutdown");
            }
            // Save config (merging daemon positions if needed, though daemon is stopped)
            // Just saving is enough because the logic callback keeps state.config fresh.
            if let Err(err) = state.save_config(SaveMode::Implicit) {
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
            debug!(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(config: Config) -> ManagerApp {
        let startup_mode = window_startup_mode(&config);

        ManagerApp {
            state: Arc::new(Mutex::new(SharedState::new(config, false))),
            profile_selector: ProfileSelector::new(),
            behavior_settings_state: components::behavior_settings::BehaviorSettingsState::default(
            ),
            hotkey_settings_state: components::hotkey_settings::HotkeySettingsState::default(),
            visual_settings_state: components::visual_settings::VisualSettingsState::default(),
            characters_state: components::characters::CharactersState::default(),
            sources_state: components::sources::SourcesTab::default(),
            #[cfg(target_os = "linux")]
            shutdown_signal: Arc::new(tokio::sync::Notify::new()),
            #[cfg(target_os = "linux")]
            update_signal: Arc::new(tokio::sync::Notify::new()),
            #[cfg(target_os = "linux")]
            tray_ready: Arc::new(AtomicBool::new(false)),
            active_tab: ManagerTab::Behavior,
            window_lifecycle: WindowLifecycle::new(startup_mode),
        }
    }

    fn run_logic(app: &mut ManagerApp, is_minimized: bool) -> egui::FullOutput {
        let mut raw_input = egui::RawInput::default();
        raw_input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .minimized = Some(is_minimized);

        egui::Context::default().run_ui(raw_input, |ui| {
            app.update_logic(ui.ctx());
        })
    }

    fn root_commands(output: &egui::FullOutput) -> &[egui::ViewportCommand] {
        &output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("root viewport output should always exist")
            .commands
    }

    fn tray_config(startup_mode: StartupMode) -> Config {
        let mut config = Config::default();
        config.global.minimize_to_tray = true;
        config.global.start_minimized_to_tray = startup_mode == StartupMode::HideWhenTrayReady;
        config
    }

    #[cfg(target_os = "linux")]
    fn mark_tray_ready(app: &ManagerApp) {
        app.tray_ready.store(true, Ordering::Release);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn minimized_window_hides_when_tray_is_ready() {
        let mut app = test_app(tray_config(StartupMode::Show));
        mark_tray_ready(&app);

        let output = run_logic(&mut app, true);

        assert_eq!(
            root_commands(&output),
            &[
                egui::ViewportCommand::Minimized(false),
                egui::ViewportCommand::Visible(false),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn minimized_window_stays_minimized_when_tray_is_unavailable() {
        let mut app = test_app(tray_config(StartupMode::Show));

        let output = run_logic(&mut app, true);

        assert!(root_commands(&output).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_hide_waits_until_tray_is_ready() {
        let mut app = test_app(tray_config(StartupMode::HideWhenTrayReady));

        let waiting_output = run_logic(&mut app, false);
        assert!(root_commands(&waiting_output).is_empty());

        mark_tray_ready(&app);
        let ready_output = run_logic(&mut app, false);
        assert_eq!(
            root_commands(&ready_output),
            &[
                egui::ViewportCommand::Minimized(false),
                egui::ViewportCommand::Visible(false),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disabling_either_setting_cancels_pending_startup_hide() {
        for disable_minimize_to_tray in [false, true] {
            let mut app = test_app(tray_config(StartupMode::HideWhenTrayReady));
            {
                let mut state = app
                    .state
                    .lock()
                    .expect("test shared state lock should not be poisoned");
                if disable_minimize_to_tray {
                    state.config.global.minimize_to_tray = false;
                } else {
                    state.config.global.start_minimized_to_tray = false;
                }
            }

            let _ = run_logic(&mut app, false);

            {
                let mut state = app
                    .state
                    .lock()
                    .expect("test shared state lock should not be poisoned");
                state.config.global.minimize_to_tray = true;
                state.config.global.start_minimized_to_tray = true;
            }
            mark_tray_ready(&app);

            let output = run_logic(&mut app, false);
            assert!(root_commands(&output).is_empty());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tray_show_request_overrides_pending_startup_hide() {
        let mut app = test_app(tray_config(StartupMode::HideWhenTrayReady));
        app.window_lifecycle.show_signal().request();

        let output = run_logic(&mut app, true);

        assert_eq!(
            root_commands(&output),
            &[
                egui::ViewportCommand::Minimized(false),
                egui::ViewportCommand::Visible(true),
                egui::ViewportCommand::Focus,
            ]
        );

        let stale_minimized_output = run_logic(&mut app, true);
        assert!(root_commands(&stale_minimized_output).is_empty());

        let next_output = run_logic(&mut app, false);
        assert!(root_commands(&next_output).is_empty());

        mark_tray_ready(&app);
        let ready_output = run_logic(&mut app, false);
        assert!(root_commands(&ready_output).is_empty());
    }

    #[test]
    fn logic_handles_tray_quit_without_rendering_ui() {
        let mut app = test_app(Config::default());
        app.state
            .lock()
            .expect("test shared state lock should not be poisoned")
            .should_quit = true;

        let output = run_logic(&mut app, false);
        let close_requested = root_commands(&output).contains(&egui::ViewportCommand::Close);
        assert!(close_requested, "logic should process a tray quit request");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn logic_handles_tray_quit_while_hidden_to_tray() {
        let mut app = test_app(tray_config(StartupMode::Show));
        mark_tray_ready(&app);
        let hide_output = run_logic(&mut app, true);
        assert!(root_commands(&hide_output).contains(&egui::ViewportCommand::Visible(false)));

        app.state
            .lock()
            .expect("test shared state lock should not be poisoned")
            .should_quit = true;

        let output = run_logic(&mut app, false);

        assert!(root_commands(&output).contains(&egui::ViewportCommand::Close));
    }
}
