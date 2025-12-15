//! GUI manager implemented with egui/eframe and ksni system tray support

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use eframe::{NativeOptions, egui};
use tracing::{error, info};

#[cfg(target_os = "linux")]
use ksni::TrayMethods;

use super::components;
use crate::config::profile::Config;
use crate::constants::gui::*;
use crate::gui::components::profile_selector::{ProfileAction, ProfileSelector};
#[cfg(target_os = "linux")]
use crate::gui::components::tray::AppTray;
use crate::gui::state::{GuiTab, SharedState, StatusMessage};
use crate::gui::utils::load_window_icon;

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
        let state = &mut *state_guard;

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
                self.cycle_order_settings_state
                    .load_from_profile(current_profile);

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
            egui::ScrollArea::vertical().show(ui, |ui| match self.active_tab {
                GuiTab::General => {
                    self.render_general_settings_columns(ui, state);
                }
                GuiTab::Advanced => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        ui.label("Advanced settings coming soon...");
                    });
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
