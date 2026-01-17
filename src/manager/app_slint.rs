//! Slint-based application manager - primary interface for configuration and daemon control

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use tracing::{debug, error, info, warn};

#[cfg(target_os = "linux")]
use ksni::TrayMethods;

use crate::common::constants::manager_ui::*;
use crate::config::backup::BackupManager;
use crate::config::profile::{Config, GlobalSettings};
use crate::manager::state::{SharedState, StatusMessage};

// Include the generated Slint components
slint::include_modules!();

// Thread-local storage for the main window (following SLINT_KSNI_INTEGRATION.md pattern)
thread_local! {
    static MAIN_WINDOW: RefCell<Option<MainWindow>> = RefCell::new(None);
    static SHARED_STATE: RefCell<Option<Arc<Mutex<SharedState>>>> = RefCell::new(None);
    static STATUS_TIMER: RefCell<Option<slint::Timer>> = RefCell::new(None);
}

/// Show the main window (create-on-demand pattern)
pub fn show_main_window() {
    MAIN_WINDOW.with(|window_cell| {
        let mut window_opt = window_cell.borrow_mut();
        
        if window_opt.is_none() {
            let window = MainWindow::new().unwrap();
            
            // Setup all callbacks
            setup_callbacks(&window);
            
            // Handle OS close request (X button) - minimize to tray
            window.window().on_close_requested(move || {
                slint::invoke_from_event_loop(hide_main_window).unwrap();
                slint::CloseRequestResponse::HideWindow
            });
            
            // Load current state into window
            update_window_from_state(&window);

            *window_opt = Some(window);
        }

        if let Some(window) = window_opt.as_ref() {
            window.show().unwrap();
        }
    });
}

/// Public wrapper for tray to show window (marshals to event loop)
pub fn show_main_window_from_tray() {
    show_main_window();
}

/// Hide the main window (destroy-on-hide pattern)
fn hide_main_window() {
    MAIN_WINDOW.with(|window_cell| {
        if let Some(window) = window_cell.borrow_mut().take() {
            let _ = window.hide();
            // Window is dropped here, fully destroyed
        }
    });
}

