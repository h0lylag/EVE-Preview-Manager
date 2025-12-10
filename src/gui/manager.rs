//! GUI manager implemented with egui/eframe and ksni system tray support

use std::io::Cursor;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver};
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
#[derive(Debug, Clone, PartialEq, Eq)]
enum TrayMessage {
    Refresh,
    SwitchProfile(usize),
    SavePositions,
    Quit,
}

#[cfg(target_os = "linux")]
struct AppTray {
    tx: std::sync::mpsc::Sender<TrayMessage>,
}

#[cfg(target_os = "linux")]
impl AppTray {
    /// Load current profile state from config file.
    /// Called each time menu is opened to ensure up-to-date state.
    fn load_current_state(&self) -> (usize, Vec<String>) {
        match Config::load() {
            Ok(config) => {
                let profile_names: Vec<String> = config
                    .profiles
                    .iter()
                    .map(|p| p.profile_name.clone())
                    .collect();
                let current_idx = config
                    .profiles
                    .iter()
                    .position(|p| p.profile_name == config.global.selected_profile)
                    .unwrap_or(0);
                (current_idx, profile_names)
            }
            Err(_) => (0, vec!["default".to_string()]),
        }
    }
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

        // Reload config to get current profile state
        let (current_profile_idx, profile_names) = self.load_current_state();

