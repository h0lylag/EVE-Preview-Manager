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

/// Recovery owns no Manager resources, so closing it cannot save configuration.
enum Application {
    Recovery {
        path: std::path::PathBuf,
        error: String,
        debug_mode: bool,
    },
    Running(Box<ManagerApp>),
}

impl Application {
    fn recovery_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let Self::Recovery { path, error, .. } = self else {
            return false;
        };
        let mut retry = false;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Unable to load configuration");
            ui.label("Repair or restore the configuration file, then click Retry. No settings will be saved while this window is open.");
            ui.add_space(10.0);
            ui.add(egui::Label::new(path.display().to_string()).selectable(true).wrap());
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                ui.add(egui::Label::new(error.as_str()).selectable(true).wrap());
            });
            ui.horizontal(|ui| {
                retry = ui.button("Retry").clicked();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
        retry
    }

    fn start_with(
        ctx: &egui::Context,
        config: Result<Config>,
        path: std::path::PathBuf,
        debug_mode: bool,
        create_manager: impl FnOnce(&egui::Context, Config, bool) -> ManagerApp,
    ) -> Self {
        match config {
            Ok(config) => Self::Running(Box::new(create_manager(ctx, config, debug_mode))),
            Err(error) => Self::Recovery {
                path,
                error: format!("{error:#}"),
                debug_mode,
            },
        }
    }

    fn retry_with(
        &mut self,
        ctx: &egui::Context,
        create_manager: impl FnOnce(&egui::Context, Config, bool) -> ManagerApp,
    ) {
        let Self::Recovery {
            path,
            error,
            debug_mode,
        } = self
        else {
            return;
        };
        match Config::read_from(path) {
            Ok(config) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    config.global.window_width as f32,
                    config.global.window_height as f32,
                )));
                let manager = create_manager(ctx, config, *debug_mode);
                // A user who just recovered their settings should see the Manager.
                manager.window_lifecycle.show_signal().request();
                *self = Self::Running(Box::new(manager));
                ctx.request_repaint();
            }
            Err(err) => {
                *error = format!("{err:#}");
                error!(error = %error, "Configuration retry failed");
            }
        }
    }
}

impl eframe::App for Application {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Self::Running(manager) = self {
            manager.logic(ctx, frame);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        match self {
            Self::Running(manager) => manager.ui(ui, frame),
            Self::Recovery { .. } => {
                if self.recovery_ui(ui) {
                    self.retry_with(ui.ctx(), ManagerApp::new);
                }
            }
        }
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let Self::Running(manager) = self {
            manager.on_exit(gl);
        }
    }
}

fn window_startup_mode(config: &Config) -> StartupMode {
    if config.global.minimize_to_tray && config.global.start_minimized_to_tray {
        StartupMode::HideWhenTrayReady
    } else {
        StartupMode::Show
    }
}

