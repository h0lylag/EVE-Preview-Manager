#![allow(clippy::collapsible_if)]
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use crate::manager::{state::SharedState, utils::load_tray_icon_pixmap};

/// System tray icon integration handling menu events and status updates
#[cfg(target_os = "linux")]
pub struct AppTray {
    pub state: Arc<Mutex<SharedState>>,
    pub is_flatpak: bool,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for AppTray {
    fn id(&self) -> String {
        if self.is_flatpak {
            "com.evepreview.manager".into()
        } else {
            "eve-preview-manager".into()
        }
    }

    fn icon_name(&self) -> String {
        if self.is_flatpak {
            "com.evepreview.manager".into()
        } else {
            "eve-preview-manager".into()
        }
    }

    fn title(&self) -> String {
        "EVE Preview Manager".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        load_tray_icon_pixmap()
            .map(|icon| vec![icon])
            .unwrap_or_default()
    }
    
    // Double-click to open window
    fn activate(&mut self, _x: i32, _y: i32) {
        // Use Slint's invoke_from_event_loop to show window from tray thread
        let _ = slint::invoke_from_event_loop(|| {
            crate::manager::app_slint::show_main_window_from_tray();
        });
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
            // Open window item
            StandardItem {
                label: "Open".into(),
                activate: Box::new(|_this: &mut AppTray| {
                    let _ = slint::invoke_from_event_loop(|| {
                        crate::manager::app_slint::show_main_window_from_tray();
                    });
                }),
                ..Default::default()
            }
            .into(),
            // Separator
            MenuItem::Separator,
            // Refresh item
            StandardItem {
                label: "Refresh".into(),
                activate: Box::new(|this: &mut AppTray| {
                    if let Ok(mut state) = this.state.lock() {
                        state.reload_daemon_config();
                    }
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
                    // Quit the Slint event loop
                    let _ = slint::invoke_from_event_loop(|| {
                        let _ = slint::quit_event_loop();
                    });
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