        vec![
            // Refresh item
            StandardItem {
                label: "Refresh".into(),
                activate: Box::new(|this: &mut AppTray| {
                    let _ = this.tx.send(TrayMessage::Refresh);
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
                    let _ = this.tx.send(TrayMessage::SwitchProfile(idx));
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
            // Save Thumbnail Positions (always show - harmless when auto-save is on)
            StandardItem {
                label: "Save Thumbnail Positions".into(),
                activate: Box::new(|this: &mut AppTray| {
                    let _ = this.tx.send(TrayMessage::SavePositions);
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
                    let _ = this.tx.send(TrayMessage::Quit);
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

struct ManagerApp {
    daemon: Option<Child>,
    daemon_status: DaemonStatus,
    last_health_check: Instant,
    status_message: Option<StatusMessage>,
    #[cfg(target_os = "linux")]
    tray_rx: Receiver<TrayMessage>,
    #[cfg(target_os = "linux")]
    shutdown_signal: std::sync::Arc<tokio::sync::Notify>,
    should_quit: bool,

    // Configuration state with profiles
    config: Config,
    selected_profile_idx: usize,
    profile_selector: ProfileSelector,
    behavior_settings_state: components::behavior_settings::BehaviorSettingsState,
    hotkey_settings_state: components::hotkey_settings::HotkeySettingsState,
    visual_settings_state: components::visual_settings::VisualSettingsState,
    cycle_order_settings_state: components::cycle_order_settings::CycleOrderSettingsState,
    settings_changed: bool,
    config_status_message: Option<StatusMessage>,
}

impl ManagerApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        info!("Initializing egui manager");

        // Create channel for tray icon commands
        #[cfg(target_os = "linux")]
        let (tx_to_app, tray_rx) = mpsc::channel();

        // Spawn Tokio thread for ksni tray
        #[cfg(target_os = "linux")]
        let shutdown_signal = std::sync::Arc::new(tokio::sync::Notify::new());
        #[cfg(target_os = "linux")]
        let shutdown_clone = shutdown_signal.clone();

        #[cfg(target_os = "linux")]
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime for tray");

            // Run a dedicated single-threaded Tokio runtime for the tray icon
            // This is required because `ksni` (StatusNotifierItem) relies on async D-Bus communication
            runtime.block_on(async move {
                let tray = AppTray { tx: tx_to_app };

                match tray.spawn().await {
                    Ok(handle) => {
                        info!("Tray icon created via ksni/D-Bus");

                        // Wait for shutdown signal
                        shutdown_clone.notified().await;

                        // Gracefully shutdown tray
                        handle.shutdown().await;
                    }
                    Err(e) => {
                        error!(error = ?e, "Failed to create tray icon (D-Bus unavailable?)");
                    }
                }
            });
        });

        // Load configuration
        let config = Config::load().unwrap_or_default();

        // Find selected profile index
        let selected_profile_idx = config
            .profiles
            .iter()
            .position(|p| p.profile_name == config.global.selected_profile)
            .unwrap_or(0);

        // Initialize component states
        let behavior_settings_state =
            components::behavior_settings::BehaviorSettingsState::default();
        let hotkey_settings_state = components::hotkey_settings::HotkeySettingsState::default();
        let visual_settings_state = components::visual_settings::VisualSettingsState::default();

        // Initialize cycle order settings state with current profile
        let mut cycle_order_settings_state =
            components::cycle_order_settings::CycleOrderSettingsState::default();
        cycle_order_settings_state.load_from_profile(&config.profiles[selected_profile_idx]);

        #[cfg(target_os = "linux")]
        let mut app = Self {
            daemon: None,
            daemon_status: DaemonStatus::Stopped,
            last_health_check: Instant::now(),
            status_message: None,
            tray_rx,
            shutdown_signal,
            should_quit: false,
            config,
            selected_profile_idx,
            profile_selector: ProfileSelector::new(),
            behavior_settings_state,
            hotkey_settings_state,
            visual_settings_state,
            cycle_order_settings_state,
            settings_changed: false,
            config_status_message: None,
        };

        #[cfg(not(target_os = "linux"))]
        let mut app = Self {
            daemon: None,
            daemon_status: DaemonStatus::Stopped,
            last_health_check: Instant::now(),
            status_message: None,
            should_quit: false,
            config,
            selected_profile_idx,
            profile_selector: ProfileSelector::new(),
            behavior_settings_state,
            hotkey_settings_state,
            visual_settings_state,
            cycle_order_settings_state,
            settings_changed: false,
            config_status_message: None,
        };

        if let Err(err) = app.start_daemon() {
            error!(error = ?err, "Failed to start preview daemon");
            app.status_message = Some(StatusMessage {
                text: format!("Failed to start daemon: {err}"),
                color: STATUS_STOPPED,
            });
        }

        app
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
        info!("Restart requested from UI");
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
        // This ensures that:
        // 1. Deletions in GUI are respected (since we start with GUI state)
        // 2. Window positions updated by the daemon (which writes to disk) are preserved
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
                    } else {
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

        // Reload config to include daemon's new characters in GUI memory
        self.config = Config::load().unwrap_or(final_config);

        self.settings_changed = false;
        self.config_status_message = Some(StatusMessage {
            text: "Configuration saved successfully".to_string(),
            color: COLOR_SUCCESS,
        });
        info!("Configuration saved to disk");
        Ok(())
    }

    fn save_thumbnail_positions(&mut self) -> Result<()> {
        // If we have a running daemon, send SIGUSR1 signal to trigger save
        if let Some(ref daemon) = self.daemon {
            let pid = daemon.id();
            info!(
                daemon_pid = pid,
                "Sending SIGUSR1 to daemon to save positions"
            );

            #[cfg(target_os = "linux")]
            {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;

                signal::kill(Pid::from_raw(pid as i32), Signal::SIGUSR1)
                    .context("Failed to send SIGUSR1 to daemon")?;

                info!("SIGUSR1 sent successfully");
            }

            #[cfg(not(target_os = "linux"))]
            {
                anyhow::bail!("Signal-based save only supported on Linux");
            }

            return Ok(());
        }

        // Fallback: no daemon running (shouldn't happen in normal use)
        // In this case we don't have reliable position data, so just return error
        anyhow::bail!("Cannot save positions: daemon is not running. Start the daemon first.")
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
                        if !gui_profile.character_thumbnails.contains_key(char_name) {
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

    fn poll_daemon(&mut self) {
        if self.last_health_check.elapsed() < Duration::from_millis(DAEMON_CHECK_INTERVAL_MS) {
            return;
        }
        self.last_health_check = Instant::now();

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
                        // Reload config when daemon transitions to running to pick up any new characters
                        self.reload_character_list();
                    }
                }
                Err(err) => {
                    error!(error = ?err, "Failed to query daemon status");
                }
            }
        }
    }

    fn poll_tray_events(&mut self) {
        #[cfg(target_os = "linux")]
        while let Ok(msg) = self.tray_rx.try_recv() {
            match msg {
                TrayMessage::Refresh => {
                    info!("Refresh requested from tray menu");
                    self.reload_daemon_config();
                }
                TrayMessage::SwitchProfile(idx) => {
                    info!(profile_idx = idx, "Profile switch requested from tray");

                    // Update config's selected_profile field
                    if idx < self.config.profiles.len() {
                        self.config.global.selected_profile =
                            self.config.profiles[idx].profile_name.clone();
                        self.selected_profile_idx = idx;

                        // Clear any pending selection in the GUI profile selector
                        self.profile_selector.clear_pending();

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
                TrayMessage::SavePositions => {
                    info!("Save positions requested from tray menu");
                    if let Err(err) = self.save_thumbnail_positions() {
                        error!(error = ?err, "Failed to save thumbnail positions");
                        self.status_message = Some(StatusMessage {
                            text: format!("Failed to save positions: {err}"),
                            color: STATUS_STOPPED,
                        });
                    } else {
                        self.status_message = Some(StatusMessage {
                            text: "Thumbnail positions saved".to_string(),
                            color: STATUS_RUNNING,
                        });
                    }
                }
                TrayMessage::Quit => {
                    info!("Quit requested from tray menu");
                    self.should_quit = true;
                }
            }
        }
    }

    fn render_unified_settings(&mut self, ui: &mut egui::Ui) {
        // Row 1: Profile dropdown group + Save/Discard buttons
        let mut action = ui
            .horizontal(|ui| {
                // Profile dropdown group
                let action = self.profile_selector.render_dropdown(
                    ui,
                    &mut self.config,
                    &mut self.selected_profile_idx,
                );

                // Save/Discard buttons aligned to the right
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Discard button
                    if ui.button("✖ Discard Changes").clicked() {
                        self.discard_changes();
                    }

                    // Save button
                    if ui.button("💾 Save & Apply").clicked() {
                        if let Err(err) = self.save_config() {
                            error!(error = ?err, "Failed to save config");
                            self.status_message = Some(StatusMessage {
                                text: format!("Save failed: {err}"),
                                color: COLOR_ERROR,
                            });
                        } else {
                            self.reload_daemon_config();
                        }
                    }
                });

                action
            })
            .inner;

        ui.add_space(ITEM_SPACING);

        // Row 2: Profile management buttons on left, status text on right
        ui.horizontal(|ui| {
            self.profile_selector
                .render_buttons(ui, &self.config, self.selected_profile_idx);

            // Status text aligned to the right
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(message) = &self.config_status_message {
                    ui.colored_label(message.color, &message.text);
                } else if self.settings_changed {
                    ui.colored_label(COLOR_WARNING, "Unsaved changes");
                }
            });
        });

        // Render modal dialogs (must be called at context level, not inside layout)
        let dialog_action = self.profile_selector.render_dialogs(
            ui.ctx(),
            &mut self.config,
            &mut self.selected_profile_idx,
        );

        // Merge dialog action with dropdown action
        if !matches!(dialog_action, ProfileAction::None) {
            action = dialog_action;
        }

        match action {
            ProfileAction::SwitchProfile => {
                // Load cycle order text when switching profiles
                let current_profile = &self.config.profiles[self.selected_profile_idx];
                self.cycle_order_settings_state
                    .load_from_profile(current_profile);

                // Save config and reload daemon
                if let Err(err) = self.save_config() {
                    error!(error = ?err, "Failed to save config after profile switch");
                    self.status_message = Some(StatusMessage {
                        text: format!("Save failed: {err}"),
                        color: COLOR_ERROR,
                    });
                } else {
                    self.reload_daemon_config();
                }
            }
            ProfileAction::ProfileCreated
            | ProfileAction::ProfileDeleted
            | ProfileAction::ProfileUpdated => {
                // Save config and reload daemon
                if let Err(err) = self.save_config() {
                    error!(error = ?err, "Failed to save config after profile action");
                    self.status_message = Some(StatusMessage {
                        text: format!("Save failed: {err}"),
                        color: COLOR_ERROR,
                    });
                } else {
                    self.reload_daemon_config();
                }
            }
            ProfileAction::None => {}
        }

        ui.add_space(SECTION_SPACING);
        ui.separator();
        ui.add_space(SECTION_SPACING);

        // 3-column layout: Behavior Settings | Visual+Hotkey Settings | Character Cycle Order
        let current_profile = &mut self.config.profiles[self.selected_profile_idx];

        ui.columns(3, |columns| {
            // Column 1: Behavior Settings
            if components::behavior_settings::ui(
                &mut columns[0],
                current_profile,
                &mut self.behavior_settings_state,
            ) {
                self.settings_changed = true;
                self.config_status_message = None;
            }

            // Column 2: Visual Settings + Hotkey Settings
            if components::visual_settings::ui(
                &mut columns[1],
                current_profile,
                &mut self.visual_settings_state,
            ) {
                self.settings_changed = true;
                self.config_status_message = None;
            }
            columns[1].add_space(SECTION_SPACING);
            if components::hotkey_settings::ui(
                &mut columns[1],
                current_profile,
                &mut self.hotkey_settings_state,
            ) {
                self.settings_changed = true;
                self.config_status_message = None;
            }

            // Column 3: Character Cycle Order & Per-Character Hotkeys
            if components::cycle_order_settings::ui(
                &mut columns[2],
                current_profile,
                &mut self.cycle_order_settings_state,
                &mut self.hotkey_settings_state,
            ) {
                self.settings_changed = true;
                self.config_status_message = None;
            }
        });
    }
}

