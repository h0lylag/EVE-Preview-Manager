#![allow(clippy::collapsible_if)]
#[cfg(target_os = "linux")]
use eframe::egui;
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use crate::gui::{state::SharedState, utils::load_tray_icon_pixmap};

/// System tray icon integration handling menu events and status updates
#[cfg(target_os = "linux")]
pub struct AppTray {
    pub state: Arc<Mutex<SharedState>>,
    pub ctx: egui::Context,
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
                        if let Err(e) = state.save_thumbnail_positions() {
                            tracing::error!("Failed to save thumbnail positions: {}", e);
                        }
                    }
                }),
                ..Default::default()
            }
            .into(),
            // Separator
            MenuItem::Separator,
            // Quit item
            StandardItem {
                label: "Quit".into(),
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
