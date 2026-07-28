pub(super) mod input;
pub(super) mod state;
pub(super) mod window;

use crate::common::types::CharacterSettings;
use std::collections::HashMap;

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
