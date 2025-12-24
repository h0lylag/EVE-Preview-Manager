//! Hotkey settings component for profile configuration

use crate::config::HotkeyBackendType;
use crate::config::profile::Profile;
use crate::constants::gui::*;
use crate::gui::key_capture::{self, CaptureResult, CaptureState};
use eframe::egui;
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, PartialEq, Eq)]
enum CaptureTarget {
    ToggleSkip,        // Hotkey to temporarily skip current character
    Profile,           // Hotkey to switch to this profile
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
    cancel_capture_tx: Option<std::sync::mpsc::Sender<()>>,
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
            cancel_capture_tx: None,
            current_capture_state: None,
            capture_result: None,
            capture_error: None,
        }
    }

    /// Start capturing a key for the specified target.
    /// Spawns a background thread via `key_capture` to listen for raw input events.
    fn start_key_capture(
        &mut self,
        target: CaptureTarget,
        backend: crate::config::HotkeyBackendType,
    ) {
        // Ensure any previous capture is cancelled first
        self.cancel_capture();

        match key_capture::start_capture(backend) {
            Ok((state_rx, result_rx, cancel_tx)) => {
                self.show_key_capture_dialog = true;
                self.capture_target = Some(target);
                self.capture_state_rx = Some(state_rx);
                self.capture_result_rx = Some(result_rx);
                self.cancel_capture_tx = Some(cancel_tx);
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
        if let Some(tx) = self.cancel_capture_tx.take() {
            let _ = tx.send(());
        }
        self.show_key_capture_dialog = false;
        self.capture_target = None;
        self.capture_state_rx = None;
        self.capture_result_rx = None;
        self.current_capture_state = None;
        self.capture_result = None;
    }

    /// Public method for starting character-specific hotkey capture
    /// Used by cycle_order_settings component's per-character hotkeys tab
    pub fn start_key_capture_for_character(
        &mut self,
        character_name: String,
        backend: crate::config::HotkeyBackendType,
    ) {
        self.start_key_capture(CaptureTarget::Character(character_name), backend);
    }

    pub fn is_capturing_for(&self, character_name: &str) -> bool {
        if let Some(CaptureTarget::Character(ref target)) = self.capture_target {
            target == character_name && self.show_key_capture_dialog
        } else {
            false
        }
    }

    /// Check if the key capture dialog is currently open
    pub fn is_dialog_open(&self) -> bool {
        self.show_key_capture_dialog
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
    // logic moved to render_key_capture_modal

    // Show capture error if any
    if let Some(ref error) = state.capture_error {
        ui.colored_label(egui::Color32::from_rgb(200, 0, 0), format!("⚠ {}", error));
        ui.add_space(ITEM_SPACING);
    }

    ui.columns(2, |columns| {
        // --- Column 1: General & Cycle Settings ---
        columns[0].group(|ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("General Settings").strong());
            ui.add_space(ITEM_SPACING);

            // Backend selector
            ui.label("Hotkey Backend:");
            ui.add_space(ITEM_SPACING / 2.0);

            use crate::config::HotkeyBackendType;
            let backend_display = match profile.hotkey_backend {
                HotkeyBackendType::X11 => "X11 (Recommended)",
                HotkeyBackendType::Evdev => "evdev (Advanced - Requires Permissions)",
            };

            egui::ComboBox::from_id_salt("hotkey_backend_selector")
                .selected_text(backend_display)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut profile.hotkey_backend, HotkeyBackendType::X11, "X11 (Recommended)").clicked() {
                        changed = true;
                    }
                    if ui.selectable_value(&mut profile.hotkey_backend, HotkeyBackendType::Evdev, "evdev (Advanced - Requires Permissions)").clicked() {
                        changed = true;
                    }
                });

            ui.add_space(ITEM_SPACING / 4.0);

            // Show backend capabilities and warnings
            match profile.hotkey_backend {
                HotkeyBackendType::X11 => {
                    // No extra info needed for X11
                }
                HotkeyBackendType::Evdev => {
                    ui.label(egui::RichText::new("⚠ Security Warning: evdev backend requires 'input' group membership.").small());
                }
            }

            ui.add_space(ITEM_SPACING);
            ui.separator();
            ui.add_space(ITEM_SPACING);

            // Input device selector (only shown for evdev backend)
            if profile.hotkey_backend == HotkeyBackendType::Evdev {
                ui.label("Input device to monitor:");
                ui.add_space(ITEM_SPACING / 2.0);

                let selected_display = match profile.hotkey_input_device.as_deref() {
                    None => "---".to_string(),
                    Some("auto") => "Auto-Detect (Recommended)".to_string(),
                    Some("all") => "All Devices".to_string(),
                    Some(device_id) => {
                        state.available_devices.iter()
                            .find(|(id, _)| id == device_id)
                            .map(|(_, name)| name.clone())
                            .unwrap_or_else(|| "Auto-Detect (Recommended)".to_string())
                    }
                };

                egui::ComboBox::from_id_salt("hotkey_device_selector")
                    .selected_text(&selected_display)
                    .width(ui.available_width())
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

                // Show helper text for auto-detect mode
                if profile.hotkey_input_device.as_deref() == Some("auto") {
                    ui.add_space(ITEM_SPACING / 4.0);
                    ui.label(egui::RichText::new("Devices will be automatically detected when you bind keys").small().weak());
                }

                // Show helper text for all devices mode
                if profile.hotkey_input_device.as_deref() == Some("all") {
                    ui.add_space(ITEM_SPACING / 4.0);
                    ui.label(egui::RichText::new("Hotkeys will work from any connected input device").small().weak());
                }

                 ui.add_space(ITEM_SPACING);
                 ui.separator();
                 ui.add_space(ITEM_SPACING);
            }

            // For X11 backend, device selection is not applicable
            let device_selected = match profile.hotkey_backend {
                HotkeyBackendType::X11 => true, // Always enabled for X11
                HotkeyBackendType::Evdev => profile.hotkey_input_device.is_some(),
            };

            ui.add_enabled_ui(device_selected, |ui| {
                // Require EVE focus checkbox
                if ui.checkbox(&mut profile.hotkey_require_eve_focus, "Require EVE window focus").changed() {
                    changed = true;
                }
                ui.label(egui::RichText::new("Cycle hotkeys only work when an EVE window is focused").small().weak());

                ui.add_space(ITEM_SPACING);

                // Logged-out cycling checkbox
                if ui.checkbox(&mut profile.hotkey_logged_out_cycle, "Include logged-out characters").changed() {
                    changed = true;
                }
                ui.label(egui::RichText::new("Characters that log out will remain in the cycle").small().weak());
            });
        });

        // --- Column 2: Profile Settings ---
        columns[1].group(|ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("Other Hotkeys").strong());
            ui.add_space(ITEM_SPACING);

            // For X11 backend, device selection is not applicable (duplicated logic for right column enabled state)
            let device_selected = match profile.hotkey_backend {
                HotkeyBackendType::X11 => true,
                HotkeyBackendType::Evdev => profile.hotkey_input_device.is_some(),
            };

            ui.add_enabled_ui(device_selected, |ui| {
                 ui.label("Load Profile Hotkey:");
                 ui.add_space(ITEM_SPACING / 2.0);

                 ui.horizontal(|ui| {
                    let binding_text = profile.hotkey_profile_switch.as_ref()
                        .map(|b| b.display_name())
                        .unwrap_or_else(|| "Not set".to_string());

                    let color = if profile.hotkey_profile_switch.is_none() {
                        ui.style().visuals.weak_text_color() // Less critical than cycle keys
                    } else {
                        ui.style().visuals.text_color()
                    };

                    ui.label(egui::RichText::new(binding_text).strong().color(color));

                    if ui.button("⌨ Bind").clicked() {
                        state.start_key_capture(CaptureTarget::Profile, profile.hotkey_backend);
                    }

                     if profile.hotkey_profile_switch.is_some() && ui.small_button("✖").on_hover_text("Clear binding").clicked() {
                        profile.hotkey_profile_switch = None;
                        changed = true;
                    }
                 });

                 ui.add_space(ITEM_SPACING);
                 ui.label(egui::RichText::new("Pressing this hotkey will immediately switch to this profile.").weak().small());

                 ui.add_space(ITEM_SPACING);
                 ui.separator();
                 ui.add_space(ITEM_SPACING);

                 // Toggle Skip Hotkey
                 ui.label("Toggle Skip Hotkey:");
                 ui.add_space(ITEM_SPACING / 2.0);

                 ui.horizontal(|ui| {
                    let binding_text = profile.hotkey_toggle_skip.as_ref()
                        .map(|b| b.display_name())
                        .unwrap_or_else(|| "Not set".to_string());

                    // Use default text color if set, weak if not (optional feature)
                    let color = if profile.hotkey_toggle_skip.is_none() {
                         ui.style().visuals.weak_text_color()
                    } else {
                        ui.style().visuals.text_color()
                    };

                    ui.label(egui::RichText::new(binding_text).strong().color(color));

                    if ui.button("⌨ Bind").clicked() {
                        state.start_key_capture(CaptureTarget::ToggleSkip, profile.hotkey_backend);
                    }

                    if profile.hotkey_toggle_skip.is_some() && ui.small_button("✖").on_hover_text("Clear binding").clicked() {
                        profile.hotkey_toggle_skip = None;
                        changed = true;
                    }
                 });
                 ui.add_space(ITEM_SPACING);
                 ui.label(egui::RichText::new("Temporarily skip the current character from cycling.").weak().small());


                 if profile.hotkey_backend == HotkeyBackendType::Evdev {
                      ui.add_space(ITEM_SPACING);
                      ui.label(egui::RichText::new("Note: Global profile hotkeys require the Evdev backend to work reliably when the EVE client is not focused.").weak().small().italics());
                 }
            });
         });
    });

    // Key Capture Dialog
    if state.show_key_capture_dialog {
        changed |= render_key_capture_modal(ui, profile, state);
    }

    changed
}

