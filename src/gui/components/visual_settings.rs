use eframe::egui;
use crate::config::profile::Profile;
use crate::constants::gui::*;

/// State for visual settings UI
pub struct VisualSettingsState {
    available_fonts: Vec<String>,
    font_load_error: Option<String>,
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

        Self {
            available_fonts,
            font_load_error,
        }
    }
}

impl Default for VisualSettingsState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn ui(ui: &mut egui::Ui, profile: &mut Profile, state: &mut VisualSettingsState) -> bool {
    let mut changed = false;

    ui.group(|ui| {
        ui.label(egui::RichText::new("Visual Settings").strong());
        ui.add_space(ITEM_SPACING);

        // Enable/disable thumbnail rendering
        if ui.checkbox(&mut profile.thumbnail_enabled, "Enable thumbnail previews").changed() {
            changed = true;
        }
        ui.label(egui::RichText::new(
            "When disabled, daemon still runs for hotkeys but thumbnails are not rendered")
            .small()
            .weak());

        ui.add_space(ITEM_SPACING);

        // Remaining settings are grayed out when thumbnails disabled
        ui.add_enabled_ui(profile.thumbnail_enabled, |ui| {
            // Opacity
            ui.horizontal(|ui| {
                ui.label("Opacity:");
                if ui.add(egui::Slider::new(&mut profile.thumbnail_opacity, 0..=100)
                    .suffix("%")).changed() {
                    changed = true;
                }
            });
        
        // Border toggle
        ui.horizontal(|ui| {
            ui.label("Borders:");
            if ui.checkbox(&mut profile.thumbnail_border, "Enabled").changed() {
                changed = true;
            }
        });
        
        // Border settings (greyed out if disabled)
        ui.indent("border_settings", |ui| {
            ui.add_enabled_ui(profile.thumbnail_border, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Border Size:");
                    if ui.add(egui::DragValue::new(&mut profile.thumbnail_border_size)
                        .range(1..=20)).changed() {
                        changed = true;
                    }
                });
                
                ui.horizontal(|ui| {
                    ui.label("Border Color:");
                    let text_edit = egui::TextEdit::singleline(&mut profile.thumbnail_border_color)
                        .desired_width(100.0);
                    if ui.add(text_edit).changed() {
                        changed = true;
                    }
                    
                    // Color picker button - parses hex string, shows picker, updates string
                    if let Ok(mut color) = parse_hex_color(&profile.thumbnail_border_color)
                        && ui.color_edit_button_srgba(&mut color).changed() {
                            profile.thumbnail_border_color = format_hex_color(color);
                            changed = true;
                        }
                });
            });
        });
        
        ui.add_space(ITEM_SPACING);
        
        // Text settings
        ui.horizontal(|ui| {
            ui.label("Text Size:");
            if ui.add(egui::DragValue::new(&mut profile.thumbnail_text_size)
                .range(8..=48)).changed() {
                changed = true;
            }
        });
        
        ui.horizontal(|ui| {
            ui.label("Text Position:");
            ui.label("X:");
            if ui.add(egui::DragValue::new(&mut profile.thumbnail_text_x)
                .range(0..=100)).changed() {
                changed = true;
            }
            ui.label("Y:");
            if ui.add(egui::DragValue::new(&mut profile.thumbnail_text_y)
                .range(0..=100)).changed() {
                changed = true;
            }
        });
        
        ui.horizontal(|ui| {
            ui.label("Text Color:");
            let text_edit = egui::TextEdit::singleline(&mut profile.thumbnail_text_color)
                .desired_width(100.0);
            if ui.add(text_edit).changed() {
                changed = true;
            }
            
            // Color picker button
            if let Ok(mut color) = parse_hex_color(&profile.thumbnail_text_color)
                && ui.color_edit_button_srgba(&mut color).changed() {
                    profile.thumbnail_text_color = format_hex_color(color);
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
                .selected_text(&profile.thumbnail_text_font)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for font_family in &state.available_fonts {
                        if ui.selectable_value(
                            &mut profile.thumbnail_text_font,
                            font_family.clone(),
                            font_family
                        ).changed() {
                            changed = true;
                        }
                    }
                });
        });
        }); // Close add_enabled_ui
    }); // Close group

    changed
}

/// Parse hex color string - supports both #RRGGBB and #AARRGGBB formats.
/// Returns a Color32 if parsing succeeds, treating 6-digit hex as full-opacity RGB.
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
