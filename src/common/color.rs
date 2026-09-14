//! Color type conversions and utilities
//!
//! Provides type-safe color handling with conversions between:
//! - Hex strings (#RRGGBB or #AARRGGBB format)
//! - ARGB32 values (u32)
//! - X11 render Colors (16-bit per channel)
//! - Premultiplied ARGB32 (for text rendering)

use x11rb::protocol::render::Color;

/// Hex color in ARGB32 format (#AARRGGBB)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexColor(u32);

impl HexColor {
    /// Parse exactly six (RRGGBB) or eight (AARRGGBB) ASCII hex digits,
    /// with one optional '#' prefix. RGB implies full opacity; ARGB preserves
    /// explicit alpha, including zero. Whitespace and other syntax are rejected.
    pub fn parse(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(hex, 16).ok()?;
        Some(Self(if hex.len() == 6 {
            0xFF_00_00_00 | value
        } else {
            value
        }))
    }

    /// Create from ARGB32 value
    pub fn from_argb32(argb: u32) -> Self {
        Self(argb)
    }

    /// Get raw ARGB32 value
    pub fn argb32(self) -> u32 {
        self.0
    }

    /// Convert straight-alpha ARGB to premultiplied pixels for XRender compositing.
    pub fn to_premultiplied_argb32(self) -> u32 {
        let alpha = self.0 >> 24;
        let premultiply = |shift: u32| (((self.0 >> shift) & 0xFF) * alpha + 127) / 255;
        (alpha << 24) | (premultiply(16) << 16) | (premultiply(8) << 8) | premultiply(0)
    }

    /// Convert straight-alpha ARGB to premultiplied XRender channels (0-65535).
    pub fn to_x11_color(self) -> Color {
        let a = (self.0 >> 24) & 0xFF;
        let r = (self.0 >> 16) & 0xFF;
        let g = (self.0 >> 8) & 0xFF;
        let b = self.0 & 0xFF;

        // Scale from 8-bit (0-255) to 16-bit (0-65535)
        let scale = |v: u32| (v << 8 | v) as u16;
        let premultiply = |v: u32| ((v * 257 * a + 127) / 255) as u16;

        Color {
            red: premultiply(r),
            green: premultiply(g),
            blue: premultiply(b),
            alpha: scale(a),
        }
    }
}

/// Convert HEX string to egui::Color32
pub fn hex_to_color32(hex: &str) -> Option<egui::Color32> {
    let color = HexColor::parse(hex)?;
    let a = (color.0 >> 24) & 0xFF;
    let r = (color.0 >> 16) & 0xFF;
    let g = (color.0 >> 8) & 0xFF;
    let b = color.0 & 0xFF;
    Some(egui::Color32::from_rgba_premultiplied(
        r as u8, g as u8, b as u8, a as u8,
    ))
}

/// Convert egui::Color32 to HEX string (#AARRGGBB)
pub fn color32_to_hex(color: egui::Color32) -> String {
    let [r, g, b, a] = color.to_array();
    format!("#{:02X}{:02X}{:02X}{:02X}", a, r, g, b)
}

/// Opacity as percentage (0-100)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opacity(u8);

impl Opacity {
    /// Create from percentage (clamped to 0-100)
    pub fn from_percent(percent: u8) -> Self {
        Self(percent.min(100))
    }

    /// Create from ARGB32 value (extracts alpha channel)
    #[cfg(test)]
    pub fn from_argb32(argb: u32) -> Self {
        let alpha = (argb >> 24) & 0xFF;
        let percent = (alpha as f32 / 255.0 * 100.0).round() as u8;
        Self(percent.min(100))
    }

    /// Get opacity as percentage (0-100)
    #[cfg(test)]
    pub fn percent(self) -> u8 {
        self.0
    }

