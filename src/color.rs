//! Color type conversions and utilities
//!
//! Provides type-safe color handling with conversions between:
//! - Hex strings (#AARRGGBB format)
//! - ARGB32 values (u32)
//! - X11 render Colors (16-bit per channel)
//! - Premultiplied ARGB32 (for text rendering)

use x11rb::protocol::render::Color;

/// Hex color in ARGB32 format (#AARRGGBB)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexColor(u32);

impl HexColor {
    /// Parse hex color string supporting multiple formats:
    /// - 6 digits: RRGGBB (full opacity assumed, becomes FFRRGGBB)
    /// - 8 digits: AARRGGBB (explicit alpha)
    /// - Optional '#' prefix supported but not required
    pub fn parse(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        let value = u32::from_str_radix(hex, 16).ok()?;

        // If 6 digits (RRGGBB), prepend full opacity (FF)
        // Check if value fits in 24 bits (max 0xFFFFFF)
        let argb = if value <= 0xFF_FF_FF {
            0xFF_00_00_00 | value // Prepend FF for full opacity
        } else {
            value // Already has alpha channel
        };

        Some(Self(argb))
    }

    /// Create from ARGB32 value
    pub fn from_argb32(argb: u32) -> Self {
        Self(argb)
    }

    /// Get raw ARGB32 value
    pub fn argb32(self) -> u32 {
        self.0
    }

    /// Convert to X11 Color (16-bit per channel, 0-65535 range)
    pub fn to_x11_color(self) -> Color {
        let a = (self.0 >> 24) & 0xFF;
        let r = (self.0 >> 16) & 0xFF;
        let g = (self.0 >> 8) & 0xFF;
        let b = self.0 & 0xFF;

        // Scale from 8-bit (0-255) to 16-bit (0-65535)
        let scale = |v: u32| (v << 8 | v) as u16;

        Color {
            red: scale(r),
            green: scale(g),
            blue: scale(b),
            alpha: scale(a),
        }
    }
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
