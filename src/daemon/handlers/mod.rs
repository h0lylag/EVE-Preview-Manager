pub(super) mod input;
pub(super) mod state;
pub(super) mod window;

use super::source_registry::SourceRegistry;
use crate::common::types::CharacterSettings;
use crate::config::DisplayConfig;
use std::collections::HashMap;
use x11rb::protocol::xproto::Window;

/// Select every tracked source window that should be minimized after activation.
/// Preview rendering is intentionally irrelevant; thumbnails are only used later
/// for optional border cleanup.
pub(super) fn source_windows_to_minimize(
    sources: &SourceRegistry,
    display_config: &DisplayConfig,
    activated_window: Window,
) -> Vec<Window> {
    sources
        .windows()
        .filter(|&source_window| {
            if source_window == activated_window {
                return false;
            }

            // Live identity first; logged-out EVE clients use their remembered character.
            let settings = sources
                .effective_identity(source_window)
                .and_then(|identity| display_config.settings_for(identity.kind, &identity.name));

            !settings.is_some_and(|settings| settings.exempt_from_minimize)
        })
        .collect()
}

pub(super) fn upsert_spatial_settings(
    map: &mut HashMap<String, CharacterSettings>,
    name: &str,
    settings: CharacterSettings,
) -> bool {
    if let Some(existing) = map.get_mut(name) {
        let changed = existing.x != settings.x
            || existing.y != settings.y
            || existing.dimensions != settings.dimensions;

        existing.x = settings.x;
        existing.y = settings.y;
        existing.dimensions = settings.dimensions;

        changed
    } else {
        map.insert(name.to_string(), settings);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::{Dimensions, PreviewMode};
    use crate::config::DaemonConfig;
    use crate::config::profile::Profile;
    use crate::daemon::source_registry::TrackedSource;

    fn test_display_config() -> DisplayConfig {
        let profile = Profile {
            thumbnail_enabled: false,
            ..Profile::default()
        };

        DaemonConfig {
            profile,
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        }
        .build_display_config()
    }

    fn exempt_settings() -> CharacterSettings {
        let mut settings = CharacterSettings::new(0, 0, 240, 135);
        settings.exempt_from_minimize = true;
        settings
    }

    #[test]
    fn minimization_uses_tracked_windows_when_rendering_is_disabled() {
        let mut sources = SourceRegistry::default();
        sources.register(1, TrackedSource::eve("Active"));
        sources.register(2, TrackedSource::eve("Other"));

        let display_config = test_display_config();
        assert!(!display_config.enabled);

        assert_eq!(
            source_windows_to_minimize(&sources, &display_config, 1),
            vec![2]
        );
    }

    #[test]
    fn minimization_uses_typed_settings_for_same_name_sources() {
        let mut sources = SourceRegistry::default();
        sources.register(1, TrackedSource::eve("Active"));
        sources.register(2, TrackedSource::eve("Shared"));
        sources.register(3, TrackedSource::custom("Shared"));

        let mut display_config = test_display_config();
        display_config
            .character_settings
            .insert("Shared".to_string(), CharacterSettings::new(0, 0, 240, 135));
        display_config
            .custom_source_settings
            .insert("Shared".to_string(), exempt_settings());

        assert_eq!(
            source_windows_to_minimize(&sources, &display_config, 1),
            vec![2]
        );
    }

    #[test]
    fn minimization_uses_remembered_identity_but_keeps_unidentified_windows() {
        let mut sources = SourceRegistry::default();
        sources.register(1, TrackedSource::eve("Active"));
        sources.register(2, TrackedSource::logged_out_after("Remembered"));
        sources.register(3, TrackedSource::eve(""));

        let mut display_config = test_display_config();
        display_config
            .character_settings
            .insert("Remembered".to_string(), exempt_settings());

        assert_eq!(
            source_windows_to_minimize(&sources, &display_config, 1),
            vec![3]
        );
    }

    #[test]
    fn upsert_spatial_settings_preserves_non_spatial_overrides() {
        let mut map = HashMap::new();
        let mut existing = CharacterSettings::new(1, 2, 300, 200);
        existing.alias = Some("Alias".to_string());
        existing.preview_mode = PreviewMode::Static {
            color: "#123456".to_string(),
        };
        existing.exempt_from_minimize = true;
        existing.override_render_preview = Some(false);
        map.insert("Source".to_string(), existing);

        let changed =
            upsert_spatial_settings(&mut map, "Source", CharacterSettings::new(10, 20, 640, 360));

        assert!(changed);
        let updated = map.get("Source").unwrap();
        assert_eq!(updated.x, 10);
        assert_eq!(updated.y, 20);
        assert_eq!(updated.dimensions, Dimensions::new(640, 360));
        assert_eq!(updated.alias.as_deref(), Some("Alias"));
        assert_eq!(
            updated.preview_mode,
            PreviewMode::Static {
                color: "#123456".to_string()
            }
        );
        assert!(updated.exempt_from_minimize);
        assert_eq!(updated.override_render_preview, Some(false));
    }
}
