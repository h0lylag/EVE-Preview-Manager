use serde::Deserialize;
use std::collections::HashMap;

use crate::common::types::CharacterSettings;
use crate::config::profile::{
    CustomWindowRule, CycleGroup, HotkeyBackendType, Profile,
    default_auto_save_thumbnail_positions, default_border_enabled, default_border_size,
    default_hotkey_backend, default_inactive_border_color, default_inactive_border_enabled,
    default_preserve_thumbnail_position_on_swap, default_profile_name, default_snap_threshold,
    default_text_font_family, default_thumbnail_enabled, default_thumbnail_height,
    default_thumbnail_width,
};

/// Helper struct for migration during deserialization
#[derive(Deserialize)]
struct ProfileHelper {
    #[serde(default = "default_profile_name")]
    profile_name: String,
    #[serde(default)]
    profile_description: String,
    #[serde(default = "default_thumbnail_width")]
    thumbnail_default_width: u16,
    #[serde(default = "default_thumbnail_height")]
    thumbnail_default_height: u16,
    #[serde(default = "default_thumbnail_enabled")]
    thumbnail_enabled: bool,
    thumbnail_opacity: u8,
    #[serde(default = "default_border_enabled", alias = "thumbnail_border")]
    thumbnail_active_border: bool,
    #[serde(alias = "thumbnail_border_size")]
    thumbnail_active_border_size: u16,
    #[serde(alias = "thumbnail_border_color")]
    thumbnail_active_border_color: String,
    #[serde(default = "default_inactive_border_enabled")]
    thumbnail_inactive_border: bool,
    #[serde(
        alias = "thumbnail_inactive_border_size",
        default = "default_border_size"
    )]
    thumbnail_inactive_border_size: u16,
    #[serde(default = "default_inactive_border_color")]
    thumbnail_inactive_border_color: String,
    thumbnail_text_size: u16,
    thumbnail_text_x: i16,
    thumbnail_text_y: i16,
    #[serde(default = "default_text_font_family")]
    thumbnail_text_font: String,
    thumbnail_text_color: String,
    #[serde(default = "default_auto_save_thumbnail_positions")]
    thumbnail_auto_save_position: bool,
    #[serde(default = "default_snap_threshold")]
    thumbnail_snap_threshold: u16,
    #[serde(default)]
    thumbnail_hide_not_focused: bool,
    #[serde(default = "default_preserve_thumbnail_position_on_swap")]
    thumbnail_preserve_position_on_swap: bool,
    #[serde(default)]
    client_minimize_on_switch: bool,
    #[serde(default)]
    client_minimize_show_overlay: bool,
    #[serde(default = "default_hotkey_backend")]
    hotkey_backend: HotkeyBackendType,
    #[serde(default)]
    hotkey_input_device: Option<String>,
    #[serde(default)]
    hotkey_logged_out_cycle: bool,
    #[serde(default)]
    hotkey_require_eve_focus: bool,
    #[serde(default)]
    hotkey_cycle_reset_index: bool,
    #[serde(default)]
    hotkey_profile_switch: Option<crate::config::HotkeyBinding>,
    #[serde(default)]
    hotkey_toggle_skip: Option<crate::config::HotkeyBinding>,
    #[serde(default)]
    hotkey_toggle_previews: Option<crate::config::HotkeyBinding>,
    #[serde(default)]
    character_hotkeys: HashMap<String, crate::config::HotkeyBinding>,
    #[serde(default)]
    character_thumbnails: HashMap<String, CharacterSettings>,
    // New field for custom source storage
    #[serde(default)]
    custom_source_thumbnails: HashMap<String, CharacterSettings>,
    #[serde(default)]
    custom_windows: Vec<CustomWindowRule>,

    // New field
    #[serde(default)]
    cycle_groups: Vec<CycleGroup>,

    // Legacy fields for migration
    #[serde(default)]
    hotkey_cycle_forward: Option<crate::config::HotkeyBinding>,
    #[serde(default)]
    hotkey_cycle_backward: Option<crate::config::HotkeyBinding>,
    #[serde(default)]
    hotkey_cycle_group: Vec<String>,
}