    /// Convert to ARGB32 opacity value (alpha in upper 8 bits)
    pub fn to_argb32(self) -> u32 {
        let alpha = (self.0 as f32 / 100.0 * 255.0) as u32;
        alpha << 24
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_regression_explicit_alpha_is_selected_by_digit_count() {
        for (input, expected) in [
            ("000000", 0xFF000000),
            ("123aBc", 0xFF123ABC),
            ("00000000", 0x00000000),
            ("00FF0000", 0x00FF0000),
            ("01123aBc", 0x01123ABC),
            ("7f123aBc", 0x7F123ABC),
            ("FF123aBc", 0xFF123ABC),
        ] {
            for prefix in ["", "#"] {
                let color = HexColor::parse(&format!("{prefix}{input}")).unwrap();
                assert_eq!(color.argb32(), expected, "{prefix}{input}");
                assert_eq!(color.to_x11_color().alpha, ((expected >> 24) * 257) as u16);
            }
        }
    }

    #[test]
    fn color_regression_rejects_unsupported_syntax() {
        for input in [
            "#€ABC",
            "#€ABCDE",
            "é1234",
            "é123456",
            "😀12",
            "😀1234",
            "",
            "#",
            "##123456",
            "##12345678",
            "12345G",
            "G0123456",
            " 123456",
            "123456 ",
            "#123456\n",
            "+12345",
            "+1234567",
            "-12345",
            "0x123456",
            "0",
            "FFF",
            "12345",
            "1234567",
            "000000000",
            "0000000000",
        ] {
            assert_eq!(HexColor::parse(input), None, "{input:?}");
        }
    }

    #[test]
    fn test_hex_color_parsing() {
        // 8-digit format (AARRGGBB)
        assert_eq!(HexColor::parse("#7FFF0000"), Some(HexColor(0x7FFF0000)));
        assert_eq!(HexColor::parse("7FFF0000"), Some(HexColor(0x7FFF0000)));
        assert_eq!(HexColor::parse("FFFFFFFF"), Some(HexColor(0xFFFFFFFF)));

        // 6-digit format (RRGGBB) - should prepend FF for full opacity
        assert_eq!(HexColor::parse("#FF0000"), Some(HexColor(0xFFFF0000)));
        assert_eq!(HexColor::parse("FF0000"), Some(HexColor(0xFFFF0000)));
        assert_eq!(HexColor::parse("#5bfc37"), Some(HexColor(0xFF5BFC37)));
        assert_eq!(HexColor::parse("5bfc37"), Some(HexColor(0xFF5BFC37)));

        // Invalid
        assert_eq!(HexColor::parse("invalid"), None);
        assert_eq!(HexColor::parse(""), None);
    }

    #[test]
    fn text_pixels_premultiply_rgb_and_preserve_alpha() {
        for (argb, expected) in [
            (0x00FF8040, 0x00000000),
            (0x80FF8040, 0x80804020),
            (0xFFFF8040, 0xFFFF8040),
            (0x01FF8040, 0x01010100),
        ] {
            assert_eq!(
                HexColor::from_argb32(argb).to_premultiplied_argb32(),
                expected
            );
        }
        for alpha in 0..=255 {
            for channel in 0..=255 {
                let argb = (alpha << 24) | (channel << 16) | (channel << 8) | channel;
                let pixel = HexColor::from_argb32(argb).to_premultiplied_argb32();
                assert_eq!(pixel >> 24, alpha);
                for shift in [0, 8, 16] {
                    assert!((pixel >> shift) & 255 <= alpha);
                }
                if alpha == 255 {
                    assert_eq!(pixel, argb);
                }
            }
        }
    }

    #[test]
    fn xrender_colors_premultiply_rgb_without_changing_alpha() {
        for (argb, expected) in [
            (0x00FF8040, [0, 0, 0, 0]),
            (0x80FF8040, [32896, 16513, 8256, 32896]),
            (0xFFFF8040, [65535, 32896, 16448, 65535]),
        ] {
            let color = HexColor::from_argb32(argb).to_x11_color();
            assert_eq!([color.red, color.green, color.blue, color.alpha], expected);
        }
    }

    #[test]
    fn test_hex_color_to_x11() {
        let color = HexColor(0xFF_80_40_20);
        let x11 = color.to_x11_color();

        // 0xFF → 0xFFFF, 0x80 → 0x8080, 0x40 → 0x4040, 0x20 → 0x2020
        assert_eq!(x11.alpha, 0xFFFF);
        assert_eq!(x11.red, 0x8080);
        assert_eq!(x11.green, 0x4040);
        assert_eq!(x11.blue, 0x2020);
    }

    #[test]
    fn test_opacity_percent() {
        let opacity = Opacity::from_percent(75);
        assert_eq!(opacity.percent(), 75);

        let opacity = Opacity::from_percent(150); // clamped
        assert_eq!(opacity.percent(), 100);
    }

    #[test]
    fn test_opacity_to_argb32() {
        let opacity = Opacity::from_percent(100);
        assert_eq!(opacity.to_argb32(), 0xFF000000);

        let opacity = Opacity::from_percent(50);
        let argb = opacity.to_argb32();
        let alpha = (argb >> 24) & 0xFF;
        assert!((127..=128).contains(&alpha)); // ~50% of 255
    }

    #[test]
    fn test_opacity_round_trip() {
        let original = Opacity::from_percent(75);
        let argb = original.to_argb32();
        let recovered = Opacity::from_argb32(argb);

        // Allow 1% difference due to rounding
        assert!((original.percent() as i16 - recovered.percent() as i16).abs() <= 1);
    }
}
