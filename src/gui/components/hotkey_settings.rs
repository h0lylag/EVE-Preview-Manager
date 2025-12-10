//! Hotkey settings component for profile configuration

use crate::config::profile::Profile;
use crate::constants::gui::*;
use crate::gui::key_capture::{self, CaptureResult, CaptureState};
use eframe::egui;
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CaptureTarget {
    Forward,
    Backward,
    Character(String), // Character name for per-character hotkey
}

/// State for hotkey settings UI
pub struct HotkeySettingsState {
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

impl HotkeySettingsState {
    pub fn new() -> Self {
        // Load available input devices at GUI startup
        let (available_devices, device_load_error) = match crate::preview::list_input_devices() {
            Ok(devices) => (devices, None),
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to load input device list");
                (Vec::new(), Some(e.to_string()))
            }
        };

        Self {
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

    /// Start capturing a key for the specified target.
    /// Spawns a background thread via `key_capture` to listen for raw input events.
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

    /// Public method for starting character-specific hotkey capture
    /// Used by cycle_order_settings component's per-character hotkeys tab
    pub fn start_key_capture_for_character(&mut self, character_name: String) {
        self.start_key_capture(CaptureTarget::Character(character_name));
    }

    /// Check if currently capturing for a specific character
    pub fn is_capturing_for(&self, character_name: &str) -> bool {
        if let Some(CaptureTarget::Character(ref target)) = self.capture_target {
            target == character_name && self.show_key_capture_dialog
        } else {
            false
        }
    }
}

impl Default for HotkeySettingsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders hotkey settings UI and returns true if changes were made
pub fn ui(ui: &mut egui::Ui, profile: &mut Profile, state: &mut HotkeySettingsState) -> bool {
    let mut changed = false;

    // Poll capture state updates if capture is active.
    // We use channels to receive async updates from the capture thread without blocking the GUI.
    if state.show_key_capture_dialog {
        // Check for final result first
        let mut got_result = false;
        if let Some(ref result_rx) = state.capture_result_rx
            && let Ok(result) = result_rx.try_recv()
        {
            // Auto-close on Escape (Cancelled)
            if matches!(result, CaptureResult::Cancelled) {
                state.cancel_capture();
                got_result = false;
            } else {
                state.capture_result = Some(result);
                got_result = true;
            }
        }

        // Update state
        if let Some(ref state_rx) = state.capture_state_rx {
            if got_result {
                let mut last_state = None;
                while let Ok(capture_state) = state_rx.try_recv() {
                    last_state = Some(capture_state);
                }
                if let Some(capture_state) = last_state {
                    state.current_capture_state = Some(capture_state);
                }
            } else if let Ok(capture_state) = state_rx.try_recv() {
                state.current_capture_state = Some(capture_state);
            }
        }
    }

    // Show capture error if any
    if let Some(ref error) = state.capture_error {
        ui.colored_label(egui::Color32::from_rgb(200, 0, 0), format!("⚠ {}", error));
        ui.add_space(ITEM_SPACING);
    }

    ui.group(|ui| {
        ui.label(egui::RichText::new("Hotkey Settings").strong());
        ui.add_space(ITEM_SPACING);

        // Input device selector - moved to top
        ui.label("Input device to monitor:");
        ui.add_space(ITEM_SPACING / 2.0);

        let selected_display = match profile.hotkey_input_device.as_deref() {
            None => "---".to_string(),
            Some("auto") => "Auto-Detect (Recommended)".to_string(),
            Some("all") => "All Devices".to_string(),
            Some(device_id) => {
                // This shouldn't happen with new system, but handle legacy configs
                state.available_devices.iter()
                    .find(|(id, _)| id == device_id)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| "Auto-Detect (Recommended)".to_string())
            }
        };

        egui::ComboBox::from_id_salt("hotkey_device_selector")
            .selected_text(&selected_display)
            .show_ui(ui, |ui| {
                if ui.selectable_value(&mut profile.hotkey_input_device, None, "---").clicked() {
                    changed = true;
                }

                if ui.selectable_value(&mut profile.hotkey_input_device, Some("auto".to_string()), "Auto-Detect (Recommended)").clicked() {
                    changed = true;
                }

                if ui.selectable_value(&mut profile.hotkey_input_device, Some("all".to_string()), "All Devices").clicked() {
                    changed = true;
                }
            });

        if let Some(ref error) = state.device_load_error {
            ui.add_space(ITEM_SPACING / 4.0);
            ui.label(egui::RichText::new(format!("⚠ {}", error)).small().color(egui::Color32::from_rgb(200, 100, 0)));
        }

        // Check if device is selected - needed for helper text
        let device_selected = profile.hotkey_input_device.is_some();

        // Show helper text for auto-detect mode
        if profile.hotkey_input_device.as_deref() == Some("auto") {
            ui.add_space(ITEM_SPACING / 4.0);
            ui.label(egui::RichText::new("Devices will be automatically detected when you bind keys")
                .small()
                .weak());
        }

        // Show helper text for all devices mode
        if profile.hotkey_input_device.as_deref() == Some("all") {
            ui.add_space(ITEM_SPACING / 4.0);
            ui.label(egui::RichText::new("Hotkeys will work from any connected input device")
                .small()
                .weak());
        }

        // Show message when device not selected
        if !device_selected {
            ui.add_space(ITEM_SPACING / 4.0);
            ui.label(egui::RichText::new("Select an input device above to configure hotkeys")
                .small()
                .weak());
        }

        ui.add_space(ITEM_SPACING);
        ui.separator();
        ui.add_space(ITEM_SPACING);

        ui.add_enabled_ui(device_selected, |ui| {
            // Hotkey Bindings
            ui.label("Configure keys for cycling through characters:");
            ui.add_space(ITEM_SPACING / 2.0);

            // Forward key binding
            ui.horizontal(|ui| {
                ui.label("Forward cycle:");
                let binding_text = profile.hotkey_cycle_forward.as_ref()
                    .map(|b| b.display_name())
                    .unwrap_or_else(|| "Not set".to_string());
                let color = if profile.hotkey_cycle_forward.is_none() {
                    egui::Color32::from_rgb(200, 100, 100)
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
                let binding_text = profile.hotkey_cycle_backward.as_ref()
                    .map(|b| b.display_name())
                    .unwrap_or_else(|| "Not set".to_string());
                let color = if profile.hotkey_cycle_backward.is_none() {
                    egui::Color32::from_rgb(200, 100, 100)
                } else {
                    ui.style().visuals.text_color()
                };
                ui.label(egui::RichText::new(binding_text).strong().color(color));
                if ui.button("🎹 Bind Key").clicked() {
                    state.start_key_capture(CaptureTarget::Backward);
                }
            });

            ui.add_space(ITEM_SPACING);
            ui.separator();
            ui.add_space(ITEM_SPACING);

            // Require EVE focus checkbox
            if ui.checkbox(&mut profile.hotkey_require_eve_focus,
                "Require EVE window focus").changed() {
                changed = true;
            }

            ui.label(egui::RichText::new(
                "When enabled, cycle hotkeys only work when an EVE window is focused")
                .small()
                .weak());

            ui.add_space(ITEM_SPACING);

            // Logged-out cycling checkbox
            if ui.checkbox(
                &mut profile.hotkey_logged_out_cycle,
                "Include logged-out characters in cycle"
            ).changed() {
                changed = true;
            }

            ui.add_space(ITEM_SPACING / 4.0);

            ui.label(egui::RichText::new(
                "When enabled, characters that log out will remain in the cycle using their last position")
                .small()
                .weak());
        });
    });

    // Key Capture Dialog
    if state.show_key_capture_dialog {
        egui::Window::new("🎹 Capture Hotkey")
            .collapsible(false)
            .resizable(false)
            .fixed_size([370.0, 280.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                let target_name = match state.capture_target {
                    Some(CaptureTarget::Forward) => "Forward Cycle".to_string(),
                    Some(CaptureTarget::Backward) => "Backward Cycle".to_string(),
                    Some(CaptureTarget::Character(ref name)) => format!("Character: {}", name),
                    None => "Unknown".to_string(),
                };

                ui.label(format!("Binding key for: {}", target_name));
                ui.add_space(ITEM_SPACING);

                // Show current capture state
                if let Some(ref capture_state) = state.current_capture_state {
                    ui.group(|ui| {
                        ui.set_min_width(320.0);
                        ui.vertical_centered(|ui| {
                            ui.add_space(ITEM_SPACING);
                            ui.label(
                                egui::RichText::new(&capture_state.description)
                                    .size(20.0)
                                    .strong(),
                            );
                            ui.add_space(ITEM_SPACING);
                        });
                    });
                } else {
                    ui.label("Initializing capture...");
                }

                ui.add_space(ITEM_SPACING);

                // Reserve space for device list (shown after capture)
                // This prevents the modal from shifting when devices are displayed
                if let Some(CaptureResult::Captured(binding)) = &state.capture_result {
                    if !binding.source_devices.is_empty() {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(egui::RichText::new("Detected on:").weak().small());
                        for device_id in &binding.source_devices {
                            // Format device ID to be more readable
                            let friendly_device = device_id
                                .replace("-event-kbd", " (Keyboard)")
                                .replace("-event-mouse", " (Mouse)")
                                .replace("_", " ")
                                .replace("-", " ");
                            ui.label(
                                egui::RichText::new(format!("  • {}", friendly_device))
                                    .weak()
                                    .small(),
                            );
                        }
                        ui.spacing_mut().item_spacing.y = 4.0;
                    } else {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label("");
                        ui.label("");
                        ui.label("");
                        ui.spacing_mut().item_spacing.y = 4.0;
                    }
                } else {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label("");
                    ui.label("");
                    ui.label("");
                    ui.spacing_mut().item_spacing.y = 4.0;
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
                            let binding_clone = binding.clone();
                            let target = state.capture_target.clone();

                            ui.separator();
                            ui.add_space(ITEM_SPACING / 2.0);

                            let mut should_accept = false;
                            let mut should_retry = false;
                            let mut should_cancel = false;

                            // Check for Escape key to cancel at confirmation step
                            ui.input(|i| {
                                if i.key_pressed(egui::Key::Escape) {
                                    should_cancel = true;
                                }
                            });

                            ui.horizontal(|ui| {
                                if ui.button("💾 Accept").clicked() {
                                    should_accept = true;
                                }

                                if ui.button("⟲ Try Again").clicked() {
                                    should_retry = true;
                                }
                            });

                            if should_cancel {
                                state.cancel_capture();
                            } else if should_accept {
                                match target {
                                    Some(CaptureTarget::Forward) => {
                                        profile.hotkey_cycle_forward = Some(binding_clone);
                                        changed = true;
                                    }
                                    Some(CaptureTarget::Backward) => {
                                        profile.hotkey_cycle_backward = Some(binding_clone);
                                        changed = true;
                                    }
                                    Some(CaptureTarget::Character(ref char_name)) => {
                                        profile
                                            .character_hotkeys
                                            .insert(char_name.clone(), binding_clone);
                                        changed = true;
                                    }
                                    None => {}
                                }
                                state.cancel_capture();
                            }

                            if should_retry && let Some(ref t) = target {
                                state.start_key_capture(t.clone());
                            }
                        }
                        CaptureResult::Cancelled => {
                            // Handled automatically above
                        }
                        CaptureResult::Timeout => {
                            ui.colored_label(
                                egui::Color32::from_rgb(200, 100, 0),
                                "Capture timed out (no key pressed)",
                            );
                            ui.add_space(ITEM_SPACING);
                            if ui.button("Close").clicked() {
                                state.cancel_capture();
                            }
                        }
                        CaptureResult::Error(err) => {
                            ui.colored_label(
                                egui::Color32::from_rgb(200, 0, 0),
                                format!("Error: {}", err),
                            );
                            ui.add_space(ITEM_SPACING);
                            if ui.button("Close").clicked() {
                                state.cancel_capture();
                            }
                        }
                    }
                } else {
                    ui.separator();
                    ui.add_space(ITEM_SPACING / 2.0);
                    if ui.button("✖ Cancel").clicked() {
                        state.cancel_capture();
                    }
                }
            });
    }

    changed
}