impl From<ProfileHelper> for Profile {
    fn from(helper: ProfileHelper) -> Self {
        let mut cycle_groups = helper.cycle_groups;

        // Migration logic:
        // If we have legacy fields but no cycle groups, create a "Default" group from them
        if cycle_groups.is_empty()
            && (!helper.hotkey_cycle_group.is_empty()
                || helper.hotkey_cycle_forward.is_some()
                || helper.hotkey_cycle_backward.is_some())
        {
            cycle_groups.push(CycleGroup {
                name: "Default".to_string(),
                cycle_list: helper
                    .hotkey_cycle_group
                    .into_iter()
                    .map(|name| {
                        if helper.custom_windows.iter().any(|w| w.alias == name) {
                            crate::config::profile::CycleSlot::Source(name)
                        } else {
                            crate::config::profile::CycleSlot::Eve(name)
                        }
                    })
                    .collect(),
                hotkey_forward: helper.hotkey_cycle_forward,
                hotkey_backward: helper.hotkey_cycle_backward,
            });
        }

        // Ensure at least one group exists
        if cycle_groups.is_empty() {
            cycle_groups.push(CycleGroup::default_group());
        }

        // Enforce separation: Ensure no custom sources remain in character_thumbnails
        let mut character_thumbnails = helper.character_thumbnails;
        let mut custom_source_thumbnails = helper.custom_source_thumbnails;

        let custom_aliases: Vec<String> = helper
            .custom_windows
            .iter()
            .map(|w| w.alias.clone())
            .collect();

        // Move any entry that matches a custom alias to the correct map
        let keys_to_move: Vec<String> = character_thumbnails
            .keys()
            .filter(|k| custom_aliases.contains(k))
            .cloned()
            .collect();

        for key in keys_to_move {
            if let Some(val) = character_thumbnails.remove(&key) {
                custom_source_thumbnails.insert(key, val);
            }
        }

        // Fixup: Go through all cycle groups and ensure any entry matching a custom window is Source
        let custom_aliases_set = custom_aliases; // move ownership
        for group in &mut cycle_groups {
            for slot in &mut group.cycle_list {
                if let crate::config::profile::CycleSlot::Eve(name) = slot
                    && custom_aliases_set.contains(name)
                {
                    *slot = crate::config::profile::CycleSlot::Source(name.clone());
                }
            }
        }

        Profile {
            profile_name: helper.profile_name,
            profile_description: helper.profile_description,
            thumbnail_default_width: helper.thumbnail_default_width,
            thumbnail_default_height: helper.thumbnail_default_height,
            thumbnail_enabled: helper.thumbnail_enabled,
            thumbnail_opacity: helper.thumbnail_opacity,
            thumbnail_active_border: helper.thumbnail_active_border,
            thumbnail_active_border_size: helper.thumbnail_active_border_size,
            thumbnail_active_border_color: helper.thumbnail_active_border_color,
            thumbnail_inactive_border: helper.thumbnail_inactive_border,
            thumbnail_inactive_border_size: helper.thumbnail_inactive_border_size,
            thumbnail_inactive_border_color: helper.thumbnail_inactive_border_color,
            thumbnail_text_size: helper.thumbnail_text_size,
            thumbnail_text_x: helper.thumbnail_text_x,
            thumbnail_text_y: helper.thumbnail_text_y,
            thumbnail_text_font: helper.thumbnail_text_font,
            thumbnail_text_color: helper.thumbnail_text_color,
            thumbnail_auto_save_position: helper.thumbnail_auto_save_position,
            thumbnail_snap_threshold: helper.thumbnail_snap_threshold,
            thumbnail_hide_not_focused: helper.thumbnail_hide_not_focused,
            thumbnail_preserve_position_on_swap: helper.thumbnail_preserve_position_on_swap,
            client_minimize_on_switch: helper.client_minimize_on_switch,
            client_minimize_show_overlay: helper.client_minimize_show_overlay,
            hotkey_backend: helper.hotkey_backend,
            hotkey_input_device: helper.hotkey_input_device,
            hotkey_logged_out_cycle: helper.hotkey_logged_out_cycle,
            hotkey_require_eve_focus: helper.hotkey_require_eve_focus,
            hotkey_cycle_reset_index: helper.hotkey_cycle_reset_index,
            hotkey_profile_switch: helper.hotkey_profile_switch,
            hotkey_toggle_skip: helper.hotkey_toggle_skip,
            hotkey_toggle_previews: helper.hotkey_toggle_previews,
            cycle_groups, // Use the migrated or valid groups
            character_hotkeys: helper.character_hotkeys,
            character_thumbnails,
            custom_source_thumbnails,
            custom_windows: helper.custom_windows,
        }
    }
}