impl ManagerApp {
    fn new(ctx: &egui::Context, config: Config, debug_mode: bool) -> Self {
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
        let ctx = ctx.clone();

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

        let behavior_settings_state =
            components::behavior_settings::BehaviorSettingsState::default();
        let hotkey_settings_state = components::hotkey_settings::HotkeySettingsState::default();
        let visual_settings_state = components::visual_settings::VisualSettingsState::default();

        let characters_state = components::characters::CharactersState::default();

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
    fn reset_profile_editors(
        pending: &mut bool,
        characters: &mut components::characters::CharactersState,
        hotkeys: &mut components::hotkey_settings::HotkeySettingsState,
    ) {
        if std::mem::take(pending) {
            characters.reset();
            hotkeys.cancel_capture();
        }
    }

    fn reload_restored_config(
        state: &mut SharedState,
        behavior: &mut components::behavior_settings::BehaviorSettingsState,
        profile_selector: &mut ProfileSelector,
    ) {
        let (text, color) = match profile_selector.reload_config(state) {
            Ok(()) => {
                state.reload_daemon_config();
                (
                    "Configuration restored and reloaded".to_string(),
                    COLOR_SUCCESS,
                )
            }
            Err(error) => (
                format!("Backup restored on disk, but reload failed: {error:#}"),
                COLOR_ERROR,
            ),
        };
        behavior.status_message = Some(text.clone());
        behavior.status_type = Some(color);
        state.config_status_message = Some(StatusMessage { text, color });
    }

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
            ProfileAction::SwitchProfile(new_idx) => {
                if state.switch_profile(new_idx) {
                    #[cfg(target_os = "linux")]
                    self.update_signal.notify_one();
                }
            }
            ProfileAction::ProfileCreated
            | ProfileAction::ProfileDeleted
            | ProfileAction::ProfileUpdated => {
                if action == ProfileAction::ProfileDeleted {
                    state.profile_editors_reload_pending = true;
                }
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

        // Cancel old profile targets before any tab can consume them, including hotkey captures.
        Self::reset_profile_editors(
            &mut state.profile_editors_reload_pending,
            &mut self.characters_state,
            &mut self.hotkey_settings_state,
        );

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
                                Self::reload_restored_config(
                                    state,
                                    &mut self.behavior_settings_state,
                                    &mut self.profile_selector,
                                );
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
    let config = Config::load();
    let (window_width, window_height) = match &config {
        Ok(config) => (
            config.global.window_width as f32,
            config.global.window_height as f32,
        ),
        Err(error) => {
            error!(error = %format!("{error:#}"), "Unable to load configuration");
            (640.0, 400.0)
        }
    };

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
        Box::new(move |cc| {
            let app = Application::start_with(
                &cc.egui_ctx,
                config,
                Config::path(),
                debug_mode,
                ManagerApp::new,
            );
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow!("Failed to launch Manager: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App as _;

    #[test]
    fn startup_recovery_retry_and_exit_preserve_files_without_starting_manager() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let backup = dir.path().join("backups/keep.tar.gz");
        std::fs::create_dir(backup.parent().unwrap()).unwrap();
        std::fs::write(&backup, b"backup").unwrap();
        let ctx = egui::Context::default();

        for bytes in [b"{broken".as_slice(), &[0xff, 0xfe]] {
            std::fs::write(&path, bytes).unwrap();
            let mut app = Application::start_with(
                &ctx,
                Config::load_from(&path),
                path.clone(),
                false,
                |_, _, _| panic!("failed load must not initialize Manager resources"),
            );
            assert!(matches!(app, Application::Recovery { .. }));
            for _ in 0..2 {
                app.retry_with(&ctx, |_, _, _| {
                    panic!("failed retry must not initialize Manager")
                });
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                    assert!(!app.recovery_ui(ui));
                });
                output.textures_delta.clear();
                assert!(!output.shapes.is_empty());
            }
            app.on_exit(None);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(std::fs::read(&backup).unwrap(), b"backup");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        }

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let mut app = Application::start_with(
            &ctx,
            Config::load_from(&path),
            path.clone(),
            false,
            |_, _, _| panic!("read failure must enter recovery"),
        );
        assert!(matches!(app, Application::Recovery { .. }));
        std::fs::remove_dir(&path).unwrap();
        app.retry_with(&ctx, |_, _, _| {
            panic!("missing file during retry must not create defaults")
        });
        app.on_exit(None);
        assert!(!path.exists());
        assert_eq!(std::fs::read(&backup).unwrap(), b"backup");
    }

    #[test]
    fn repaired_config_starts_manager_once_and_keeps_it_visible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, b"{broken").unwrap();
        let ctx = egui::Context::default();
        let mut app = Application::start_with(
            &ctx,
            Config::load_from(&path),
            path.clone(),
            true,
            |_, _, _| panic!("failed load must enter recovery"),
        );
        let mut config = tray_config(StartupMode::HideWhenTrayReady);
        config.global.window_width = 812;
        config.global.window_height = 613;
        config.save_to(&path).unwrap();
        let repaired = std::fs::read(&path).unwrap();
        let starts = std::cell::Cell::new(0);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.retry_with(ui.ctx(), |_, config, debug| {
                assert!(debug);
                starts.set(starts.get() + 1);
                test_app(config)
            });
        });
        output.textures_delta.clear();
        assert_eq!(starts.get(), 1);
        assert!(
            root_commands(&output)
                .contains(&egui::ViewportCommand::InnerSize(egui::vec2(812.0, 613.0)))
        );
        app.retry_with(&ctx, |_, _, _| {
            panic!("already running Manager must not be initialized again")
        });
        let Application::Running(manager) = &mut app else {
            panic!("expected running Manager");
        };
        assert_eq!(
            manager.state.lock().unwrap().config.global.window_width,
            812
        );
        #[cfg(target_os = "linux")]
        mark_tray_ready(manager);
        let output = run_logic(manager, false);
        assert!(!root_commands(&output).contains(&egui::ViewportCommand::Visible(false)));
        assert_eq!(std::fs::read(&path).unwrap(), repaired);
    }

    #[test]
    fn failed_restore_reload_keeps_runtime_and_exit_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, b"{broken").unwrap();
        let mut app = test_app(Config::default());
        let mut state = SharedState::at_path(Config::default(), &path);
        state.daemon_status = crate::manager::state::DaemonStatus::Running;
        state.settings_changed = true;
        let config_before = serde_json::to_value(&state.config).unwrap();
        let (sender, receiver) = ipc_channel::ipc::channel().unwrap();
        state.ipc_config_tx = Some(sender);

        ManagerApp::reload_restored_config(
            &mut state,
            &mut app.behavior_settings_state,
            &mut app.profile_selector,
        );

        assert_eq!(
            state.daemon_status,
            crate::manager::state::DaemonStatus::Running
        );
        assert!(state.ipc_config_tx.is_some());
        assert!(receiver.try_recv().is_err());
        assert_eq!(serde_json::to_value(&state.config).unwrap(), config_before);
        assert!(state.settings_changed);
        assert!(state.config_load_error.is_some());
        assert!(
            app.behavior_settings_state
                .status_message
                .as_ref()
                .unwrap()
                .contains("reload failed")
        );
        assert_eq!(app.behavior_settings_state.status_type, Some(COLOR_ERROR));
        app.state = Arc::new(Mutex::new(state));
        app.on_exit(None);
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
    }

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

        let ctx = egui::Context::default();
        // Test lifecycle commands without egui's automatic window-theme command.
        ctx.options_mut(|options| options.sync_window_theme = false);
        let mut output = ctx.run_ui(raw_input, |ui| {
            app.update_logic(ui.ctx());
        });
        // Headless tests have no renderer to consume texture updates.
        output.textures_delta.clear();
        output
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
    #[test]
    fn profile_reload_cancels_pending_hotkey_targets_before_other_tabs_render() {
        for custom_rule in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let saved = Config::default();
            saved.save_to(&path).unwrap();
            let mut state = SharedState::at_path(saved, &path);
            let mut characters = components::characters::CharactersState::new();
            let (mut hotkeys, cancelled) =
                components::hotkey_settings::HotkeySettingsState::pending_capture_for_test(
                    custom_rule,
                );
            assert!(hotkeys.is_dialog_open());
            std::fs::write(&path, b"{broken").unwrap();
            assert!(ProfileSelector::new().reload_config(&mut state).is_err());
            ManagerApp::reset_profile_editors(
                &mut state.profile_editors_reload_pending,
                &mut characters,
                &mut hotkeys,
            );
            assert!(
                hotkeys.is_dialog_open(),
                "failed reload must retain the pending capture"
            );
            assert!(cancelled.try_recv().is_err());
            state.config.save_to(&path).unwrap();
            ProfileSelector::new().reload_config(&mut state).unwrap();
            ManagerApp::reset_profile_editors(
                &mut state.profile_editors_reload_pending,
                &mut characters,
                &mut hotkeys,
            );
            assert!(!hotkeys.is_dialog_open());
            assert!(cancelled.try_recv().is_ok());
            assert!(!state.profile_editors_reload_pending);
        }
    }

    #[test]
    fn config_reload_cancels_pending_group_operations_only_after_success() {
        for readable in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let mut saved = Config::default();
            saved.profiles[0].cycle_groups[0].name = "Restored group".into();
            saved.save_to(&path).unwrap();
            if !readable {
                std::fs::write(&path, b"{broken").unwrap();
            }
            let mut shared = SharedState::at_path(Config::default(), &path);
            let mut editor = components::characters::CharactersState::new();
            editor.show_add_characters_popup = true;
            editor.character_selections.insert(
                crate::config::profile::CycleSlot::Eve("Old profile character".into()),
                true,
            );
            editor.renaming_group_idx = Some(0);
            editor.rename_buffer = "Stale draft".into();
            editor.rename_error = Some("Old error".into());
            assert_eq!(
                ProfileSelector::new().reload_config(&mut shared).is_ok(),
                readable
            );
            let mut hotkeys = components::hotkey_settings::HotkeySettingsState::new();
            ManagerApp::reset_profile_editors(
                &mut shared.profile_editors_reload_pending,
                &mut editor,
                &mut hotkeys,
            );
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                components::characters::ui(
                    ui,
                    &mut shared.config.profiles[0],
                    &mut editor,
                    &mut hotkeys,
                );
            });
            output.textures_delta.clear();
            if readable {
                assert!(!editor.show_add_characters_popup);
                assert!(editor.character_selections.is_empty());
                assert!(
                    shared.config.profiles[0].cycle_groups[0]
                        .cycle_list
                        .is_empty()
                );
                assert!(
                    editor.renaming_group_idx.is_none(),
                    "successful reload must cancel the old rename target"
                );
                assert!(editor.rename_buffer.is_empty() && editor.rename_error.is_none());
                assert_eq!(
                    shared.config.profiles[0].cycle_groups[0].name,
                    "Restored group"
                );
            } else {
                assert!(editor.show_add_characters_popup);
                assert_eq!(editor.character_selections.len(), 1);
                assert_eq!(editor.renaming_group_idx, Some(0));
                assert_eq!(editor.rename_buffer, "Stale draft");
                assert_eq!(editor.rename_error.as_deref(), Some("Old error"));
            }
        }
    }
}