/// Renders the key capture modal dialog
/// Returns true if changes were made (e.g. key bound)
pub fn render_key_capture_modal(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut HotkeySettingsState,
) -> bool {
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

    egui::Window::new("⌨ Capture Key")
        .collapsible(false)
        .resizable(false)
        .fixed_size([370.0, 280.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            let target_name = match state.capture_target {
                Some(CaptureTarget::ToggleSkip) => "Toggle Skip".to_string(),
                Some(CaptureTarget::Profile) => "Switch to Profile".to_string(),
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
                                Some(CaptureTarget::ToggleSkip) => {
                                    profile.hotkey_toggle_skip = Some(binding_clone);
                                    changed = true;
                                }
                                Some(CaptureTarget::Profile) => {
                                    profile.hotkey_profile_switch = Some(binding_clone);
                                    changed = true;
                                }
                                Some(CaptureTarget::Character(ref char_name)) => {
                                    // Check for special Cycle Group binding protocol
                                    if char_name.starts_with("GROUP:") {
                                        // Format: GROUP:<index>:FWD or GROUP:<index>:BWD
                                        let parts: Vec<&str> = char_name.split(':').collect();
                                        #[allow(clippy::collapsible_if)]
                                        if parts.len() == 3 {
                                            if let Ok(idx) = parts[1].parse::<usize>()
                                                && idx < profile.cycle_groups.len()
                                            {
                                                match parts[2] {
                                                    "FWD" => {
                                                        profile.cycle_groups[idx].hotkey_forward =
                                                            Some(binding_clone);
                                                        changed = true;
                                                    }
                                                    "BWD" => {
                                                        profile.cycle_groups[idx].hotkey_backward =
                                                            Some(binding_clone);
                                                        changed = true;
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                    } else {
                                        // Standard character hotkey
                                        profile
                                            .character_hotkeys
                                            .insert(char_name.clone(), binding_clone);
                                        changed = true;
                                    }
                                }
                                None => {}
                            }
                            state.cancel_capture();
                        }

                        if should_retry && let Some(ref t) = target {
                            state.start_key_capture(t.clone(), profile.hotkey_backend);
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

    changed
}