// Custom implementation to support both Helper (JSON/Human) and Strict/Binary (Bincode/IPC)
impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            // Use ProfileHelper for JSON migration and flexibility
            ProfileHelper::deserialize(deserializer).map(Profile::from)
        } else {
            // Use strict binary structure for IPC/Bincode (matches Serialize output)
            #[derive(Deserialize)]
            struct ProfileBinary {
                pub profile_name: String,
                #[serde(default)]
                pub profile_description: String,
                #[serde(default = "default_thumbnail_width")]
                pub thumbnail_default_width: u16,
                #[serde(default = "default_thumbnail_height")]
                pub thumbnail_default_height: u16,
                #[serde(default = "default_thumbnail_enabled")]
                pub thumbnail_enabled: bool,
                pub thumbnail_opacity: u8,
                #[serde(default = "default_border_enabled", alias = "thumbnail_border")]
                pub thumbnail_active_border: bool,
                #[serde(alias = "thumbnail_border_size")]
                pub thumbnail_active_border_size: u16,
                #[serde(alias = "thumbnail_border_color")]
                pub thumbnail_active_border_color: String,
                #[serde(default = "default_inactive_border_enabled")]
                pub thumbnail_inactive_border: bool,
                #[serde(
                    alias = "thumbnail_inactive_border_size",
                    default = "default_border_size"
                )]
                pub thumbnail_inactive_border_size: u16,
                #[serde(default = "default_inactive_border_color")]
                pub thumbnail_inactive_border_color: String,
                pub thumbnail_text_size: u16,
                pub thumbnail_text_x: i16,
                pub thumbnail_text_y: i16,
                #[serde(default = "default_text_font_family")]
                pub thumbnail_text_font: String,
                pub thumbnail_text_color: String,
                #[serde(default = "default_auto_save_thumbnail_positions")]
                pub thumbnail_auto_save_position: bool,
                #[serde(default = "default_snap_threshold")]
                pub thumbnail_snap_threshold: u16,
                #[serde(default)]
                pub thumbnail_hide_not_focused: bool,
                #[serde(default = "default_preserve_thumbnail_position_on_swap")]
                pub thumbnail_preserve_position_on_swap: bool,
                #[serde(default)]
                pub client_minimize_on_switch: bool,
                #[serde(default)]
                pub client_minimize_show_overlay: bool,
                #[serde(default = "default_hotkey_backend")]
                pub hotkey_backend: HotkeyBackendType,
                #[serde(default)]
                pub hotkey_input_device: Option<String>,
                #[serde(default)]
                pub cycle_groups: Vec<CycleGroupBinary>,
                #[serde(default)]
                pub hotkey_logged_out_cycle: bool,
                #[serde(default)]
                pub hotkey_require_eve_focus: bool,
                #[serde(default)]
                pub hotkey_cycle_reset_index: bool,
                #[serde(default)]
                pub hotkey_profile_switch: Option<crate::config::HotkeyBinding>,
                #[serde(default)]
                pub hotkey_toggle_skip: Option<crate::config::HotkeyBinding>,
                #[serde(default)]
                pub hotkey_toggle_previews: Option<crate::config::HotkeyBinding>,
                #[serde(default)]
                pub character_hotkeys: HashMap<String, crate::config::HotkeyBinding>,
                #[serde(default)]
                pub character_thumbnails: HashMap<String, CharacterSettings>,
                #[serde(default)]
                pub custom_source_thumbnails: HashMap<String, CharacterSettings>,
                #[serde(default)]
                pub custom_windows: Vec<CustomWindowRule>,
            }

            #[derive(Deserialize)]
            pub struct CycleGroupBinary {
                pub name: String,
                pub cycle_list: Vec<CycleSlotBinary>,
                pub hotkey_forward: Option<crate::config::HotkeyBinding>,
                pub hotkey_backward: Option<crate::config::HotkeyBinding>,
            }

            #[derive(Deserialize)]
            pub enum CycleSlotBinary {
                Eve(String),
                Source(String),
            }

            let p = ProfileBinary::deserialize(deserializer)?;

            // Convert binary cycle groups to standard CycleGroup
            let cycle_groups: Vec<CycleGroup> = p
                .cycle_groups
                .into_iter()
                .map(|g| CycleGroup {
                    name: g.name,
                    cycle_list: g
                        .cycle_list
                        .into_iter()
                        .map(|s| match s {
                            CycleSlotBinary::Eve(n) => crate::config::profile::CycleSlot::Eve(n),
                            CycleSlotBinary::Source(n) => {
                                crate::config::profile::CycleSlot::Source(n)
                            }
                        })
                        .collect(),
                    hotkey_forward: g.hotkey_forward,
                    hotkey_backward: g.hotkey_backward,
                })
                .collect();

            Ok(Profile {
                profile_name: p.profile_name,
                profile_description: p.profile_description,
                thumbnail_default_width: p.thumbnail_default_width,
                thumbnail_default_height: p.thumbnail_default_height,
                thumbnail_enabled: p.thumbnail_enabled,
                thumbnail_opacity: p.thumbnail_opacity,
                thumbnail_active_border: p.thumbnail_active_border,
                thumbnail_active_border_size: p.thumbnail_active_border_size,
                thumbnail_active_border_color: p.thumbnail_active_border_color,
                thumbnail_inactive_border: p.thumbnail_inactive_border,
                thumbnail_inactive_border_size: p.thumbnail_inactive_border_size,
                thumbnail_inactive_border_color: p.thumbnail_inactive_border_color,
                thumbnail_text_size: p.thumbnail_text_size,
                thumbnail_text_x: p.thumbnail_text_x,
                thumbnail_text_y: p.thumbnail_text_y,
                thumbnail_text_font: p.thumbnail_text_font,
                thumbnail_text_color: p.thumbnail_text_color,
                thumbnail_auto_save_position: p.thumbnail_auto_save_position,
                thumbnail_snap_threshold: p.thumbnail_snap_threshold,
                thumbnail_hide_not_focused: p.thumbnail_hide_not_focused,
                thumbnail_preserve_position_on_swap: p.thumbnail_preserve_position_on_swap,
                client_minimize_on_switch: p.client_minimize_on_switch,
                client_minimize_show_overlay: p.client_minimize_show_overlay,
                hotkey_backend: p.hotkey_backend,
                hotkey_input_device: p.hotkey_input_device,
                cycle_groups,
                hotkey_logged_out_cycle: p.hotkey_logged_out_cycle,
                hotkey_require_eve_focus: p.hotkey_require_eve_focus,
                hotkey_cycle_reset_index: p.hotkey_cycle_reset_index,
                hotkey_profile_switch: p.hotkey_profile_switch,
                hotkey_toggle_skip: p.hotkey_toggle_skip,
                hotkey_toggle_previews: p.hotkey_toggle_previews,
                character_hotkeys: p.character_hotkeys,
                character_thumbnails: p.character_thumbnails,
                custom_source_thumbnails: p.custom_source_thumbnails,
                custom_windows: p.custom_windows,
            })
        }
    }
}