impl eframe::App for ManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_daemon();
        self.poll_tray_events();

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
            && (new_width != self.config.global.window_width
                || new_height != self.config.global.window_height)
        {
            self.config.global.window_width = new_width;
            self.config.global.window_height = new_height;
        }

        // Request repaint after short delay to poll for tray events even when unfocused
        // Request repaint after short delay to poll for tray events even when unfocused
        // This ensures tray menu actions are processed promptly without burning CPU in a tight loop
        ctx.request_repaint_after(std::time::Duration::from_millis(100));

        // Handle quit request from tray menu
        if self.should_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Slim status bar at top
            ui.horizontal(|ui| {
                ui.colored_label(self.daemon_status.color(), self.daemon_status.label());
                if let Some(child) = &self.daemon {
                    ui.label(format!("(PID: {})", child.id()));
                }
                if let Some(message) = &self.status_message {
                    ui.add_space(10.0);
                    ui.colored_label(message.color, &message.text);
                }
            });

            ui.separator();
            ui.add_space(SECTION_SPACING);

            // Unified Settings Content (3-column layout)
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.render_unified_settings(ui);
            });
        });

        ctx.request_repaint_after(Duration::from_millis(DAEMON_CHECK_INTERVAL_MS));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Err(err) = self.stop_daemon() {
            error!(error = ?err, "Failed to stop daemon during shutdown");
        }

        // Save window geometry on exit (avoids flooding disk during resize drag)
        // Use simple config save since daemon is stopped (no merge needed)
        if let Err(err) = self
            .config
            .save_with_strategy(SaveStrategy::PreserveCharacterPositions)
        {
            error!(error = ?err, "Failed to save window geometry on exit");
        } else {
            info!("Window geometry saved on exit");
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
        Box::new(|cc| Ok(Box::new(ManagerApp::new(cc)))),
    )
    .map_err(|err| anyhow!("Failed to launch egui manager: {err}"))
}
