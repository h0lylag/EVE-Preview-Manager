//! Global settings component (applies to all profiles)

use eframe::egui;
use crate::config::profile::GlobalSettings;
use crate::constants::gui::*;

/// Renders global settings UI and returns true if changes were made
pub fn ui(ui: &mut egui::Ui, global: &mut GlobalSettings) -> bool {
    let mut changed = false;
    
    // Behavior Settings (Global)
    ui.group(|ui| {
        ui.label(egui::RichText::new("Behavior Settings").strong());
        ui.add_space(ITEM_SPACING);
        
        // Minimize clients on switch
        if ui.checkbox(&mut global.minimize_clients_on_switch, 
            "Minimize EVE clients when switching focus").changed() {
            changed = true;
        }
        
        ui.label(egui::RichText::new(
            "When clicking a thumbnail, minimize all other EVE clients")
            .small()
            .weak());
        
        ui.add_space(ITEM_SPACING);
        
        // Hide when no focus
        if ui.checkbox(&mut global.hide_when_no_focus, 
            "Hide thumbnails when EVE loses focus").changed() {
            changed = true;
        }
        
        ui.label(egui::RichText::new(
            "When enabled, thumbnails disappear when no EVE window is focused")
            .small()
            .weak());
        
        ui.add_space(ITEM_SPACING);
        
        // Preserve thumbnail position on character swap
        if ui.checkbox(&mut global.preserve_thumbnail_position_on_swap, 
            "Keep thumbnail position when switching characters").changed() {
            changed = true;
        }
        
        ui.label(egui::RichText::new(
            "New characters inherit thumbnail position from the logged-out character")
            .small()
            .weak());
        
        ui.add_space(ITEM_SPACING);
        
        // Snap threshold
        ui.horizontal(|ui| {
            ui.label("Thumbnail Snap Distance:");
            if ui.add(egui::Slider::new(&mut global.snap_threshold, 0..=50)
                .suffix(" px")).changed() {
                changed = true;
            }
        });
        
        ui.label(egui::RichText::new(
            "Distance for edge/corner snapping (0 = disabled)")
            .small()
            .weak());
        
        ui.add_space(ITEM_SPACING);
        
        // Default thumbnail dimensions with aspect ratio controls
        ui.vertical(|ui| {
            // Aspect ratio preset definitions
            let aspect_ratios = [
                ("16:9", 16.0 / 9.0),
                ("16:10", 16.0 / 10.0),
                ("4:3", 4.0 / 3.0),
                ("21:9", 21.0 / 9.0),
                ("Custom", 0.0),
            ];
            
            // Calculate current aspect ratio and find closest matching preset
            let current_ratio = global.default_thumbnail_width as f32 / global.default_thumbnail_height as f32;
            let detected_preset = {
                let mut preset = "Custom";
                for (name, ratio) in &aspect_ratios[..aspect_ratios.len()-1] {
                    if (current_ratio - ratio).abs() < 0.01 {
                        preset = name;
                        break;
                    }
                }
                preset
            };
            
            // Use egui memory to persist the selected mode
            let id = ui.make_persistent_id("thumbnail_aspect_mode");
            let mut selected_mode = ui.data_mut(|d| 
                d.get_temp::<String>(id).unwrap_or_else(|| detected_preset.to_string())
            );
            
            ui.horizontal(|ui| {
                ui.label("Default Thumbnail Size:");
                
                let mut mode_changed = false;
                egui::ComboBox::from_id_salt("thumbnail_aspect_ratio")
                    .selected_text(&selected_mode)
                    .show_ui(ui, |ui| {
                        for (name, ratio) in &aspect_ratios {
                            if ui.selectable_value(&mut selected_mode, name.to_string(), *name).changed() {
                                mode_changed = true;
                                if *ratio > 0.0 {
                                    // Update height based on width and selected ratio
                                    global.default_thumbnail_height = 
                                        (global.default_thumbnail_width as f32 / ratio).round() as u16;
                                    changed = true;
                                }
                            }
                        }
                    });
                
                // Save the selected mode to egui memory
                if mode_changed {
                    ui.data_mut(|d| d.insert_temp(id, selected_mode.clone()));
                }
            });
            
            ui.add_space(ITEM_SPACING / 2.0);
            
            // Width slider (primary control)
            ui.horizontal(|ui| {
                ui.label("Width:");
                if ui.add(egui::Slider::new(&mut global.default_thumbnail_width, 100..=800)
                    .suffix(" px")).changed() {
                    // If not custom, maintain aspect ratio
                    if selected_mode != "Custom" {
                        for (name, ratio) in &aspect_ratios[..aspect_ratios.len()-1] {
                            if name == &selected_mode.as_str() {
                                global.default_thumbnail_height = 
                                    (global.default_thumbnail_width as f32 / ratio).round() as u16;
                                break;
                            }
                        }
                    }
                    changed = true;
                }
            });
            
            // Height slider (locked unless custom)
            let is_custom = selected_mode == "Custom";
            ui.horizontal(|ui| {
                ui.label("Height:");
                
                if is_custom {
                    if ui.add(egui::Slider::new(&mut global.default_thumbnail_height, 50..=600)
                        .suffix(" px")).changed() {
                        changed = true;
                    }
                } else {
                    ui.add_enabled(false, 
                        egui::Slider::new(&mut global.default_thumbnail_height, 50..=600)
                            .suffix(" px"));
                    ui.weak("(locked to aspect ratio)");
                }
            });
            
            // Preview display
            ui.horizontal(|ui| {
                ui.weak(format!(
                    "Preview: {}×{} ({:.2}:1 ratio)", 
                    global.default_thumbnail_width, 
                    global.default_thumbnail_height,
                    global.default_thumbnail_width as f32 / global.default_thumbnail_height as f32
                ));
            });
        });
        
        ui.label(egui::RichText::new(
            "Default size for newly created character thumbnails")
            .small()
            .weak());
    });
    
    ui.add_space(SECTION_SPACING);
    
    // Hotkey Settings (Global)
    ui.group(|ui| {
        ui.label(egui::RichText::new("Hotkey Settings").strong());
        ui.add_space(ITEM_SPACING);
        
        // Hotkey require EVE focus
        if ui.checkbox(&mut global.hotkey_require_eve_focus, 
            "Require EVE window focused for hotkeys to work").changed() {
            changed = true;
        }
        
        ui.label(egui::RichText::new(
            "When enabled, Tab/Shift+Tab only work when an EVE window is focused")
            .small()
            .weak());
        
        ui.add_space(ITEM_SPACING);
        ui.separator();
        ui.add_space(ITEM_SPACING);
        
        ui.label(egui::RichText::new("Custom Hotkey Editor").italics());
        ui.label("Future: Configure custom global hotkeys here");
        ui.label("• Screenshot hotkey");
        ui.label("• Quick minimize all");
        ui.label("• Toggle preview visibility");
    });
    
    changed
}
