use eframe::egui;
use crate::config::profile::Profile;
use crate::constants::gui::*;
use crate::gui::key_capture::{self, CaptureResult, CaptureState};
use crate::types::Dimensions;
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureTarget {
    Forward,
    Backward,
}

/// State for visual settings UI
pub struct VisualSettingsState {
    show_resize_confirmation: bool,
    pending_resize_all: Option<Dimensions>,
    current_width: u16,
    current_height: u16,
    last_target: String,
    available_fonts: Vec<String>,
    font_load_error: Option<String>,

    // Input device state
    available_devices: Vec<(String, String)>, // (device_id, friendly_name)
    device_load_error: Option<String>,

    // Key capture state
    show_key_capture_dialog: bool,
    capture_target: Option<CaptureTarget>,
    capture_state_rx: Option<Receiver<CaptureState>>,
    capture_result_rx: Option<Receiver<CaptureResult>>,
    current_capture_state: Option<CaptureState>,
    capture_result: Option<CaptureResult>,
    capture_error: Option<String>,
}

impl VisualSettingsState {
    pub fn new() -> Self {
        // Load available fonts at GUI startup
        let (available_fonts, font_load_error) = match crate::preview::list_fonts() {
            Ok(fonts) => (fonts, None),
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to load font list from fontconfig");
                (vec!["Monospace".to_string()], Some(e.to_string()))
            }
        };

        // Load available input devices at GUI startup
        let (available_devices, device_load_error) = match crate::hotkeys::list_input_devices() {
            Ok(devices) => (devices, None),
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to load input device list");
                (Vec::new(), Some(e.to_string()))
            }
        };

        Self {
            show_resize_confirmation: false,
            pending_resize_all: None,
            current_width: 250,
            current_height: 141,
            last_target: "---".to_string(),
            available_fonts,
            font_load_error,
            available_devices,
            device_load_error,
            show_key_capture_dialog: false,
            capture_target: None,
            capture_state_rx: None,
            capture_result_rx: None,
            current_capture_state: None,
            capture_result: None,
            capture_error: None,
        }
    }

    /// Start capturing a key for the specified target
    fn start_key_capture(&mut self, target: CaptureTarget) {
        match key_capture::start_capture() {
            Ok((state_rx, result_rx)) => {
                self.show_key_capture_dialog = true;
                self.capture_target = Some(target);
                self.capture_state_rx = Some(state_rx);
                self.capture_result_rx = Some(result_rx);
                self.current_capture_state = Some(CaptureState::new());
                self.capture_result = None;
                self.capture_error = None;
            }
            Err(e) => {
                self.capture_error = Some(format!("Failed to start key capture: {}", e));
            }
        }
    }

    /// Cancel ongoing key capture
    fn cancel_capture(&mut self) {
        self.show_key_capture_dialog = false;
        self.capture_target = None;
        self.capture_state_rx = None;
        self.capture_result_rx = None;
        self.current_capture_state = None;
        self.capture_result = None;
    }
}

impl Default for VisualSettingsState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn ui(ui: &mut egui::Ui, profile: &mut Profile, state: &mut VisualSettingsState) -> bool {
    let mut changed = false;

    // Poll capture state updates if capture is active
    if state.show_key_capture_dialog {
        // Check for final result first
        let mut got_result = false;
        if let Some(ref result_rx) = state.capture_result_rx {
            if let Ok(result) = result_rx.try_recv() {
                // Auto-close on Escape (Cancelled)
                if matches!(result, CaptureResult::Cancelled) {
                    state.cancel_capture();
                    got_result = false; // Don't process this further
                } else {
                    state.capture_result = Some(result);
                    got_result = true;
                }
            }
        }

        // Update state, but if we got a result, drain all buffered states
        // to avoid showing stale "Modifier+?" states after capture completes
        if let Some(ref state_rx) = state.capture_state_rx {
            if got_result {
                // Drain all buffered state updates to get the latest one
                let mut last_state = None;
                while let Ok(capture_state) = state_rx.try_recv() {
                    last_state = Some(capture_state);
                }
                if let Some(capture_state) = last_state {
                    state.current_capture_state = Some(capture_state);
                }
            } else {
                // Normal case: just get the latest state
                if let Ok(capture_state) = state_rx.try_recv() {
                    state.current_capture_state = Some(capture_state);
                }
            }
        }
    }

    // Show capture error if any
    if let Some(ref error) = state.capture_error {
        ui.colored_label(egui::Color32::from_rgb(200, 0, 0), format!("⚠ {}", error));
        ui.add_space(ITEM_SPACING);
    }