/// Setup all window callbacks
fn setup_callbacks(window: &MainWindow) {
    // Profile management callbacks
    {
        let window_weak = window.as_weak();
        window.on_load_profile(move |index| {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                            state.selected_profile_idx = index as usize;
                            state.config.global.selected_profile = 
                                state.config.profiles[index as usize].profile_name.clone();
                            
                            // TODO: Reload character state, save config, reload daemon
                            debug!("Loaded profile index: {}", index);
                        }
                    }
                });
                // Refresh window with new profile data
                update_window_from_state(&window);
            }
        });
    }
    
    // Save/Discard callbacks
    {
        let window_weak = window.as_weak();
        window.on_save_config(move || {
            if let Some(_window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                            // Sync UI state back to config before saving
                            // Appearance settings
                            if let Some(profile) = state.config.get_active_profile_mut() {
                                profile.thumbnail_enabled = _window.get_appearance_thumbnail_enabled();
                                profile.thumbnail_opacity = _window.get_appearance_thumbnail_opacity() as u8;
                                profile.thumbnail_active_border = _window.get_appearance_active_border_enabled();
                                profile.thumbnail_active_border_size = _window.get_appearance_active_border_size() as u16;
                                profile.thumbnail_active_border_color = _window.get_appearance_active_border_color().to_string();
                                profile.thumbnail_inactive_border = _window.get_appearance_inactive_border_enabled();
                                profile.thumbnail_inactive_border_size = _window.get_appearance_inactive_border_size() as u16;
                                profile.thumbnail_inactive_border_color = _window.get_appearance_inactive_border_color().to_string();
                                profile.client_minimize_show_overlay = _window.get_appearance_show_overlay();
                                
                                // Behavior settings
                                profile.thumbnail_auto_save_position = _window.get_behavior_auto_save_position();
                                profile.thumbnail_snap_threshold = _window.get_behavior_snap_threshold() as u16;
                                profile.thumbnail_hide_not_focused = _window.get_behavior_hide_not_focused();
                                profile.thumbnail_preserve_position_on_swap = _window.get_behavior_preserve_position();
                                profile.thumbnail_new_clients_inherit_position = _window.get_behavior_inherit_position();
                                profile.client_minimize_on_switch = _window.get_behavior_minimize_on_switch();
                                profile.hotkey_require_eve_focus = _window.get_hotkey_require_eve_focus();
                                profile.hotkey_logged_out_cycle = _window.get_behavior_cycle_logged_out();
                                profile.hotkey_logged_out_cycle = _window.get_behavior_cycle_logged_out();
                                profile.hotkey_cycle_reset_index = _window.get_behavior_reset_index();
                                
                                // Thumbnail Text Settings
                                profile.thumbnail_text_size = _window.get_appearance_thumbnail_text_size() as u16;
                                profile.thumbnail_text_color = _window.get_appearance_thumbnail_text_color().to_string();
                                profile.thumbnail_text_x = _window.get_appearance_thumbnail_text_x() as i16;
                                profile.thumbnail_text_y = _window.get_appearance_thumbnail_text_y() as i16;
                                profile.thumbnail_text_font = _window.get_appearance_thumbnail_text_font().to_string();
                                
                                // Hotkey Backend
                                let backend_str = _window.get_hotkey_backend().to_string();
                                profile.hotkey_backend = match backend_str.as_str() {
                                    "Evdev" => crate::config::profile::HotkeyBackendType::Evdev,
                                    _ => crate::config::profile::HotkeyBackendType::X11,
                                };
                            }
                            
                            // Global Backup Settings
                            state.config.global.backup_enabled = _window.get_backup_enabled();
                            state.config.global.backup_interval_days = _window.get_backup_interval() as u32;
                            state.config.global.backup_retention_count = _window.get_backup_retention() as u32;

                            // Save to disk
                            match state.save_config(crate::manager::state::core::SaveMode::Explicit) {
                                Ok(_) => debug!("Config saved successfully"),
                                Err(e) => error!("Failed to save config: {}", e),
                            }
                            
                            // Mark as clean in UI
                            _window.set_has_unsaved_changes(false);
                        }
                    }
                });
            }
        });
    }
    
    {
        let window_weak = window.as_weak();
        window.on_discard_changes(move || {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                            state.discard_changes();
                            debug!("Discard changes requested");
                            window.set_has_unsaved_changes(false);
                            update_window_from_state(&window);
                        }
                    }
                });
            }
        });
    }
    
    // Profile action callbacks
    {
        let window_weak = window.as_weak();
        window.on_new_profile(move || {
            if let Some(window) = window_weak.upgrade() {
                 window.set_edit_profile_name("".into());
                 window.set_edit_profile_desc("".into());
                 window.set_show_new_profile_dialog(true);
            }
        });
    }
    
    {
        let window_weak = window.as_weak();
        window.on_duplicate_profile(move || {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(state) = state_arc.lock() {
                            let index = window.get_selected_profile_index();
                            if let Some(profile) = state.config.profiles.get(index as usize) {
                                let current_name = &profile.profile_name;
                                let current_desc = &profile.profile_description;
                                window.set_edit_profile_name(format!("{} - Copy", current_name).into());
                                window.set_edit_profile_desc(current_desc.clone().into());
                                window.set_show_duplicate_profile_dialog(true);
                            }
                        }
                    }
                });
            }
        });
    }
    
    {
        let window_weak = window.as_weak();
        window.on_edit_profile(move || {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(state) = state_arc.lock() {
                            let index = window.get_selected_profile_index();
                            if let Some(profile) = state.config.profiles.get(index as usize) {
                                let current_name = &profile.profile_name;
                                let current_desc = &profile.profile_description;
                                window.set_edit_profile_name(current_name.into());
                                window.set_edit_profile_desc(current_desc.clone().into());
                                window.set_show_edit_profile_dialog(true);
                            }
                        }
                    }
                });
            }
        });
    }
    
    {
        let window_weak = window.as_weak();
        window.on_delete_profile(move || {
            if let Some(window) = window_weak.upgrade() {
                 // confirm deletion of currently selected profile
                 window.set_show_delete_profile_confirm(true);
            }
        });
    }

    {
        let window_weak = window.as_weak();
        window.on_cancel_profile_dialog(move || {
            if let Some(window) = window_weak.upgrade() {
                window.set_show_new_profile_dialog(false);
                window.set_show_duplicate_profile_dialog(false);
                window.set_show_edit_profile_dialog(false);
                window.set_show_delete_profile_confirm(false);
            }
        });
    }

    {
        let window_weak = window.as_weak();
        window.on_create_profile(move |name, desc| {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                     if let Some(state_arc) = state_cell.borrow().as_ref() {
                         if let Ok(mut state) = state_arc.lock() {
                             let name_str = name.to_string();
                             let desc_str = desc.to_string();
                             
                             // Check existence
                             if state.config.profiles.iter().any(|p| p.profile_name == name_str) {
                                 // TODO: Show error?
                                  error!("Profile already exists: {}", name_str);
                                  return;
                             }
                             
                             // Create dummy profile (cloning default or creating fresh)
                             let mut new_profile = crate::config::profile::Profile::default();
                             new_profile.profile_name = name_str.clone();
                             new_profile.profile_description = desc_str;
                             
                             state.config.profiles.push(new_profile);
                             // Switch to new profile
                             state.config.global.selected_profile = name_str;
                             
                             state.settings_changed = true;
                             window.set_has_unsaved_changes(true);
                             window.set_show_new_profile_dialog(false);
                             update_window_from_state(&window);
                         }
                     }
                });
            }
        });
    }

    {
         let window_weak = window.as_weak();
         window.on_duplicate_profile_confirmed(move |name, desc| {
             if let Some(window) = window_weak.upgrade() {
                 SHARED_STATE.with(|state_cell| {
                     if let Some(state_arc) = state_cell.borrow().as_ref() {
                         if let Ok(mut state) = state_arc.lock() {
                             let name_str = name.to_string();
                             let desc_str = desc.to_string();
                             
                             // Check existence
                             if state.config.profiles.iter().any(|p| p.profile_name == name_str) {
                                  error!("Profile already exists: {}", name_str);
                                  return;
                             }

                             if let Some(current) = state.config.get_active_profile() {
                                 let mut new_profile = current.clone();
                                 new_profile.profile_name = name_str.clone();
                                 new_profile.profile_description = desc_str;
                                 
                                 state.config.profiles.push(new_profile);
                                 state.config.global.selected_profile = name_str;
                                 
                                 state.settings_changed = true;
                                 window.set_has_unsaved_changes(true);
                                 window.set_show_duplicate_profile_dialog(false);
                                 update_window_from_state(&window);
                             }
                         }
                     }
                 });
             }
         });
    }

    {
         let window_weak = window.as_weak();
         window.on_edit_profile_confirmed(move |name, desc| {
             if let Some(window) = window_weak.upgrade() {
                 SHARED_STATE.with(|state_cell| {
                     if let Some(state_arc) = state_cell.borrow().as_ref() {
                         if let Ok(mut state) = state_arc.lock() {
                             let name_str = name.to_string();
                             let desc_str = desc.to_string();
                             
                             // If renaming, check existence of NEW name (unless it matches old name)
                             let current_name = state.config.global.selected_profile.clone();
                             if name_str != current_name && state.config.profiles.iter().any(|p| p.profile_name == name_str) {
                                  error!("Profile name taken: {}", name_str);
                                  return;
                             }
                             
                             if let Some(profile) = state.config.get_active_profile_mut() {
                                 profile.profile_name = name_str.clone();
                                 profile.profile_description = desc_str;
                                 
                                 // If name changed, update global selection
                                 if name_str != current_name {
                                     state.config.global.selected_profile = name_str;
                                 }
                                 
                                 state.settings_changed = true;
                                 window.set_has_unsaved_changes(true);
                                 window.set_show_edit_profile_dialog(false);
                                 update_window_from_state(&window);
                             }
                         }
                     }
                 });
             }
         });
    }
    
    {
         let window_weak = window.as_weak();
         window.on_delete_profile_confirmed(move || {
             if let Some(window) = window_weak.upgrade() {
                 SHARED_STATE.with(|state_cell| {
                     if let Some(state_arc) = state_cell.borrow().as_ref() {
                         if let Ok(mut state) = state_arc.lock() {
                             if state.config.profiles.len() <= 1 {
                                 error!("Cannot delete last profile");
                                 window.set_show_delete_profile_confirm(false);
                                 return;
                             }
                             
                             let current_name = state.config.global.selected_profile.clone();
                             // Remove profile
                             state.config.profiles.retain(|p| p.profile_name != current_name);
                             
                             // Select another profile (e.g. first one)
                             let next_profile_name = state.config.profiles.first().map(|p| p.profile_name.clone());
                             
                             if let Some(name) = next_profile_name {
                                 state.config.global.selected_profile = name.clone();
                                 info!("Switched to profile: {}", name);
                             }
                             
                             state.settings_changed = true;
                             window.set_has_unsaved_changes(true);
                             window.set_show_delete_profile_confirm(false);
                             update_window_from_state(&window);
                         }
                     }
                 });
             }
         });
    }
    
    // Hotkey callbacks
    {
        let window_weak = window.as_weak();
        window.on_update_binding(move |binding_id| {
             if let Some(_window) = window_weak.upgrade() {
                 debug!("Update binding requested for: {}", binding_id);
                 // UI handles showing modal, backend just acknowledges or prepares
             }
        });
        
        let window_weak = window.as_weak();
        window.on_cancel_binding(move || {
             debug!("Binding cancelled by user");
        });

        let window_weak = window.as_weak();
        window.on_apply_binding(move |binding_id, text, modifiers| {
             if let Some(window) = window_weak.upgrade() {
                 let key_combo = format!("{}{}", modifiers, text.to_uppercase());
                 debug!("Applying binding: {} = {}", binding_id, key_combo);
                 
                 SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                            if let Some(profile) = state.config.get_active_profile_mut() {
                                use crate::config::HotkeyBinding;
                                use std::str::FromStr;
                                
                                // let binding_result = HotkeyBinding::from_str(&key_combo);
                                let binding_result = key_combo.parse::<HotkeyBinding>();
                                
                                match binding_result {
                                    Ok(binding) => {
                                        match binding_id.as_str() {
                                            "cycle_forward" => {
                                                if let Some(group) = profile.cycle_groups.first_mut() {
                                                     group.hotkey_forward = Some(binding);
                                                     window.set_hotkey_cycle_forward(group.hotkey_forward.as_ref().map(|b| b.display_name().into()).unwrap_or_default());
                                                }
                                            },
                                            "cycle_backward" => {
                                                if let Some(group) = profile.cycle_groups.first_mut() {
                                                     group.hotkey_backward = Some(binding);
                                                     window.set_hotkey_cycle_backward(group.hotkey_backward.as_ref().map(|b| b.display_name().into()).unwrap_or_default());
                                                }
                                            },
                                            "toggle_previews" => {
                                                profile.hotkey_toggle_previews = Some(binding.clone());
                                                window.set_hotkey_toggle_preview(binding.display_name().into());
                                            },
                                            "toggle_skip" => {
                                                profile.hotkey_toggle_skip = Some(binding.clone());
                                                window.set_hotkey_toggle_skip(binding.display_name().into());
                                            },
                                            "profile_switch" => {
                                                 // TODO: This maps to profile hotkeys, which is complex (map vs field)
                                                 // Assume "Next Profile" action or specific profile binding?
                                                 // Current logic in hotkeys.slint suggests "Next Profile"
                                                 // But Profile struct usually maps specific keys to specific profiles.
                                                 // We'll skip this one or implement naive "Next" if supported.
                                                 warn!("Profile switch binding not fully supported via simple recorder yet");
                                            },
                                            _ => warn!("Unknown binding ID: {}", binding_id),
                                        }
                                        
                                        state.settings_changed = true;
                                        window.set_has_unsaved_changes(true);
                                        update_window_from_state(&window);
                                    },
                                    Err(e) => {
                                        error!("Failed to parse hotkey binding '{}': {}", key_combo, e);
                                    }
                                }
                            }
                        }
                    }
                 });
             }
        });
    }
    
    // Character callbacks
    {
        let window_weak = window.as_weak();
        window.on_character_selected(move |index| {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(state) = state_arc.lock() {
                            if let Some(profile) = state.config.get_active_profile() {
                                // Default to first cycle group for now
                                if let Some(group) = profile.cycle_groups.first() {
                                    if let Some(slot) = group.cycle_list.get(index as usize) {
                                        let name = match slot {
                                            crate::config::profile::CycleSlot::Eve(n) => n,
                                            crate::config::profile::CycleSlot::Source(n) => n,
                                        };
                                        
                                        window.set_character_edit_name(name.clone().into());
                                        
                                        // Load settings if exist, else defaults
                                        if let Some(settings) = profile.character_thumbnails.get(name) {
                                            window.set_character_edit_width(settings.dimensions.width as i32);
                                            window.set_character_edit_height(settings.dimensions.height as i32);
                                            // Disabled not supported in current struct
                                            window.set_character_edit_disabled(false);
                                        } else {
                                            window.set_character_edit_width(profile.thumbnail_default_width as i32);
                                            window.set_character_edit_height(profile.thumbnail_default_height as i32);
                                            window.set_character_edit_disabled(false);
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });
        
        let window_weak = window.as_weak();
        window.on_save_character(move || {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                             let name = window.get_character_edit_name();
                             if name.is_empty() { return; }
                             let name_str = name.to_string();
                             
                             let width = window.get_character_edit_width() as u16;
                             let height = window.get_character_edit_height() as u16;
                             // let disabled = window.get_character_edit_disabled(); // Unused
                             
                             if let Some(profile) = state.config.get_active_profile_mut() {
                                 // Update or insert
                                 let settings = profile.character_thumbnails.entry(name_str).or_insert(
                                     crate::common::types::CharacterSettings::new(
                                         0, 0, // Default position
                                         profile.thumbnail_default_width,
                                         profile.thumbnail_default_height
                                     )
                                 );
                                 settings.dimensions.width = width;
                                 settings.dimensions.height = height;
                                 // settings.disabled = disabled;
                                 
                                 state.settings_changed = true;
                                 window.set_has_unsaved_changes(true);
                             }
                        }
                    }
                });
            }
        });
     
        let window_weak = window.as_weak();
        window.on_move_character_up(move |index| {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                            if let Some(profile) = state.config.get_active_profile_mut() {
                                if let Some(group) = profile.cycle_groups.first_mut() {
                                    if index > 0 && (index as usize) < group.cycle_list.len() {
                                        group.cycle_list.swap(index as usize, (index - 1) as usize);
                                        state.settings_changed = true;
                                        window.set_has_unsaved_changes(true);
                                        // Refresh model and selection
                                        update_window_from_state(&window);
                                        window.set_character_selected_index(index - 1);
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });
        
        let window_weak = window.as_weak();
        window.on_move_character_down(move |index| {
             if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                            if let Some(profile) = state.config.get_active_profile_mut() {
                                if let Some(group) = profile.cycle_groups.first_mut() {
                                    if index >= 0 && (index as usize) < group.cycle_list.len() - 1 {
                                        group.cycle_list.swap(index as usize, (index + 1) as usize);
                                        state.settings_changed = true;
                                        window.set_has_unsaved_changes(true);
                                        // Refresh model and selection
                                        update_window_from_state(&window);
                                        window.set_character_selected_index(index + 1);
                                    }
                                }
                            }
                        }
                    }
                });
            }
        });
        let window_weak = window.as_weak();
        window.on_source_selected(move |index| {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(state) = state_arc.lock() {
                            if let Some(profile) = state.config.get_active_profile() {
                                if let Some(rule) = profile.custom_windows.get(index as usize) {
                                    window.set_source_edit_alias(rule.alias.clone().into());
                                    window.set_source_edit_title_pattern(rule.title_pattern.clone().unwrap_or_default().into());
                                    window.set_source_edit_class_pattern(rule.class_pattern.clone().unwrap_or_default().into());
                                    
                                    // Visual Overrides
                                    window.set_source_edit_border_color(rule.active_border_color.clone().unwrap_or_default().into());
                                    window.set_source_edit_border_size(rule.active_border_size.unwrap_or(0) as i32);
                                }
                            }
                        }
                    }
                });
            }
        });
        
        let window_weak = window.as_weak();
        window.on_save_source(move || {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                             let index = window.get_source_selected_index();
                             if index < 0 { return; }
                             
                             let alias = window.get_source_edit_alias().to_string();
                             let title = window.get_source_edit_title_pattern().to_string();
                             let class = window.get_source_edit_class_pattern().to_string();
                             let border_color = window.get_source_edit_border_color().to_string();
                             let border_size = window.get_source_edit_border_size() as u16;
                             
                             if let Some(profile) = state.config.get_active_profile_mut() {
                                 if let Some(rule) = profile.custom_windows.get_mut(index as usize) {
                                     rule.alias = alias;
                                     rule.title_pattern = if title.is_empty() { None } else { Some(title) };
                                     rule.class_pattern = if class.is_empty() { None } else { Some(class) };
                                     rule.active_border_color = if border_color.is_empty() { None } else { Some(border_color) };
                                     rule.active_border_size = if border_size == 0 { None } else { Some(border_size) };
                                     
                                     state.settings_changed = true;
                                     window.set_has_unsaved_changes(true);
                                     
                                     // Refresh list to update alias
                                     update_window_from_state(&window);
                                 }
                             }
                        }
                    }
                });
            }
        });

        let window_weak = window.as_weak();
        window.on_add_source(move || {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                             let new_index = if let Some(profile) = state.config.get_active_profile_mut() {
                                 let new_rule = crate::config::profile::CustomWindowRule {
                                     alias: "New Source".to_string(),
                                     title_pattern: None,
                                     class_pattern: None,
                                     default_width: profile.thumbnail_default_width,
                                     default_height: profile.thumbnail_default_height,
                                     limit: false,
                                     active_border_color: None,
                                     inactive_border_color: None,
                                     active_border_size: None,
                                     inactive_border_size: None,
                                     text_color: None,
                                     text_size: None,
                                     text_x: None,
                                     text_y: None,
                                     preview_mode: None,
                                     hotkey: None,
                                 };
                                 profile.custom_windows.push(new_rule);
                                 Some((profile.custom_windows.len() - 1) as i32)
                             } else {
                                 None
                             };

                             if let Some(index) = new_index {
                                 state.settings_changed = true;
                                 window.set_has_unsaved_changes(true);
                                 update_window_from_state(&window);
                                 
                                 // Select the new item
                                 window.set_source_selected_index(index);
                                 window.set_source_edit_alias("New Source".into());
                                 window.set_source_edit_title_pattern("".into());
                                 window.set_source_edit_class_pattern("".into());
                             }
                        }
                    }
                });
            }
        });
        
        let window_weak = window.as_weak();
        window.on_remove_source(move || {
            if let Some(window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                             let index = window.get_source_selected_index();
                             if index < 0 { return; }
                             
                             if let Some(profile) = state.config.get_active_profile_mut() {
                                 if (index as usize) < profile.custom_windows.len() {
                                     profile.custom_windows.remove(index as usize);
                                     state.settings_changed = true;
                                     window.set_has_unsaved_changes(true);
                                     window.set_source_selected_index(-1);
                                     update_window_from_state(&window);
                                 }
                             }
                        }
                    }
                });
            }
        });
    }

    // Backup Callbacks
    {
        let window_weak = window.as_weak();
        window.on_create_backup(move || {
             if let Some(_window) = window_weak.upgrade() {
                 info!("Creating manual backup...");
                 match crate::config::backup::BackupManager::create_backup(true, None) {
                     Ok(path) => info!("Backup created successfully at {:?}", path),
                     Err(e) => error!("Failed to create backup: {}", e),
                 }
             }
        });
        
        let _window_weak = window.as_weak();
        window.on_restore_backup(move || {
            // TODO: Implement restore dialog
            info!("Restore backup requested (not implemented in UI yet)");
        });
        
        let _window_weak = window.as_weak();
        window.on_delete_backup(move || {
             // TODO: Implement delete logic
             info!("Delete backup requested (not implemented in UI yet)");
        });
    }
}

