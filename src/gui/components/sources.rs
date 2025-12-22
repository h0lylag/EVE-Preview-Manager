use egui::{Ui, ScrollArea};
use crate::config::profile::CustomWindowRule;
use crate::gui::x11_utils::{get_running_applications, WindowInfo};

pub struct SourcesTab {
    // Component state
    new_rule: CustomWindowRule,
    running_apps: Option<Vec<WindowInfo>>,
    selected_app_idx: Option<usize>,
    error_msg: Option<String>,
}

impl Default for SourcesTab {
    fn default() -> Self {
        Self {
            new_rule: CustomWindowRule {
                title_pattern: None,
                class_pattern: None,
                alias: String::new(),
                default_width: crate::constants::defaults::thumbnail::WIDTH,
                default_height: crate::constants::defaults::thumbnail::HEIGHT,
                limit: false,
            },
            running_apps: None,
            selected_app_idx: None,
            error_msg: None,
        }
    }
}

impl SourcesTab {
    pub fn ui(&mut self, ui: &mut Ui, profile: &mut crate::config::profile::Profile) -> bool {
        let mut changed = false;

        ui.heading("Custom Sources");
        ui.label("Add external applications to preview.");
        ui.colored_label(
            egui::Color32::YELLOW,
            "⚠ Applications must run in X11 or XWayland mode to be detected.",
        );
        ui.add_space(10.0);

        // -- Rules List --
        ui.group(|ui| {
            ui.heading("Configured Rules");
            ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                if profile.custom_windows.is_empty() {
                    ui.label("No custom rules configured.");
                }

                let mut remove_idx = None;

                for (idx, rule) in profile.custom_windows.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("\"{}\"", rule.alias));
                        
                        let details = format!(
                            "[{}]", 
                            vec![
                                rule.class_pattern.as_ref().map(|p| format!("Class: {}", p)),
                                rule.title_pattern.as_ref().map(|p| format!("Title: {}", p)),
                            ]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(", ")
                        );
                        ui.weak(details);

                        if rule.limit {
                            ui.colored_label(egui::Color32::LIGHT_BLUE, "(Single)");
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                           if ui.button("🗑").clicked() {
                               remove_idx = Some(idx);
                               changed = true;
                           }
                        });
                    });
                    ui.separator();
                }

                if let Some(idx) = remove_idx {
                    profile.custom_windows.remove(idx);
                }
            });
        });

        ui.add_space(20.0);

        // -- Add New Rule Section --
        ui.group(|ui| {
            ui.heading("Add New Source");
            
            // Window Picker
            ui.horizontal(|ui| {
                let combo_label = if let Some(apps) = &self.running_apps
                    && let Some(idx) = self.selected_app_idx
                    && idx < apps.len() 
                {
                    format!("{} ({})", apps[idx].class, apps[idx].title)
                } else {
                    "Select from running applications...".to_string()
                };

                egui::ComboBox::from_id_source("app_picker")
                    .selected_text(combo_label)
                    .show_ui(ui, |ui| {
                        // Refresh logic handled by parent container or lazily on open?
                        // egui ComboBox content is immediate.
                        if ui.button("🔄 Refresh List").clicked() || self.running_apps.is_none() {
                             match get_running_applications() {
                                 Ok(mut apps) => {
                                     // Filter out EVE Preview Manager and EVE processes (optional, but good for clarity)
                                     // For now just removing ourselves
                                     // ... filtering handled in x11_utils
                                     
                                     // Dedup logic? windows might have same class/title
                                     apps.dedup_by(|a, b| a.class == b.class && a.title == b.title);
                                     self.running_apps = Some(apps);
                                     self.error_msg = None;
                                 }
                                 Err(e) => {
                                     self.error_msg = Some(format!("Failed to list apps: {}", e));
                                 }
                             }
                        }
                        
                        if let Some(msg) = &self.error_msg {
                            ui.colored_label(egui::Color32::RED, msg);
                        }

                        if let Some(apps) = &self.running_apps {
                             for (idx, app) in apps.iter().enumerate() {
                                 let text = format!("{} ({})", app.class, app.title);
                                 // Truncate if too long?
                                 if ui.selectable_value(&mut self.selected_app_idx, Some(idx), &text).clicked() {
                                     // Auto-fill fields
                                     // Use Class as Alias (more stable than dynamic titles)
                                     self.new_rule.alias = app.class.clone();
                                     self.new_rule.class_pattern = Some(app.class.clone());
                                     // Do NOT set title pattern by default. Titles change (e.g. browsers), causing mismatches.
                                     // Users can add a title pattern manually if they want to match a specific window.
                                     self.new_rule.title_pattern = None;
                                 }
                             }
                        }
                    });
                
                // Refresh button outside combobox too for accessibility
                 if ui.button("🔄").on_hover_text("Refresh application list").clicked() {
                     match get_running_applications() {
                         Ok(mut apps) => {
                             apps.dedup_by(|a, b| a.class == b.class && a.title == b.title);
                             self.running_apps = Some(apps);
                             self.error_msg = None;
                         }
                         Err(e) => {
                             self.error_msg = Some(format!("Failed to list apps: {}", e));
                         }
                     }
                 }
            });
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Display Name:");
                ui.text_edit_singleline(&mut self.new_rule.alias);
            });
            
            ui.horizontal(|ui| {
                ui.label("Window Class Pattern:");
                let mut class_text = self.new_rule.class_pattern.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut class_text).changed() {
                    self.new_rule.class_pattern = if class_text.is_empty() { None } else { Some(class_text) };
                }
            });

            ui.horizontal(|ui| {
                ui.label("Window Title Pattern:");
                let mut title_text = self.new_rule.title_pattern.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut title_text).changed() {
                    self.new_rule.title_pattern = if title_text.is_empty() { None } else { Some(title_text) };
                }
            });
            ui.weak("Leave Title Pattern empty to match any window of this application.");

            ui.horizontal(|ui| {
                ui.label("Default Size:");
                ui.add(egui::DragValue::new(&mut self.new_rule.default_width).prefix("W: "));
                ui.add(egui::DragValue::new(&mut self.new_rule.default_height).prefix("H: "));
            });

            ui.checkbox(&mut self.new_rule.limit, "Limit to single instance")
                .on_hover_text("If checked, only the first matching window will be previewed.");

            ui.add_space(5.0);

            let is_valid = !self.new_rule.alias.is_empty() && 
                          (self.new_rule.class_pattern.is_some() || self.new_rule.title_pattern.is_some());
            
            ui.add_enabled_ui(is_valid, |ui| {
                if ui.button("Add Source").clicked() {
                    profile.custom_windows.push(self.new_rule.clone());
                    changed = true;
                    // Reset form
                    self.new_rule.alias.clear();
                    self.new_rule.class_pattern = None;
                    self.new_rule.title_pattern = None;
                    self.new_rule.limit = false;
                }
            });
            if !is_valid {
                ui.weak(" Name and at least one pattern required.");
            }
        });

        changed
    }
}