    ui.group(|ui| {
        ui.label(egui::RichText::new("Visual Settings").strong());
        ui.add_space(ITEM_SPACING);
        
        // Opacity
        ui.horizontal(|ui| {
            ui.label("Opacity:");
            if ui.add(egui::Slider::new(&mut profile.opacity_percent, 0..=100)
                .suffix("%")).changed() {
                changed = true;
            }
        });
        
        // Border toggle
        ui.horizontal(|ui| {
            ui.label("Borders:");
            if ui.checkbox(&mut profile.border_enabled, "Enabled").changed() {
                changed = true;
            }
        });
        
        // Border settings (only if enabled)
        if profile.border_enabled {
            ui.indent("border_settings", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Border Size:");
                    if ui.add(egui::DragValue::new(&mut profile.border_size)
                        .range(1..=20)).changed() {
                        changed = true;
                    }
                });
                
                ui.horizontal(|ui| {
                    ui.label("Border Color:");
                    let text_edit = egui::TextEdit::singleline(&mut profile.border_color)
                        .desired_width(100.0);
                    if ui.add(text_edit).changed() {
                        changed = true;
                    }
                    
                    // Color picker button - parses hex string, shows picker, updates string
                    if let Ok(mut color) = parse_hex_color(&profile.border_color)
                        && ui.color_edit_button_srgba(&mut color).changed() {
                            profile.border_color = format_hex_color(color);
                            changed = true;
                        }
                });
            });
        }
        
        ui.add_space(ITEM_SPACING);
        
        // Text settings
        ui.horizontal(|ui| {
            ui.label("Text Size:");
            if ui.add(egui::DragValue::new(&mut profile.text_size)
                .range(8..=48)).changed() {
                changed = true;
            }
        });
        
        ui.horizontal(|ui| {
            ui.label("Text Position:");
            ui.label("X:");
            if ui.add(egui::DragValue::new(&mut profile.text_x)
                .range(0..=100)).changed() {
                changed = true;
            }
            ui.label("Y:");
            if ui.add(egui::DragValue::new(&mut profile.text_y)
                .range(0..=100)).changed() {
                changed = true;
            }
        });
        
        ui.horizontal(|ui| {
            ui.label("Text Color:");
            let text_edit = egui::TextEdit::singleline(&mut profile.text_color)
                .desired_width(100.0);
            if ui.add(text_edit).changed() {
                changed = true;
            }
            
            // Color picker button
            if let Ok(mut color) = parse_hex_color(&profile.text_color)
                && ui.color_edit_button_srgba(&mut color).changed() {
                    profile.text_color = format_hex_color(color);
                    changed = true;
                }
        });
        
        // Font family selector
        ui.horizontal(|ui| {
            ui.label("Font:");
            
            if let Some(ref error) = state.font_load_error {
                ui.colored_label(egui::Color32::RED, "⚠")
                    .on_hover_text(format!("Failed to load fonts: {}", error));
            }
            
            egui::ComboBox::from_id_salt("text_font_family")
                .selected_text(&profile.text_font_family)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for font_family in &state.available_fonts {
                        if ui.selectable_value(
                            &mut profile.text_font_family,
                            font_family.clone(),
                            font_family
                        ).changed() {
                            changed = true;
                        }
                    }
                });
        });
    });
    
    ui.add_space(SECTION_SPACING);
    
    // Thumbnail Size Editor
    ui.group(|ui| {
        ui.label(egui::RichText::new("Thumbnail Size Adjustment").strong());
        ui.add_space(ITEM_SPACING);
        
        // Target selector
        let id = ui.make_persistent_id("thumbnail_resize_target");
        let mut selected_target = ui.data_mut(|d| 
            d.get_temp::<String>(id).unwrap_or_else(|| "---".to_string())
        );
        
        ui.horizontal(|ui| {
            ui.label("Resize:");
            
            egui::ComboBox::from_id_salt("thumbnail_resize_target")
                .selected_text(&selected_target)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected_target, "---".to_string(), "---");
                    ui.selectable_value(&mut selected_target, "All Characters".to_string(), "All Characters");
                    ui.separator();
                    
                    // Add individual characters
                    let mut char_names: Vec<_> = profile.character_positions.keys().cloned().collect();
                    char_names.sort();
                    for char_name in char_names {
                        ui.selectable_value(&mut selected_target, char_name.clone(), char_name);
                    }
                });
            
            ui.data_mut(|d| d.insert_temp(id, selected_target.clone()));
        });
        
        ui.add_space(ITEM_SPACING / 2.0);
        
        let is_enabled = selected_target != "---";
        
        // If target changed, load new dimensions
        if is_enabled && selected_target != state.last_target {
            let (width, height) = if selected_target == "All Characters" {
                // Use first character's dimensions as baseline, or default
                profile.character_positions.values().next()
                    .map(|s| (s.dimensions.width, s.dimensions.height))
                    .unwrap_or((250, 141))
            } else {
                // Get specific character's dimensions
                profile.character_positions.get(&selected_target)
                    .map(|s| (s.dimensions.width, s.dimensions.height))
                    .unwrap_or((250, 141))
            };
            
            state.current_width = width;
            state.current_height = height;
            state.last_target = selected_target.clone();
        }
        
        // Aspect ratio controls (same as global settings)
        let aspect_ratios = [
            ("16:9", 16.0 / 9.0),
            ("16:10", 16.0 / 10.0),
            ("4:3", 4.0 / 3.0),
            ("21:9", 21.0 / 9.0),
            ("Custom", 0.0),
        ];
        
        let current_ratio = state.current_width as f32 / state.current_height as f32;
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
        
        let mode_id = ui.make_persistent_id(format!("resize_aspect_mode_{}", selected_target));
        let mut aspect_mode = ui.data_mut(|d| 
            d.get_temp::<String>(mode_id).unwrap_or_else(|| detected_preset.to_string())
        );
        
        ui.add_enabled_ui(is_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Aspect Ratio:");
                
                let mut mode_changed = false;
                egui::ComboBox::from_id_salt(format!("resize_aspect_{}", selected_target))
                    .selected_text(&aspect_mode)
                    .show_ui(ui, |ui| {
                        for (name, ratio) in &aspect_ratios {
                            if ui.selectable_value(&mut aspect_mode, name.to_string(), *name).changed() {
                                mode_changed = true;
                                if *ratio > 0.0 {
                                    state.current_height = (state.current_width as f32 / ratio).round() as u16;
                                }
                            }
                        }
                    });
                
                if mode_changed {
                    ui.data_mut(|d| d.insert_temp(mode_id, aspect_mode.clone()));
                }
            });
            
            ui.add_space(ITEM_SPACING / 2.0);
            
            // Width slider
            ui.horizontal(|ui| {
                ui.label("Width:");
                if ui.add(egui::Slider::new(&mut state.current_width, 100..=800)
                    .suffix(" px")).changed() {
                    // Maintain aspect ratio if not custom
                    if aspect_mode != "Custom" {
                        for (name, ratio) in &aspect_ratios[..aspect_ratios.len()-1] {
                            if name == &aspect_mode.as_str() {
                                state.current_height = (state.current_width as f32 / ratio).round() as u16;
                                break;
                            }
                        }
                    }
                }
            });
            
            // Height slider
            let is_custom = aspect_mode == "Custom";
            ui.horizontal(|ui| {
                ui.label("Height:");
                
                if is_custom {
                    ui.add(egui::Slider::new(&mut state.current_height, 50..=600)
                        .suffix(" px"));
                } else {
                    ui.add_enabled(false, 
                        egui::Slider::new(&mut state.current_height, 50..=600)
                            .suffix(" px"));
                }
            });
            
            ui.horizontal(|ui| {
                ui.weak(format!(
                    "Preview: {}×{} ({:.2}:1 ratio)", 
                    state.current_width, 
                    state.current_height,
                    state.current_width as f32 / state.current_height as f32
                ));
            });
            
            ui.add_space(ITEM_SPACING / 2.0);
            
            // Apply button (narrower)
            ui.horizontal(|ui| {
                if ui.button("Apply Size").clicked() {
                    let new_dimensions = Dimensions::new(state.current_width, state.current_height);
                    
                    if selected_target == "All Characters" {
                        // Show confirmation for "All Characters"
                        state.pending_resize_all = Some(new_dimensions);
                        state.show_resize_confirmation = true;
                    } else {
                        // Apply to single character immediately
                        if let Some(char_settings) = profile.character_positions.get_mut(&selected_target) {
                            char_settings.dimensions = new_dimensions;
                            changed = true;
                        }
                    }
                }
            });
        });
    });
    
    // Confirmation dialog for resizing all characters
    if state.show_resize_confirmation {
        egui::Window::new("Confirm Resize")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if let Some(dims) = state.pending_resize_all {
                    ui.label(format!(
                        "Apply {}×{} size to all {} character thumbnails?",
                        dims.width,
                        dims.height,
                        profile.character_positions.len()
                    ));
                    ui.add_space(ITEM_SPACING);
                    ui.label(egui::RichText::new("This will overwrite all individual thumbnail sizes.")
                        .small()
                        .weak());
                    ui.add_space(ITEM_SPACING);
                    
                    ui.horizontal(|ui| {
                        if ui.button("Yes, Resize All").clicked() {
                            // Apply to all characters
                            for char_settings in profile.character_positions.values_mut() {
                                char_settings.dimensions = dims;
                            }
                            changed = true;
                            state.show_resize_confirmation = false;
                            state.pending_resize_all = None;
                        }
                        
                        if ui.button("Cancel").clicked() {
                            state.show_resize_confirmation = false;
                            state.pending_resize_all = None;
                        }
                    });
                }
            });
    }

    ui.add_space(SECTION_SPACING);

    // Hotkey Settings
    ui.group(|ui| {
        ui.label(egui::RichText::new("Hotkey Settings").strong());
        ui.add_space(ITEM_SPACING);

        // Hotkey Bindings
        ui.label("Configure keys for cycling through characters:");
        ui.add_space(ITEM_SPACING / 2.0);

        // Forward key binding
        ui.horizontal(|ui| {
            ui.label("Forward cycle:");
            let binding_text = profile.cycle_forward_keys.as_ref()
                .map(|b| b.display_name())
                .unwrap_or_else(|| "Not set".to_string());
            let color = if profile.cycle_forward_keys.is_none() {
                egui::Color32::from_rgb(200, 100, 100) // Red-ish for unset
            } else {
                ui.style().visuals.text_color()
            };
            ui.label(egui::RichText::new(binding_text).strong().color(color));
            if ui.button("🎹 Bind Key").clicked() {
                state.start_key_capture(CaptureTarget::Forward);
            }
        });

        ui.add_space(ITEM_SPACING / 2.0);

        // Backward key binding
        ui.horizontal(|ui| {
            ui.label("Backward cycle:");
            let binding_text = profile.cycle_backward_keys.as_ref()
                .map(|b| b.display_name())
                .unwrap_or_else(|| "Not set".to_string());
            let color = if profile.cycle_backward_keys.is_none() {
                egui::Color32::from_rgb(200, 100, 100) // Red-ish for unset
            } else {
                ui.style().visuals.text_color()
            };
            ui.label(egui::RichText::new(binding_text).strong().color(color));
            if ui.button("🎹 Bind Key").clicked() {
                state.start_key_capture(CaptureTarget::Backward);
            }
        });

        ui.add_space(ITEM_SPACING);

        // Input device selector
        ui.label("Input device to monitor:");
        ui.add_space(ITEM_SPACING / 2.0);

        let selected_display = if let Some(ref device_id) = profile.selected_hotkey_device {
            // Find friendly name for selected device
            state.available_devices.iter()
                .find(|(id, _)| id == device_id)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| device_id.clone())
        } else {
            "All Devices".to_string()
        };

        egui::ComboBox::from_id_salt("hotkey_device_selector")
            .selected_text(&selected_display)
            .show_ui(ui, |ui| {
                // "All Devices" option
                if ui.selectable_value(&mut profile.selected_hotkey_device, None, "All Devices").clicked() {
                    changed = true;
                }

                ui.separator();

                // Individual devices
                for (device_id, friendly_name) in &state.available_devices {
                    let device_clone = device_id.clone();
                    if ui.selectable_value(
                        &mut profile.selected_hotkey_device,
                        Some(device_clone),
                        friendly_name
                    ).clicked() {
                        changed = true;
                    }
                }
            });

        if let Some(ref error) = state.device_load_error {
            ui.add_space(ITEM_SPACING / 4.0);
            ui.label(egui::RichText::new(format!("⚠ {}", error)).small().color(egui::Color32::from_rgb(200, 100, 0)));
        }

        ui.add_space(ITEM_SPACING);

        // Logged-out cycling checkbox
        if ui.checkbox(
            &mut profile.include_logged_out_in_cycle,
            "Include logged-out characters in cycle"
        ).changed() {
            changed = true;
        }

        ui.add_space(ITEM_SPACING / 4.0);

        ui.label(egui::RichText::new(
            "When enabled, characters that log out will remain in the cycle using their last position")
            .small()
            .weak());

        ui.add_space(ITEM_SPACING);

        // Auto-save thumbnail positions checkbox
        if ui.checkbox(
            &mut profile.auto_save_thumbnail_positions,
            "Automatically save thumbnail positions"
        ).changed() {
            changed = true;
        }

        ui.add_space(ITEM_SPACING / 4.0);

        ui.label(egui::RichText::new(
            "When disabled, positions are only saved when you use 'Save Thumbnail Positions' from the system tray menu")
            .small()
            .weak());
    });

    // Key Capture Dialog
    if state.show_key_capture_dialog {
        egui::Window::new("🎹 Capture Hotkey")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.set_min_width(350.0);

                let target_name = match state.capture_target {
                    Some(CaptureTarget::Forward) => "Forward Cycle",
                    Some(CaptureTarget::Backward) => "Backward Cycle",
                    None => "Unknown",
                };

                ui.label(format!("Binding key for: {}", target_name));
                ui.add_space(ITEM_SPACING);

                // Show current capture state
                if let Some(ref capture_state) = state.current_capture_state {
                    ui.group(|ui| {
                        ui.set_min_width(320.0);
                        ui.vertical_centered(|ui| {
                            ui.add_space(ITEM_SPACING);
                            ui.label(egui::RichText::new(&capture_state.description)
                                .size(20.0)
                                .strong()
                                .color(egui::Color32::from_rgb(0, 200, 0)));
                            ui.add_space(ITEM_SPACING);
                        });
                    });
                } else {
                    ui.label("Initializing capture...");
                }

                ui.add_space(ITEM_SPACING);

                // Instructions
                ui.label(egui::RichText::new("Instructions:").strong());
                ui.label("• Press any key combination to bind it");
                ui.label("• Press Esc to cancel");

                ui.add_space(ITEM_SPACING);

                // Check if capture completed
                if let Some(ref result) = state.capture_result {
                    match result {
                        CaptureResult::Captured(binding) => {
                            // Clone data we need before entering closures to avoid borrow conflicts
                            let binding_clone = binding.clone();
                            let target = state.capture_target;

                            ui.separator();
                            ui.add_space(ITEM_SPACING / 2.0);

                            // Accept or Try Again buttons
                            let mut should_accept = false;
                            let mut should_retry = false;

                            ui.horizontal(|ui| {
                                if ui.button("✓ Accept").clicked() {
                                    should_accept = true;
                                }

                                if ui.button("✗ Try Again").clicked() {
                                    should_retry = true;
                                }
                            });

                            if should_accept {
                                // Apply the binding to profile
                                match target {
                                    Some(CaptureTarget::Forward) => {
                                        profile.cycle_forward_keys = Some(binding_clone);
                                        changed = true;
                                    }
                                    Some(CaptureTarget::Backward) => {
                                        profile.cycle_backward_keys = Some(binding_clone);
                                        changed = true;
                                    }
                                    None => {}
                                }
                                state.cancel_capture();
                            }

                            if should_retry {
                                if let Some(t) = target {
                                    state.start_key_capture(t);
                                }
                            }
                        }
                        CaptureResult::Cancelled => {
                            // This case is now handled automatically above - dialog closes immediately
                            // This branch should never be reached, but kept for exhaustiveness
                        }
                        CaptureResult::Timeout => {
                            ui.colored_label(egui::Color32::from_rgb(200, 100, 0), "Capture timed out (no key pressed)");
                            ui.add_space(ITEM_SPACING);
                            if ui.button("Close").clicked() {
                                state.cancel_capture();
                            }
                        }
                        CaptureResult::Error(err) => {
                            ui.colored_label(egui::Color32::from_rgb(200, 0, 0), format!("Error: {}", err));
                            ui.add_space(ITEM_SPACING);
                            if ui.button("Close").clicked() {
                                state.cancel_capture();
                            }
                        }
                    }
                } else {
                    // Still waiting for input
                    ui.separator();
                    ui.add_space(ITEM_SPACING / 2.0);
                    if ui.button("Cancel").clicked() {
                        state.cancel_capture();
                    }
                }
            });
    }

    changed
}

/// Parse hex color string - supports both #RRGGBB and #AARRGGBB formats
fn parse_hex_color(hex: &str) -> Result<egui::Color32, ()> {
    let hex = hex.trim_start_matches('#');
    
    match hex.len() {
        6 => {
            // RGB format - assume full opacity
            let rr = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
            let gg = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
            let bb = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
            Ok(egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 255))
        }
        8 => {
            // ARGB format
            let aa = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
            let rr = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
            let gg = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
            let bb = u8::from_str_radix(&hex[6..8], 16).map_err(|_| ())?;
            Ok(egui::Color32::from_rgba_unmultiplied(rr, gg, bb, aa))
        }
        _ => Err(()),
    }
}

/// Format egui Color32 to hex string (#AARRGGBB or #RRGGBB)
fn format_hex_color(color: egui::Color32) -> String {
    if color.a() == 255 {
        // Full opacity - use shorter RGB format
        format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
    } else {
        // Has transparency - use ARGB format
        format!("#{:02X}{:02X}{:02X}{:02X}", 
            color.a(), color.r(), color.g(), color.b())
    }
}
