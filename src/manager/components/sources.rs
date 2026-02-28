use crate::config::profile::CustomWindowRule;
use crate::manager::x11_utils::{WindowInfo, get_running_applications};
use egui::{ScrollArea, Ui};
use std::collections::HashSet;

pub struct SourcesTab {
    // Component state
    new_rule: CustomWindowRule,
    running_apps: Option<Vec<WindowInfo>>,
    selected_app_idx: Option<usize>,
    error_msg: Option<String>,
    // Track expanded rows for editing: index -> expanded
    expanded_rows: HashSet<usize>,
}

impl Default for SourcesTab {
    fn default() -> Self {
        Self {
            new_rule: CustomWindowRule {
                title_pattern: None,
                class_pattern: None,
                alias: String::new(),
                default_width: crate::common::constants::defaults::thumbnail::WIDTH,
                default_height: crate::common::constants::defaults::thumbnail::HEIGHT,
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
                exempt_from_minimize: false,
                hotkey: None,
            },
            running_apps: None,
            selected_app_idx: None,
            error_msg: None,
            expanded_rows: HashSet::new(),
        }
    }
}

impl SourcesTab {
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        profile: &mut crate::config::profile::Profile,
        hotkey_state: &mut crate::manager::components::hotkey_settings::HotkeySettingsState,
    ) -> bool {
        let mut changed = false;

        ui.heading("Custom Sources");
        ui.label("Add external applications to preview. Applications must run in X11 or XWayland mode to be detected.");
        ui.label(
            egui::RichText::new("⚠ Feature is experimental")
                .weak()
                .small(),
        );
        ui.add_space(10.0);

        // -- Rules List (Expandable) --
        ui.group(|ui| {
            ui.heading("Configured Rules");
            ui.label(
                egui::RichText::new("Manage and edit your custom sources.")
                    .weak()
                    .small(),
            );
            ui.add_space(5.0);

            ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                if profile.custom_windows.is_empty() {
                    ui.label("No custom rules configured.");
                }

                let mut remove_idx = None;

                for (idx, rule) in profile.custom_windows.iter_mut().enumerate() {
                    let is_expanded = self.expanded_rows.contains(&idx);

                    ui.horizontal(|ui| {
                        let icon = if is_expanded { "v" } else { ">" };
                        if ui.small_button(icon).clicked() {
                            if is_expanded {
                                self.expanded_rows.remove(&idx);
                            } else {
                                self.expanded_rows.insert(idx);
                            }
                        }

                        ui.label(egui::RichText::new(&rule.alias).strong());

                        // Show brief details when collapsed
                        if !is_expanded {
                            let mut details = Vec::new();
                            if let Some(c) = &rule.class_pattern {
                                details.push(format!("Class: {}", c));
                            }
                            if let Some(t) = &rule.title_pattern {
                                details.push(format!("Title: {}", t));
                            }
                            ui.label(egui::RichText::new(details.join(", ")).weak());
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("🗑").on_hover_text("Delete Rule").clicked() {
                                remove_idx = Some(idx);
                                changed = true;
                            }

                            if rule.limit {
                                ui.colored_label(egui::Color32::LIGHT_BLUE, "(Single)");
                            }
                        });
                    });

                    if is_expanded {
                        ui.indent("rule_details", |ui| {
                            ui.add_space(4.0);
                            egui::Grid::new(format!("grid_edit_rule_{}", idx))
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(ui, |ui| {
                                    // Alias
                                    ui.label("Display Name:");
                                    if ui.text_edit_singleline(&mut rule.alias).changed() {
                                        changed = true;
                                    }
                                    ui.end_row();

                                    // Class Pattern
                                    ui.label("Class Pattern:");
                                    let mut class_text =
                                        rule.class_pattern.clone().unwrap_or_default();
                                    if ui.text_edit_singleline(&mut class_text).changed() {
                                        rule.class_pattern = if class_text.is_empty() {
                                            None
                                        } else {
                                            Some(class_text)
                                        };
                                        changed = true;
                                    }
                                    ui.end_row();

                                    // Title Pattern
                                    ui.label("Title Pattern:");
                                    let mut title_text =
                                        rule.title_pattern.clone().unwrap_or_default();
                                    if ui.text_edit_singleline(&mut title_text).changed() {
                                        rule.title_pattern = if title_text.is_empty() {
                                            None
                                        } else {
                                            Some(title_text)
                                        };
                                        changed = true;
                                    }
                                    ui.end_row();

                                    // Hotkey
                                    ui.label("Hotkey:");
                                    ui.horizontal(|ui| {
                                        if let Some(binding) = &rule.hotkey {
                                            ui.label(
                                                egui::RichText::new(binding.display_name())
                                                    .strong()
                                                    .color(ui.style().visuals.text_color()),
                                            );

                                            if ui
                                                .small_button("✖")
                                                .on_hover_text("Clear Hotkey")
                                                .clicked()
                                            {
                                                rule.hotkey = None;
                                                changed = true;
                                            }
                                        } else {
                                            ui.label(
                                                egui::RichText::new("Not set")
                                                    .weak()
                                                    .color(ui.style().visuals.weak_text_color()),
                                            );
                                        }

                                        let bind_text =
                                            if hotkey_state.is_capturing_custom_rule(&rule.alias) {
                                                "Capturing..."
                                            } else {
                                                "⌨ Bind"
                                            };

                                        if ui.button(bind_text).clicked() {
                                            hotkey_state.start_key_capture_for_custom_rule(
                                                rule.alias.clone(),
                                                profile.hotkey_backend,
                                            );
                                        }
                                    });
                                    ui.end_row();

                                    // --- Visual Overrides ---
                                    ui.label("Overrides:");
                                    ui.vertical(|ui| {
                                        // Active Border
                                        ui.horizontal(|ui| {
                                            ui.label("Active Border:");
                                            let mut enabled = rule.active_border_color.is_some()
                                                || rule.active_border_size.is_some();
                                            if ui.checkbox(&mut enabled, "Enabled").changed() {
                                                if enabled {
                                                    // Default to profile settings
                                                    rule.active_border_color = Some(
                                                        profile
                                                            .thumbnail_active_border_color
                                                            .clone(),
                                                    );
                                                    rule.active_border_size =
                                                        Some(profile.thumbnail_active_border_size);
                                                } else {
                                                    rule.active_border_color = None;
                                                    rule.active_border_size = None;
                                                }
                                                changed = true;
                                            }
                                        });
                                        if rule.active_border_color.is_some()
                                            || rule.active_border_size.is_some()
                                        {
                                            ui.indent("active_border_details", |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label("Color:");
                                                    let color = rule
                                                        .active_border_color
                                                        .clone()
                                                        .unwrap_or_else(|| {
                                                            profile
                                                                .thumbnail_active_border_color
                                                                .clone()
                                                        });
                                                    let mut egui_color =
                                                        crate::common::color::hex_to_color32(
                                                            &color,
                                                        )
                                                        .unwrap_or(egui::Color32::WHITE);
                                                    if ui
                                                        .color_edit_button_srgba(&mut egui_color)
                                                        .changed()
                                                    {
                                                        rule.active_border_color = Some(
                                                            crate::common::color::color32_to_hex(
                                                                egui_color,
                                                            ),
                                                        );
                                                        changed = true;
                                                    }
                                                });
                                                ui.horizontal(|ui| {
                                                    ui.label("Size:");
                                                    let mut size =
                                                        rule.active_border_size.unwrap_or(
                                                            profile.thumbnail_active_border_size,
                                                        );
                                                    if ui
                                                        .add(
                                                            egui::DragValue::new(&mut size)
                                                                .range(1..=20),
                                                        )
                                                        .changed()
                                                    {
                                                        rule.active_border_size = Some(size);
                                                        changed = true;
                                                    }
                                                });
                                            });
                                        }

                                        // Inactive Border
                                        ui.horizontal(|ui| {
                                            ui.label("Inactive Border:");
                                            let mut enabled = rule.inactive_border_color.is_some()
                                                || rule.inactive_border_size.is_some();
                                            if ui.checkbox(&mut enabled, "Enabled").changed() {
                                                if enabled {
                                                    rule.inactive_border_color = Some(
                                                        profile
                                                            .thumbnail_inactive_border_color
                                                            .clone(),
                                                    );
                                                    rule.inactive_border_size = Some(
                                                        profile.thumbnail_inactive_border_size,
                                                    );
                                                } else {
                                                    rule.inactive_border_color = None;
                                                    rule.inactive_border_size = None;
                                                }
                                                changed = true;
                                            }
                                        });
                                        if rule.inactive_border_color.is_some()
                                            || rule.inactive_border_size.is_some()
                                        {
                                            ui.indent("inactive_border_details", |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label("Color:");
                                                    let color = rule
                                                        .inactive_border_color
                                                        .clone()
                                                        .unwrap_or_else(|| {
                                                            profile
                                                                .thumbnail_inactive_border_color
                                                                .clone()
                                                        });
                                                    let mut egui_color =
                                                        crate::common::color::hex_to_color32(
                                                            &color,
                                                        )
                                                        .unwrap_or(egui::Color32::WHITE);
                                                    if ui
                                                        .color_edit_button_srgba(&mut egui_color)
                                                        .changed()
                                                    {
                                                        rule.inactive_border_color = Some(
                                                            crate::common::color::color32_to_hex(
                                                                egui_color,
                                                            ),
                                                        );
                                                        changed = true;
                                                    }
                                                });
                                                ui.horizontal(|ui| {
                                                    ui.label("Size:");
                                                    let mut size =
                                                        rule.inactive_border_size.unwrap_or(
                                                            profile.thumbnail_inactive_border_size,
                                                        );
                                                    if ui
                                                        .add(
                                                            egui::DragValue::new(&mut size)
                                                                .range(1..=20),
                                                        )
                                                        .changed()
                                                    {
                                                        rule.inactive_border_size = Some(size);
                                                        changed = true;
                                                    }
                                                });
                                            });
                                        }

                                        // Text Color
                                        ui.horizontal(|ui| {
                                            ui.label("Text Color:");
                                            let mut enabled = rule.text_color.is_some();
                                            if ui.checkbox(&mut enabled, "Enabled").changed() {
                                                if enabled {
                                                    rule.text_color =
                                                        Some(profile.thumbnail_text_color.clone());
                                                } else {
                                                    rule.text_color = None;
                                                }
                                                changed = true;
                                            }
                                        });
                                        if let Some(ref mut color_hex) = rule.text_color {
                                            ui.indent("text_color_details", |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label("Color:");
                                                    let mut egui_color =
                                                        crate::common::color::hex_to_color32(
                                                            color_hex,
                                                        )
                                                        .unwrap_or(egui::Color32::WHITE);
                                                    if ui
                                                        .color_edit_button_srgba(&mut egui_color)
                                                        .changed()
                                                    {
                                                        *color_hex =
                                                            crate::common::color::color32_to_hex(
                                                                egui_color,
                                                            );
                                                        changed = true;
                                                    }
                                                });
                                            });
                                        }

                                        // Preview Mode (Static Mode)
                                        ui.horizontal(|ui| {
                                            ui.label("Static Mode:");
                                            let mut is_static = matches!(
                                                rule.preview_mode,
                                                Some(
                                                    crate::common::types::PreviewMode::Static { .. }
                                                )
                                            );

                                            if ui.checkbox(&mut is_static, "Enabled").changed() {
                                                if is_static {
                                                    // Enable Static Mode (Default to Black)
                                                    rule.preview_mode = Some(
                                                        crate::common::types::PreviewMode::Static {
                                                            color: "#000000".to_string(),
                                                        },
                                                    );
                                                } else {
                                                    // Disable Static Mode (Revert to Live/None)
                                                    rule.preview_mode = None;
                                                }
                                                changed = true;
                                            }
                                        });

                                        // Static Mode Settings (Indented)
                                        if let Some(crate::common::types::PreviewMode::Static {
                                            ref mut color,
                                        }) = rule.preview_mode
                                        {
                                            ui.indent("static_mode_details", |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label("Color:");
                                                    let mut color_str = color.clone();
                                                    let text_edit =
                                                        egui::TextEdit::singleline(&mut color_str)
                                                            .desired_width(100.0);

                                                    if ui.add(text_edit).changed() {
                                                        *color = color_str.clone();
                                                        changed = true;
                                                    }

                                                    if let Ok(mut c) =
                                                        crate::manager::utils::parse_hex_color(
                                                            &color_str,
                                                        )
                                                        && ui
                                                            .color_edit_button_srgba(&mut c)
                                                            .changed()
                                                    {
                                                        let new_hex =
                                                            crate::manager::utils::format_hex_color(
                                                                c,
                                                            );
                                                        *color = new_hex;
                                                        changed = true;
                                                    }
                                                });
                                            });
                                        }

                                        // Minimize Exemption
                                        ui.horizontal(|ui| {
                                            ui.label("Minimize Exemption:");
                                            if ui
                                                .checkbox(&mut rule.exempt_from_minimize, "Enabled")
                                                .on_hover_text("If enabled, this source will not be minimized when switching to another window.")
                                                .changed()
                                            {
                                                changed = true;
                                            }
                                        });
                                    });
                                    ui.end_row();

                                    // --- Aspect Ratio Controls ---
                                    ui.label("Size & Ratio:");
                                    ui.vertical(|ui| {
                                        // Aspect Ratio Logic (Replicated from visual_settings.rs)
                                        let aspect_ratios = [
                                            ("16:9", 16.0 / 9.0),
                                            ("16:10", 16.0 / 10.0),
                                            ("4:3", 4.0 / 3.0),
                                            ("21:9", 21.0 / 9.0),
                                            ("Custom", 0.0),
                                        ];

                                        let current_ratio =
                                            rule.default_width as f32 / rule.default_height as f32;
                                        let detected_preset = {
                                            let mut preset = "Custom";
                                            for (name, ratio) in
                                                &aspect_ratios[..aspect_ratios.len() - 1]
                                            {
                                                if (current_ratio - ratio).abs() < 0.01 {
                                                    preset = name;
                                                    break;
                                                }
                                            }
                                            preset
                                        };

                                        // Persistent state for "Custom" mode
                                        let id = ui
                                            .make_persistent_id(format!("src_ratio_mode_{}", idx));
                                        let mut selected_mode = ui.data_mut(|d| {
                                            d.get_temp::<String>(id)
                                                .unwrap_or_else(|| detected_preset.to_string())
                                        });

                                        ui.horizontal(|ui| {
                                            let mut mode_changed = false;
                                            egui::ComboBox::from_id_salt(format!(
                                                "src_ratio_combo_{}",
                                                idx
                                            ))
                                            .selected_text(&selected_mode)
                                            .show_ui(
                                                ui,
                                                |ui| {
                                                    for (name, ratio) in &aspect_ratios {
                                                        if ui
                                                            .selectable_value(
                                                                &mut selected_mode,
                                                                name.to_string(),
                                                                *name,
                                                            )
                                                            .changed()
                                                        {
                                                            mode_changed = true;
                                                            if *ratio > 0.0 {
                                                                rule.default_height =
                                                                    (rule.default_width as f32
                                                                        / ratio)
                                                                        .round()
                                                                        as u16;
                                                                changed = true;
                                                            }
                                                        }
                                                    }
                                                },
                                            );
                                            if mode_changed {
                                                ui.data_mut(|d| {
                                                    d.insert_temp(id, selected_mode.clone())
                                                });
                                            }
                                        });

                                        ui.add_space(2.0);

                                        // Sliders
                                        // Width (Always active)
                                        ui.horizontal(|ui| {
                                            ui.label("Width:");
                                            if ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut rule.default_width,
                                                        100..=1200,
                                                    )
                                                    .suffix(" px"),
                                                )
                                                .changed()
                                            {
                                                changed = true;
                                                // Update height if locked
                                                if selected_mode != "Custom" {
                                                    for (name, ratio) in
                                                        &aspect_ratios[..aspect_ratios.len() - 1]
                                                    {
                                                        if name == &selected_mode.as_str() {
                                                            rule.default_height =
                                                                (rule.default_width as f32 / ratio)
                                                                    .round()
                                                                    as u16;
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        });

                                        // Height (Locked unless Custom)
                                        ui.horizontal(|ui| {
                                            ui.label("Height:");
                                            let is_custom = selected_mode == "Custom";

                                            ui.add_enabled_ui(is_custom, |ui| {
                                                if ui
                                                    .add(
                                                        egui::Slider::new(
                                                            &mut rule.default_height,
                                                            100..=1200,
                                                        )
                                                        .suffix(" px"),
                                                    )
                                                    .changed()
                                                {
                                                    changed = true;
                                                }
                                            });
                                        });

                                        ui.weak(format!(
                                            "Preview: {}x{}",
                                            rule.default_width, rule.default_height
                                        ));
                                    });
                                    ui.end_row();

                                    // Limit
                                    ui.label("Limit:");
                                    if ui.checkbox(&mut rule.limit, "Single Instance").changed() {
                                        changed = true;
                                    }
                                    ui.end_row();
                                });
                            ui.add_space(8.0);
                        });
                    }
                    ui.separator();
                }

                if let Some(idx) = remove_idx {
                    profile.custom_windows.remove(idx);
                    self.expanded_rows.remove(&idx);
                    changed = true;
                }
            });
        });

        ui.add_space(20.0);

        // -- Add New Rule Section --
        ui.group(|ui| {
            ui.heading("Add New Source");
            ui.label(
                egui::RichText::new("Configure a new application to preview.")
                    .weak()
                    .small(),
            );
            ui.add_space(5.0);

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

                let mut trigger_refresh = false;

                ui.push_id("app_picker_combo", |ui| {
                    egui::ComboBox::from_id_salt("app_picker")
                        .selected_text(combo_label)
                        .show_ui(ui, |ui| {
                            if ui.button("🔄 Refresh List").clicked() || self.running_apps.is_none()
                            {
                                trigger_refresh = true;
                            }

                            if let Some(msg) = &self.error_msg {
                                ui.colored_label(egui::Color32::RED, msg);
                            }

                            if let Some(apps) = &self.running_apps {
                                for (idx, app) in apps.iter().enumerate() {
                                    let text = format!("{} ({})", app.class, app.title);
                                    if ui
                                        .selectable_value(
                                            &mut self.selected_app_idx,
                                            Some(idx),
                                            &text,
                                        )
                                        .clicked()
                                    {
                                        // Auto-fill fields from selection
                                        self.new_rule.alias = app.class.clone();
                                        self.new_rule.class_pattern = Some(app.class.clone());
                                        self.new_rule.title_pattern = None;
                                    }
                                }
                            }
                        });
                });

                // Refresh button outside combobox for easy access
                if ui
                    .button("🔄")
                    .on_hover_text("Refresh application list")
                    .clicked()
                {
                    trigger_refresh = true;
                }

                if trigger_refresh {
                    match get_running_applications() {
                        Ok(mut apps) => {
                            // Filter out EVE clients to prevent duplication/confusion
                            apps.retain(|app| {
                                !app.title.starts_with("EVE - ") && app.title != "EVE"
                            });

                            // Dedup logic based on class+title
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

            egui::Grid::new("add_source_grid")
                .num_columns(2)
                .spacing([10.0, 8.0]) // Increased vertical spacing for cleaner look
                .show(ui, |ui| {
                    ui.label("Display Name:");
                    ui.text_edit_singleline(&mut self.new_rule.alias);
                    ui.end_row();

                    ui.label("Window Class Pattern:");
                    let mut class_text = self.new_rule.class_pattern.clone().unwrap_or_default();
                    if ui.text_edit_singleline(&mut class_text).changed() {
                        self.new_rule.class_pattern = if class_text.is_empty() {
                            None
                        } else {
                            Some(class_text)
                        };
                    }
                    ui.end_row();

                    ui.label("Window Title Pattern:");
                    let mut title_text = self.new_rule.title_pattern.clone().unwrap_or_default();
                    if ui.text_edit_singleline(&mut title_text).changed() {
                        self.new_rule.title_pattern = if title_text.is_empty() {
                            None
                        } else {
                            Some(title_text)
                        };
                    }
                    ui.end_row();

                    ui.label("");
                    ui.weak(
                        "A Display Name and at least one pattern (Class or Title) are required.",
                    );
                    ui.end_row();

                    ui.label("Limit:");
                    ui.checkbox(&mut self.new_rule.limit, "Limit to single instance")
                        .on_hover_text(
                            "If checked, only the first matching window will be previewed.",
                        );
                    ui.end_row();
                });

            ui.add_space(10.0);

            let is_valid = !self.new_rule.alias.is_empty()
                && (self.new_rule.class_pattern.is_some() || self.new_rule.title_pattern.is_some());

            ui.horizontal(|ui| {
                ui.add_enabled_ui(is_valid, |ui| {
                    if ui.button("Add Source").clicked() {
                        // Inherit global defaults for dimensions
                        self.new_rule.default_width = profile.thumbnail_default_width;
                        self.new_rule.default_height = profile.thumbnail_default_height;

                        profile.custom_windows.push(self.new_rule.clone());
                        changed = true;

                        // Reset form state
                        self.new_rule.alias.clear();
                        self.new_rule.class_pattern = None;
                        self.new_rule.title_pattern = None;
                        self.new_rule.limit = false;
                        self.new_rule.active_border_color = None;
                        self.new_rule.inactive_border_color = None;
                        self.new_rule.active_border_size = None;
                        self.new_rule.inactive_border_size = None;
                        self.new_rule.text_color = None;
                        self.new_rule.text_size = None;
                        self.new_rule.text_x = None;
                        self.new_rule.text_y = None;
                        self.new_rule.preview_mode = None;
                        self.new_rule.exempt_from_minimize = false;
                        self.new_rule.hotkey = None;
                    }
                });
            });
        });

        // Render Hotkey Modal if active
        if hotkey_state.is_dialog_open() {
            changed |= crate::manager::components::hotkey_settings::render_key_capture_modal(
                ui,
                profile,
                hotkey_state,
            );
        }

        changed
    }
}
