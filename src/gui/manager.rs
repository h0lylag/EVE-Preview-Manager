//! GUI manager implemented with egui/eframe and ksni system tray support

use std::io::Cursor;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use eframe::{NativeOptions, egui};
use tracing::{error, info, warn};

#[cfg(target_os = "linux")]
use ksni::TrayMethods;

use super::components;
use crate::config::profile::{Config, SaveStrategy};
use crate::constants::gui::*;
use crate::gui::components::profile_selector::{ProfileAction, ProfileSelector};

#[cfg(target_os = "linux")]
struct AppTray {
    state: Arc<Mutex<SharedState>>,
    ctx: egui::Context,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for AppTray {
    fn id(&self) -> String {
        "eve-preview-manager".into()
    }

    fn title(&self) -> String {
        "EVE Preview Manager".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        load_tray_icon_pixmap()
            .map(|icon| vec![icon])
            .unwrap_or_default()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        // Lock state to get current info
        let (current_profile_idx, profile_names) = {
            if let Ok(state) = self.state.lock() {
                let profile_names: Vec<String> = state
                    .config
                    .profiles
                    .iter()
                    .map(|p| p.profile_name.clone())
                    .collect();
                let idx = state.selected_profile_idx;
                (idx, profile_names)
            } else {
                (0, vec!["default".to_string()])
            }
        };

        vec![
            // Refresh item
            StandardItem {
                label: "Refresh".into(),
                activate: Box::new(|this: &mut AppTray| {
                    if let Ok(mut state) = this.state.lock() {
                        state.reload_daemon_config();
                    }
                    this.ctx.request_repaint();
                }),
                ..Default::default()
            }
            .into(),
            // Separator
            MenuItem::Separator,
            // Profile selector (radio group)
            RadioGroup {
                selected: current_profile_idx,
                select: Box::new(|this: &mut AppTray, idx| {
                    if let Ok(mut state) = this.state.lock() {
                        state.switch_profile(idx);
                    }
                    this.ctx.request_repaint();
                }),
                options: profile_names
                    .iter()
                    .map(|name| RadioItem {
                        label: name.clone(),
                        ..Default::default()
                    })
                    .collect(),
            }
            .into(),
            // Separator
            MenuItem::Separator,
            // Save Thumbnail Positions
            StandardItem {
                label: "Save Thumbnail Positions".into(),
                activate: Box::new(|this: &mut AppTray| {
                    if let Ok(mut state) = this.state.lock() {
                        let _ = state.save_thumbnail_positions();
                    }
                    this.ctx.request_repaint();
                }),
                ..Default::default()
            }
            .into(),
            // Separator
            MenuItem::Separator,
            // Quit item
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut AppTray| {
                    if let Ok(mut state) = this.state.lock() {
                        state.should_quit = true;
                    }
                    this.ctx.request_repaint();
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonStatus {
    Starting,
    Running,
    Stopped,
    Crashed(Option<i32>),
}

impl DaemonStatus {
    fn color(&self) -> egui::Color32 {
        match self {
            DaemonStatus::Running => STATUS_RUNNING,
            DaemonStatus::Starting => STATUS_STARTING,
            _ => STATUS_STOPPED,
        }
    }

    fn label(&self) -> String {
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

struct StatusMessage {
    text: String,
    color: egui::Color32,
}

// Core application state shared between GUI and Tray
struct SharedState {
    config: Config,
    daemon: Option<Child>,
    daemon_status: DaemonStatus,
    last_health_check: Instant,
    status_message: Option<StatusMessage>,
    config_status_message: Option<StatusMessage>,
    settings_changed: bool,
    selected_profile_idx: usize,
    should_quit: bool,
    last_config_mtime: Option<std::time::SystemTime>,
}

impl SharedState {
    fn new(config: Config) -> Self {
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

    fn start_daemon(&mut self) -> Result<()> {
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

    fn stop_daemon(&mut self) -> Result<()> {
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

    fn restart_daemon(&mut self) {
        info!("Restart requested");
        if let Err(err) = self.stop_daemon().and_then(|_| self.start_daemon()) {
            error!(error = ?err, "Failed to restart daemon");
            self.status_message = Some(StatusMessage {
                text: format!("Restart failed: {err}"),
                color: STATUS_STOPPED,
            });
        }
    }

    fn reload_daemon_config(&mut self) {
        info!("Config reload requested - restarting daemon");
        self.restart_daemon();
    }

    fn save_config(&mut self) -> Result<()> {
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
                    } else if !char_name.is_empty() {
                        // Character only in disk (daemon added it): preserve it completely
                        merged_profile
                            .character_thumbnails
                            .insert(char_name.clone(), *disk_settings);
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
        final_config
            .save_with_strategy(SaveStrategy::OverwriteCharacterPositions)
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

    fn switch_profile(&mut self, idx: usize) {
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

    fn discard_changes(&mut self) {
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

    fn reload_character_list(&mut self) {
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
                                .insert(char_name.clone(), *char_settings);
                            info!(character = %char_name, profile = %gui_profile.profile_name, "Detected new character from daemon");
                        }
                    }
                }
            }
        }
    }

    fn save_thumbnail_positions(&mut self) -> Result<()> {
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

    fn poll_daemon(&mut self) {
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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum GuiTab {
    General,
    Advanced,
}

struct ManagerApp {
    state: Arc<Mutex<SharedState>>,

    // UI-only state (doesn't need to be shared deeply)
    profile_selector: ProfileSelector,
    behavior_settings_state: components::behavior_settings::BehaviorSettingsState,
    hotkey_settings_state: components::hotkey_settings::HotkeySettingsState,
    visual_settings_state: components::visual_settings::VisualSettingsState,
    cycle_order_settings_state: components::cycle_order_settings::CycleOrderSettingsState,
    #[cfg(target_os = "linux")]
    shutdown_signal: std::sync::Arc<tokio::sync::Notify>,
    #[cfg(target_os = "linux")]
    update_signal: std::sync::Arc<tokio::sync::Notify>,

    active_tab: GuiTab,
}

impl ManagerApp {
    fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        info!("Initializing egui manager");

        // Initialize SharedState
        let mut state = SharedState::new(config.clone());
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
                let tray = AppTray {
                    state: state_clone,
                    ctx,
                };

                match tray.spawn().await {
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

        let mut cycle_order_settings_state =
            components::cycle_order_settings::CycleOrderSettingsState::default();
        cycle_order_settings_state.load_from_profile(&config.profiles[selected_profile_idx]);

        #[cfg(target_os = "linux")]
        let app = Self {
            state,
            shutdown_signal,
            update_signal,
            profile_selector: ProfileSelector::new(),
            behavior_settings_state,
            hotkey_settings_state,
            visual_settings_state,
            cycle_order_settings_state,
            active_tab: GuiTab::General,
        };

        #[cfg(not(target_os = "linux"))]
        let app = Self {
            state,
            profile_selector: ProfileSelector::new(),
            behavior_settings_state,
            hotkey_settings_state,
            visual_settings_state,
            cycle_order_settings_state,
            active_tab: GuiTab::General,
        };

        app
    }

    fn render_general_settings_columns(&mut self, ui: &mut egui::Ui, state: &mut SharedState) {
        let current_profile = &mut state.config.profiles[state.selected_profile_idx];

        ui.columns(3, |columns| {
            // Column 1: Behavior Settings
            if components::behavior_settings::ui(
                &mut columns[0],
                current_profile,
                &mut self.behavior_settings_state,
            ) {
                state.settings_changed = true;
                state.config_status_message = None;
            }

            // Column 2: Visual Settings + Hotkey Settings
            if components::visual_settings::ui(
                &mut columns[1],
                current_profile,
                &mut self.visual_settings_state,
            ) {
                state.settings_changed = true;
                state.config_status_message = None;
            }
            columns[1].add_space(SECTION_SPACING);
            if components::hotkey_settings::ui(
                &mut columns[1],
                current_profile,
                &mut self.hotkey_settings_state,
            ) {
                state.settings_changed = true;
                state.config_status_message = None;
            }

            // Column 3: Character Cycle Order & Per-Character Hotkeys
            if components::cycle_order_settings::ui(
                &mut columns[2],
                current_profile,
                &mut self.cycle_order_settings_state,
                &mut self.hotkey_settings_state,
            ) {
                state.settings_changed = true;
                state.config_status_message = None;
            }
        });
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
        let mut state = &mut *state_guard;

        state.poll_daemon();

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
        // Handle quit request from tray menu
        if state.should_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        let mut action = ProfileAction::None;

        // Global Header Panel (Fixed at top)
        // We use a TopBottomPanel to ensure this section remains visible while the content below scrolls.
        egui::TopBottomPanel::top("global_header").show(ctx, |ui| {
            // Row 0: Daemon Status (Left) | Tabs (Right)
            ui.horizontal(|ui| {
                // Left side: Status indicators
                ui.colored_label(state.daemon_status.color(), state.daemon_status.label());
                if let Some(child) = &state.daemon {
                    ui.label(format!("(PID: {})", child.id()));
                }
                if let Some(message) = &state.status_message {
                    ui.add_space(10.0);
                    ui.colored_label(message.color, &message.text);
                }

                // Right side: Navigation Tabs
                // We use right_to_left layout to anchor the tabs to the top-right corner,
                // keeping them distinct from the profile controls below.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(5.0); // Padding from right edge
                    if ui.add(egui::Button::new("Advanced").selected(self.active_tab == GuiTab::Advanced)).clicked() {
                        self.active_tab = GuiTab::Advanced;
                    }
                    ui.add_space(5.0);
                    if ui.add(egui::Button::new("General").selected(self.active_tab == GuiTab::General)).clicked() {
                        self.active_tab = GuiTab::General;
                    }
                });
            });
            ui.separator();

            // Row 1: Control Bar - Profile Selector (Left) | Save Actions (Right)
            ui.horizontal(|ui| {
                // Ensure the row has enough height for standard buttons/dropdowns
                ui.set_min_height(30.0);
                
                // 1. Left: Profile Dropdown
                action = self.profile_selector.render_dropdown(
                    ui,
                    &mut state.config,
                    &mut state.selected_profile_idx,
                );

                // 2. Right: Save & Discard Buttons
                // Anchored right to keep the primary actions easily accessible
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Discard button
                    if ui.button("✖ Discard Changes").clicked() {
                        state.discard_changes();
                    }

                    // Save button
                    if ui.button("💾 Save & Apply").clicked() {
                        if let Err(err) = state.save_config() {
                            error!(error = ?err, "Failed to save config");
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
                });
            });

            // Row 2: Profile Actions | Config Status
            ui.horizontal(|ui| {
                self.profile_selector
                    .render_buttons(ui, &state.config, state.selected_profile_idx);

                // Status text aligned to the right
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(message) = &state.config_status_message {
                        ui.colored_label(message.color, &message.text);
                    } else if state.settings_changed {
                        ui.colored_label(COLOR_WARNING, "Unsaved changes");
                    }
                });
            });
            
            ui.add_space(5.0);
        });
        
        // Handle Dialogs (Context level)
        let dialog_action = self.profile_selector.render_dialogs(
            ctx,
            &mut state.config,
            &mut state.selected_profile_idx,
        );

        if !matches!(dialog_action, ProfileAction::None) {
            action = dialog_action;
        }

        // Handle Actions
        match action {
            ProfileAction::SwitchProfile => {
                let current_profile = &state.config.profiles[state.selected_profile_idx];
                self.cycle_order_settings_state.load_from_profile(current_profile);

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
            ProfileAction::ProfileCreated | ProfileAction::ProfileDeleted | ProfileAction::ProfileUpdated => {
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
                match self.active_tab {
                    GuiTab::General => {
                        self.render_general_settings_columns(ui, &mut state);
                    }
                    GuiTab::Advanced => {
                        ui.vertical_centered(|ui| {
                            ui.add_space(50.0);
                            ui.label("Advanced settings coming soon...");
                        });
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

fn spawn_preview_daemon() -> Result<Child> {
    let exe_path = std::env::current_exe().context("Failed to resolve executable path")?;
    Command::new(exe_path)
        .arg("--preview")
        .spawn()
        .context("Failed to spawn preview daemon")
}

#[cfg(target_os = "linux")]
fn load_tray_icon_pixmap() -> Result<ksni::Icon> {
    let icon_bytes = include_bytes!("../../assets/icon.png");
    let decoder = png::Decoder::new(Cursor::new(icon_bytes));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .context("PNG has no output buffer size")?
    ];
    let info = reader.next_frame(&mut buf)?;
    let rgba = &buf[..info.buffer_size()];

    // Convert RGBA to ARGB for ksni
    let argb: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => {
            rgba.chunks_exact(4)
                .flat_map(|chunk| [chunk[3], chunk[0], chunk[1], chunk[2]]) // RGBA → ARGB
                .collect()
        }
        png::ColorType::Rgb => {
            rgba.chunks_exact(3)
                .flat_map(|chunk| [0xFF, chunk[0], chunk[1], chunk[2]]) // RGB → ARGB (full alpha)
                .collect()
        }
        other => {
            return Err(anyhow!(
                "Unsupported icon color type {:?} (expected RGB or RGBA)",
                other
            ));
        }
    };

    Ok(ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data: argb,
    })
}

/// Load window icon from embedded PNG (same as tray icon)
#[cfg(target_os = "linux")]
fn load_window_icon() -> Result<egui::IconData> {
    let icon_bytes = include_bytes!("../../assets/icon.png");
    let decoder = png::Decoder::new(Cursor::new(icon_bytes));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .context("PNG has no output buffer size")?
    ];
    let info = reader.next_frame(&mut buf)?;
    let rgba = &buf[..info.buffer_size()];

    // egui IconData expects RGBA format
    let rgba_vec = match info.color_type {
        png::ColorType::Rgba => rgba.to_vec(),
        png::ColorType::Rgb => {
            // Convert RGB to RGBA
            let mut rgba_data = Vec::with_capacity(rgba.len() / 3 * 4);
            for chunk in rgba.chunks_exact(3) {
                rgba_data.extend_from_slice(chunk);
                rgba_data.push(0xFF); // Add full alpha
            }
            rgba_data
        }
        other => {
            return Err(anyhow!(
                "Unsupported window icon color type {:?} (expected RGB or RGBA)",
                other
            ));
        }
    };

    Ok(egui::IconData {
        rgba: rgba_vec,
        width: info.width,
        height: info.height,
    })
}

pub fn run_gui() -> Result<()> {
    // Load config to get window dimensions
    let config = Config::load().unwrap_or_default();
    let window_width = config.global.window_width as f32;
    let window_height = config.global.window_height as f32;

    #[cfg(target_os = "linux")]
    let icon = match load_window_icon() {
        Ok(icon_data) => {
            info!(
                "Loaded window icon ({} bytes, {}x{})",
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
        .with_min_inner_size([WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT])
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
        Box::new(|cc| Ok(Box::new(ManagerApp::new(cc, config)))),
    )
    .map_err(|err| anyhow!("Failed to launch egui manager: {err}"))
}