/// Update window properties from shared state
fn update_window_from_state(window: &MainWindow) {
    SHARED_STATE.with(|state_cell| {
        if let Some(state_arc) = state_cell.borrow().as_ref() {
            if let Ok(state) = state_arc.lock() {
                // Update daemon status
                window.set_daemon_status(state.daemon_status.label().into());
                window.set_daemon_status_color(slint::Color::from_rgb_u8(
                    // TODO: Convert from egui::Color32 to slint::Color
                    200, 0, 0
                ));
                
                if let Some(child) = &state.daemon {
                    window.set_daemon_pid(child.id() as i32);
                } else {
                    window.set_daemon_pid(0);
                }
                
                // Update profile list
                let profile_names: Vec<slint::SharedString> = state.config.profiles
                    .iter()
                    .map(|p| p.profile_name.clone().into())
                    .collect();
                let profile_descs: Vec<slint::SharedString> = state.config.profiles
                    .iter()
                    .map(|p| p.profile_description.clone().into())
                    .collect();
                    
                window.set_profile_names(std::rc::Rc::new(slint::VecModel::from(profile_names)).into());
                window.set_profile_descriptions(std::rc::Rc::new(slint::VecModel::from(profile_descs)).into());
                window.set_selected_profile_index(state.selected_profile_idx as i32);
                
                // Update config status
                window.set_has_unsaved_changes(state.settings_changed);
                
                // Update Global Backup Settings
                window.set_backup_enabled(state.config.global.backup_enabled);
                window.set_backup_interval(state.config.global.backup_interval_days as i32);
                window.set_backup_retention(state.config.global.backup_retention_count as i32);
                
                // Update Appearance Tab Properties
                if let Some(profile) = state.config.get_active_profile() {
                    window.set_appearance_thumbnail_enabled(profile.thumbnail_enabled);
                    window.set_appearance_thumbnail_opacity(profile.thumbnail_opacity as f32);
                    window.set_appearance_active_border_enabled(profile.thumbnail_active_border);
                    window.set_appearance_active_border_size(profile.thumbnail_active_border_size as i32);
                    
                    // Convert hex string to Slint color or default
                    // Note: Slint doesn't have a direct string->color parser in public API easily accessible here without helper
                    // We'll rely on our helper or just pass the string if the UI expects string
                    // But wait, the UI expects 'string' for the LineEdit, but we also want a color preview.
                    // In main.slint we defined: in-out property <string> appearance-active-border-color;
                    // So passing string is correct.
                    window.set_appearance_active_border_color(profile.thumbnail_active_border_color.clone().into());
                    
                    window.set_appearance_inactive_border_enabled(profile.thumbnail_inactive_border);
                    window.set_appearance_inactive_border_size(profile.thumbnail_inactive_border_size as i32);
                    window.set_appearance_inactive_border_color(profile.thumbnail_inactive_border_color.clone().into());
                    
                    window.set_appearance_inactive_border_color(profile.thumbnail_inactive_border_color.clone().into());
                    
                    window.set_appearance_show_overlay(profile.client_minimize_show_overlay);
                    
                    // Behavior settings
                    window.set_behavior_auto_save_position(profile.thumbnail_auto_save_position);
                    window.set_behavior_snap_threshold(profile.thumbnail_snap_threshold as i32);
                    window.set_behavior_hide_not_focused(profile.thumbnail_hide_not_focused);
                    window.set_behavior_preserve_position(profile.thumbnail_preserve_position_on_swap);
                    window.set_behavior_inherit_position(profile.thumbnail_new_clients_inherit_position);
                    window.set_behavior_minimize_on_switch(profile.client_minimize_on_switch);
                    window.set_hotkey_require_eve_focus(profile.hotkey_require_eve_focus);
                    window.set_behavior_cycle_logged_out(profile.hotkey_logged_out_cycle);
                    window.set_behavior_reset_index(profile.hotkey_cycle_reset_index);
                    
                    // Hotkey settings (Input Device is per profile)
                    if let Some(device) = &profile.hotkey_input_device {
                        window.set_hotkey_input_device(device.clone().into());
                    } else {
                        window.set_hotkey_input_device("Auto".into());
                    }
                    
                    match profile.hotkey_backend {
                         crate::config::profile::HotkeyBackendType::X11 => window.set_hotkey_backend("X11".into()),
                         crate::config::profile::HotkeyBackendType::Evdev => window.set_hotkey_backend("Evdev".into()),
                    }
                    
                    // Thumbnail Text Settings
                    window.set_appearance_thumbnail_text_size(profile.thumbnail_text_size as i32);
                    window.set_appearance_thumbnail_text_color(profile.thumbnail_text_color.clone().into());
                    window.set_appearance_thumbnail_text_x(profile.thumbnail_text_x as i32);
                    window.set_appearance_thumbnail_text_y(profile.thumbnail_text_y as i32);
                    window.set_appearance_thumbnail_text_font(profile.thumbnail_text_font.clone().into());
                }
                
                // Hotkeys (Global/Profile) - These are actually stored in Profile
                if let Some(profile) = state.config.get_active_profile() {
                    let toggle_preview = profile.hotkey_toggle_previews.as_ref()
                        .map(|h| h.display_name()).unwrap_or("None".to_string());
                    window.set_hotkey_toggle_preview(toggle_preview.into());
                    
                    let toggle_skip = profile.hotkey_toggle_skip.as_ref()
                        .map(|h| h.display_name()).unwrap_or("None".to_string());
                    window.set_hotkey_toggle_skip(toggle_skip.into());
                    
                    let profile_switch = profile.hotkey_profile_switch.as_ref()
                        .map(|h| h.display_name()).unwrap_or("None".to_string());
                    window.set_hotkey_profile_switch(profile_switch.into());
                    
                    // Cycle Hotkeys (from Default Group)
                     if let Some(group) = profile.cycle_groups.first() {
                        let forward = group.hotkey_forward.as_ref().map(|h| h.display_name()).unwrap_or("None".to_string());
                        let backward = group.hotkey_backward.as_ref().map(|h| h.display_name()).unwrap_or("None".to_string());
                        window.set_hotkey_cycle_forward(forward.into());
                        window.set_hotkey_cycle_backward(backward.into());
                     }
                    
                    // Character Model
                    // Use first cycle group for now
                    if let Some(group) = profile.cycle_groups.first() {
                         let items: Vec<slint::StandardListViewItem> = group.cycle_list.iter().map(|slot| {
                             let name = match slot {
                                 crate::config::profile::CycleSlot::Eve(n) => n,
                                 crate::config::profile::CycleSlot::Source(n) => n,
                             };
                             slint::StandardListViewItem::from(slint::SharedString::from(name.clone()))
                         }).collect();
                         window.set_character_model(std::rc::Rc::new(slint::VecModel::from(items)).into());
                    }
                    
                    // Source Model
                    let source_items: Vec<slint::StandardListViewItem> = profile.custom_windows.iter().map(|rule| {
                        slint::StandardListViewItem::from(slint::SharedString::from(rule.alias.clone()))
                    }).collect();
                    window.set_source_model(std::rc::Rc::new(slint::VecModel::from(source_items)).into());
                }
            }
        }
    });
}

