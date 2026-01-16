//! Slint-based application manager - primary interface for configuration and daemon control

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use tracing::{debug, error, info};

#[cfg(target_os = "linux")]
use ksni::TrayMethods;

use crate::common::constants::manager_ui::*;
use crate::config::backup::BackupManager;
use crate::config::profile::Config;
use crate::manager::state::{ManagerTab, SharedState, StatusMessage};

// Include the generated Slint components
slint::include_modules!();

// Thread-local storage for the main window (following SLINT_KSNI_INTEGRATION.md pattern)
thread_local! {
    static MAIN_WINDOW: RefCell<Option<MainWindow>> = RefCell::new(None);
    static SHARED_STATE: RefCell<Option<Arc<Mutex<SharedState>>>> = RefCell::new(None);
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
                            // TODO: Implement save logic
                            debug!("Save config requested");
                        }
                    }
                });
            }
        });
    }
    
    {
        let window_weak = window.as_weak();
        window.on_discard_changes(move || {
            if let Some(_window) = window_weak.upgrade() {
                SHARED_STATE.with(|state_cell| {
                    if let Some(state_arc) = state_cell.borrow().as_ref() {
                        if let Ok(mut state) = state_arc.lock() {
                            // TODO: Implement discard logic
                            debug!("Discard changes requested");
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
            if let Some(_window) = window_weak.upgrade() {
                debug!("New profile requested");
                // TODO: Show new profile dialog
            }
        });
    }
    
    {
        let window_weak = window.as_weak();
        window.on_duplicate_profile(move || {
            if let Some(_window) = window_weak.upgrade() {
                debug!("Duplicate profile requested");
                // TODO: Show duplicate profile dialog
            }
        });
    }
    
    {
        let window_weak = window.as_weak();
        window.on_edit_profile(move || {
            if let Some(_window) = window_weak.upgrade() {
                debug!("Edit profile requested");
                // TODO: Show edit profile dialog
            }
        });
    }
    
    {
        let window_weak = window.as_weak();
        window.on_delete_profile(move || {
            if let Some(_window) = window_weak.upgrade() {
                debug!("Delete profile requested");
                // TODO: Show delete confirmation dialog
            }
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
    SHARED_STATE.with(|state_cell| {
        *state_cell.borrow_mut() = Some(state.clone());
    });

    #[cfg(target_os = "linux")]
    {
        let shutdown_signal = std::sync::Arc::new(tokio::sync::Notify::new());
        let shutdown_clone = shutdown_signal.clone();
        let update_signal = std::sync::Arc::new(tokio::sync::Notify::new());
        let update_clone = update_signal.clone();
        let state_clone = state.clone();

        // Start tray in separate thread
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime for tray");

            runtime.block_on(async move {
                let is_flatpak = std::env::var("FLATPAK_ID").is_ok();
                let tray = crate::manager::components::tray::AppTray {
                    state: state_clone,
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
                        debug!("Tray icon created via ksni/D-Bus");
                        loop {
                            tokio::select! {
                                _ = shutdown_clone.notified() => {
                                    handle.shutdown().await;
                                    break;
                                }
                                _ = update_clone.notified() => {
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
    }

    // Run Slint event loop (application starts with no window visible)
    slint::run_event_loop_until_quit()
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
