//! Font discovery via fontconfig

use anyhow::{Context, Result};
use fontconfig::{Fontconfig, Pattern};
use std::collections::BTreeSet;
use std::ffi::CString;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Common font style names for parsing family+style strings
const KNOWN_STYLES: &[&str] = &[
    // Condensed variants (longest first)
    "Condensed Bold Oblique",
    "Condensed Bold Italic",
    "Condensed Bold",
    "Condensed Oblique",
    "Condensed Italic",
    "Condensed",
    // Weight + style combinations
    "ExtraBold Italic",
    "ExtraLight Italic",
    "SemiBold Italic",
    "Black Italic",
    "Medium Italic",
    "Light Italic",
    "Thin Italic",
    "Bold Oblique",
    "Bold Italic",
    // Weights (heavy to light)
    "ExtraBold",
    "Black",
    "Bold",
    "SemiBold",
    "Medium",
    "Book",
    "Regular",
    "Light",
    "ExtraLight",
    "Thin",
    // Styles
    "Oblique",
    "Italic",
    // Width variants
    "Expanded",
];

/// Helper to parse a font string into family and optional style.
///
/// Example: "Open Sans Condensed Bold" -> ("Open Sans", Some("Condensed Bold"))
fn parse_font_name(font_name: &str) -> (&str, Option<&str>) {
    for style in KNOWN_STYLES {
        if let Some(style_pos) = font_name.rfind(style) {
            // Check if style is at the end of the string
            if style_pos + style.len() == font_name.len() {
                let prefix = &font_name[..style_pos];
                // Check if the prefix implies a valid separation (empty or ends with space)
                if prefix.is_empty() || prefix.ends_with(' ') {
                    let family_name = prefix.trim();
                    if !family_name.is_empty() {
                        return (family_name, Some(style));
                    }
                }
            }
        }
    }
    (font_name, None)
}

/// Get list of all individual fonts with their full names
pub fn list_fonts() -> Result<Vec<String>> {
    debug!("Loading available fonts from fontconfig...");
    let fc = Fontconfig::new().context("Failed to initialize fontconfig")?;
    let pattern = Pattern::new(&fc);
    let font_set = fontconfig::list_fonts(&pattern, None);

    let mut fonts = BTreeSet::new();
    for font_pattern in font_set.iter() {
        let family = font_pattern
            .get_string(fontconfig::FC_FAMILY)
            .unwrap_or("Unknown");

        let font_name = if let Some(style_str) = font_pattern.get_string(fontconfig::FC_STYLE) {
            if style_str == "Regular" {
                family.to_string()
            } else {
                format!("{} {}", family, style_str)
            }
        } else {
            family.to_string()
        };

        fonts.insert(font_name);
    }

    debug!(
        count = fonts.len(),
        "Discovered individual fonts via fontconfig"
    );
    Ok(fonts.into_iter().collect())
}

/// Find best matching font file path for a given family name or full font name
pub fn find_font_path(font_name: &str) -> Result<PathBuf> {
    let fc = Fontconfig::new().context("Failed to initialize fontconfig")?;

    let (family_name, style_name) = parse_font_name(font_name);

    if style_name.is_some() {
        debug!(
            font = font_name,
            family = family_name,
            style = ?style_name,
            "Parsed font into family and style"
        );
    }

    let mut pattern = Pattern::new(&fc);
    let family_cstr = CString::new(family_name)
        .with_context(|| format!("Invalid family name: {}", family_name))?;
    pattern.add_string(fontconfig::FC_FAMILY, &family_cstr);

    if let Some(style) = style_name {
        let style_cstr =
            CString::new(style).with_context(|| format!("Invalid style name: {}", style))?;
        pattern.add_string(fontconfig::FC_STYLE, &style_cstr);
    }

    let matched = pattern.font_match();

    if let Some(matched_family) = matched.get_string(fontconfig::FC_FAMILY)
        && !matched_family.eq_ignore_ascii_case(family_name)
    {
        warn!(
            requested = font_name,
            requested_family = family_name,
            matched_family = matched_family,
            "Fontconfig returned different font family - requested font may not be installed"
        );
        return Err(anyhow::anyhow!(
            "Font '{}' not found - fontconfig returned family '{}' instead",
            font_name,
            matched_family
        ));
    }

    let file_path = matched
        .filename()
        .with_context(|| format!("No font file found for '{}'", font_name))?;

    let path = PathBuf::from(file_path);

    if !path.exists() {
        warn!(
            font = font_name,
            path = %path.display(),
            "Font file path from fontconfig does not exist"
        );
        return Err(anyhow::anyhow!(
            "Font file path '{}' does not exist",
            path.display()
        ));
    }

    debug!(
        font = font_name,
        family = family_name,
        style = ?style_name,
        path = %path.display(),
        "Resolved font path via family + style"
    );

    Ok(path)
}

/// Scans for a suitable default TrueType font from a hardcoded list of preferred fonts.
/// Returns the first match found on the system.
pub fn select_best_default_font() -> Result<(String, PathBuf)> {
    let candidates = crate::common::constants::defaults::text::FONT_CANDIDATES;

    for candidate in candidates {
        if let Ok(path) = find_font_path(candidate)
            && path.exists()
        {
            info!(font = candidate, path = %path.display(), "Selected default font via fontconfig");
            return Ok((candidate.to_string(), path));
        }
    }

    debug!("Specific fonts not found, querying for any monospace font");
    let fc = Fontconfig::new().context("Failed to initialize fontconfig")?;
    let mut pattern = Pattern::new(&fc);
    pattern.add_integer(fontconfig::FC_SPACING, 100);

    let font_set = fontconfig::list_fonts(&pattern, None);

    for font_pattern in font_set.iter() {
        let family = font_pattern
            .get_string(fontconfig::FC_FAMILY)
            .unwrap_or("Unknown");

        if let Some(style) = font_pattern.get_string(fontconfig::FC_STYLE) {
            let style_lower = style.to_lowercase();
            if style_lower.contains("bold")
                || style_lower.contains("italic")
                || style_lower.contains("oblique")
            {
                continue;
            }
        }

        if let Some(file_path) = font_pattern.filename() {
            let path = PathBuf::from(file_path);
            if path.exists() {
                info!(font = family, path = %path.display(), "Selected first available monospace font");
                return Ok((family.to_string(), path));
            }
        }
    }

    Err(anyhow::anyhow!(
        "No TrueType fonts found. Tried:\n\
         - Specific fonts: {:?}\n\
         - Any monospace Regular/Normal font via fontconfig\n\
         \n\
         Will fall back to X11 core fonts.",
        candidates
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_font_name() {
        assert_eq!(parse_font_name("Open Sans"), ("Open Sans", None));
        assert_eq!(
            parse_font_name("Open Sans Condensed Bold"),
            ("Open Sans", Some("Condensed Bold"))
        );
        assert_eq!(
            parse_font_name("Fira Code Regular"),
            ("Fira Code", Some("Regular"))
        );
        assert_eq!(
            parse_font_name("Roboto Condensed"),
            ("Roboto", Some("Condensed"))
        );
        assert_eq!(
            parse_font_name("Noto Sans Bold Italic"),
            ("Noto Sans", Some("Bold Italic"))
        );
    }

    #[test]
    fn test_find_common_fonts() {
        let test_families = vec!["DejaVu Sans", "Liberation Sans", "Monospace"];

        for family in test_families {
            if let Ok(path) = find_font_path(family) {
                println!("{} -> {}", family, path.display());
                assert!(path.is_absolute(), "Font path should be absolute");
            }
        }
    }
}