pub fn run_manager(debug_mode: bool) -> Result<()> {
    debug!("Initializing Slint Manager (debug_mode={})", debug_mode);

    // Load config
    let config = Config::load().unwrap_or_default();
    
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
            color_rgb: STATUS_STOPPED_RGB,
        });
    }
    let state = Arc::new(Mutex::new(state));
    
    // Store state in thread-local
    // Store state in thread-local
    SHARED_STATE.with(|state_cell| {
        *state_cell.borrow_mut() = Some(state.clone());
    });

    // Start global status/polling timer
    // This runs regardless of whether the window is visible
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(500), move || {
        SHARED_STATE.with(|state_cell| {
            if let Some(state_arc) = state_cell.borrow().as_ref() {
                if let Ok(mut state) = state_arc.lock() {
                    // Always poll daemon to ensure IPC/heartbeats work
                    state.poll_daemon();
                    
                    // If window is visible, update it
                    MAIN_WINDOW.with(|window_cell| {
                        if let Some(window) = window_cell.borrow().as_ref() {
                            // Update Daemon Status in UI
                            let (status_text, status_color): (slint::SharedString, (u8, u8, u8)) = match state.daemon_status {
                                crate::manager::state::DaemonStatus::Running(pid) => {
                                    window.set_daemon_pid(pid as i32);
                                    ("Running".into(), crate::common::constants::manager_ui::STATUS_RUNNING_RGB.into())
                                },
                                crate::manager::state::DaemonStatus::Stopped => {
                                    window.set_daemon_pid(0);
                                    ("Stopped".into(), crate::common::constants::manager_ui::STATUS_STOPPED_RGB.into())
                                },
                                crate::manager::state::DaemonStatus::Starting => {
                                    window.set_daemon_pid(0);
                                    ("Starting...".into(), crate::common::constants::manager_ui::STATUS_WARNING_RGB.into())
                                },
                                crate::manager::state::DaemonStatus::Crashed(code) => {
                                    window.set_daemon_pid(0);
                                    let msg = if let Some(c) = code {
                                        format!("Crashed (Exit: {})", c)
                                    } else {
                                        "Crashed".to_string()
                                    };
                                    (msg.into(), crate::common::constants::manager_ui::STATUS_STOPPED_RGB.into())
                                },
                            };
                            window.set_daemon_status(status_text);
                            window.set_daemon_status_color(crate::common::color::slint_color_from_rgb(status_color.0, status_color.1, status_color.2));
                            
                            // Update Status Message in UI
                            if let Some(msg) = &state.status_message {
                                window.set_status_message(msg.text.clone().into());
                                window.set_status_message_color(crate::common::color::slint_color_from_rgb(msg.color_rgb.0, msg.color_rgb.1, msg.color_rgb.2));
                            } else {
                                window.set_status_message("".into());
                            }
    
                            // Update Config Status Message in UI
                            if let Some(msg) = &state.config_status_message {
                                window.set_config_status_message(msg.text.clone().into());
                                window.set_config_status_color(crate::common::color::slint_color_from_rgb(msg.color_rgb.0, msg.color_rgb.1, msg.color_rgb.2));
                            } else {
                                window.set_config_status_message("".into());
                            }
                        }
                    });
                }
            }
        });
    });
    STATUS_TIMER.with(|t| *t.borrow_mut() = Some(timer));

    #[cfg(target_os = "linux")]
    {
        let state_clone = state.clone();

// Start tray in separate thread
    // Even "blocking" ksni needs Tokio runtime for internal zbus calls
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime for tray");
        runtime.block_on(async {
            let is_flatpak = std::env::var("FLATPAK_ID").is_ok();
            let tray = crate::manager::components::tray::AppTray {
                state: state_clone,
                is_flatpak,
            };
            // Run blocking ksni spawn in spawn_blocking to avoid blocking the runtime
            let _handle = tokio::task::spawn_blocking(move || {
                // Use fully qualified syntax to avoid trait ambiguity
                if is_flatpak {
                    info!("Running in Flatpak: disabling D-Bus name");
                    <crate::manager::components::tray::AppTray as ksni::blocking::TrayMethods>::disable_dbus_name(tray, true)
                        .spawn()
                        .expect("Failed to spawn tray")
                } else {
                    <crate::manager::components::tray::AppTray as ksni::blocking::TrayMethods>::spawn(tray)
                        .expect("Failed to spawn tray")
                }
            }).await.expect("Failed to spawn tray task");
            // Keep runtime alive with async sleep instead of park()
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
            }
        });
    });
    }

    // Create a global Tokio runtime for the main thread (needed by Slint/winit/zbus)
    // We use a multi-thread runtime so that background IO (zbus) can proceed 
    // even while the main thread is blocked by the Slint event loop.
    let main_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create main Tokio runtime");
    
    // Enter the runtime context for the duration of the event loop
    let _guard = main_runtime.enter();

    // Run Slint event loop (application starts with no window visible)
    slint::run_event_loop()
        .map_err(|err| anyhow!("Failed to run Slint event loop: {err}"))?;

    // Cleanup on exit
    if let Ok(mut state) = state.lock() {
        if let Err(err) = state.stop_daemon() {
            error!(error = ?err, "Failed to stop daemon during shutdown");
        }
        // Save config
        if let Err(err) = state.save_config(crate::manager::state::core::SaveMode::Implicit) {
            error!(error = ?err, "Failed to save window geometry on exit");
        } else {
            info!("Configuration saved on exit");
        }
    }

    #[cfg(target_os = "linux")]
    {
        // TODO: Signal tray thread to shutdown
        info!("Signaled tray thread to shutdown");
    }

    info!("Manager exiting");
    Ok(())
}
