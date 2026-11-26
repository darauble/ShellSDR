//! JSON theme specification and parsing (pure).
//!
//! Step 3.6a. A theme on disk is a JSON document with a readable `name`, hex
//! color strings for each UI role, and a `waterfall` array of hex color stops
//! of any length (2..~20, not fixed to four). `ThemeSpec` is the serde form;
//! it converts into the runtime `Theme`. Hex parsing is `#RRGGBB` (with or
//! without the leading `#`).

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

use super::theme::Theme;

/// Error parsing a theme document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeParseError {
    Json(String),
    Color { field: String, value: String },
}

impl std::fmt::Display for ThemeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeParseError::Json(m) => write!(f, "theme JSON error: {m}"),
            ThemeParseError::Color { field, value } => {
                write!(f, "invalid color in '{field}': '{value}'")
            }
        }
    }
}

impl std::error::Error for ThemeParseError {}

/// Parse a `#RRGGBB` (or `RRGGBB`) hex string into an `(r, g, b)` triple.
pub fn parse_hex_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some((r, g, b))
}

fn color(field: &str, s: &str) -> Result<Color, ThemeParseError> {
    parse_hex_rgb(s)
        .map(|(r, g, b)| Color::Rgb(r, g, b))
        .ok_or_else(|| ThemeParseError::Color {
            field: field.to_string(),
            value: s.to_string(),
        })
}

/// Serde form of a theme file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeSpec {
    /// Human-readable name shown in the theme selector.
    pub name: String,
    pub text: String,
    pub dim: String,
    pub accent: String,
    pub border: String,
    pub border_focused: String,
    pub field_idle: String,
    pub field_selected: String,
    pub editing_fg: String,
    pub editing_bg: String,
    pub off_note: String,
    pub spectrum: String,
    /// Waterfall gradient color stops, low power -> high power. Any length
    /// >= 1 (typically 2..20); not fixed to four.
    pub waterfall: Vec<String>,
}

impl ThemeSpec {
    /// Parse a theme from a JSON string.
    pub fn from_json(s: &str) -> Result<Self, ThemeParseError> {
        serde_json::from_str(s).map_err(|e| ThemeParseError::Json(e.to_string()))
    }

    /// Serialize to pretty JSON (used to write the shipped files / scaffolds).
    // Used by tests and by theme-file scaffolding.
    #[allow(dead_code)]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Convert into the runtime `Theme`, validating every color.
    pub fn to_theme(&self) -> Result<Theme, ThemeParseError> {
        let mut waterfall = Vec::with_capacity(self.waterfall.len());
        for (i, stop) in self.waterfall.iter().enumerate() {
            let (r, g, b) = parse_hex_rgb(stop).ok_or_else(|| ThemeParseError::Color {
                field: format!("waterfall[{i}]"),
                value: stop.clone(),
            })?;
            waterfall.push((r, g, b));
        }
        Ok(Theme {
            name: self.name.clone(),
            text: color("text", &self.text)?,
            dim: color("dim", &self.dim)?,
            accent: color("accent", &self.accent)?,
            border: color("border", &self.border)?,
            border_focused: color("border_focused", &self.border_focused)?,
            field_idle: color("field_idle", &self.field_idle)?,
            field_selected: color("field_selected", &self.field_selected)?,
            editing_fg: color("editing_fg", &self.editing_fg)?,
            editing_bg: color("editing_bg", &self.editing_bg)?,
            off_note: color("off_note", &self.off_note)?,
            spectrum: color("spectrum", &self.spectrum)?,
            waterfall,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_with_and_without_hash() {
        assert_eq!(parse_hex_rgb("#66FF66"), Some((0x66, 0xFF, 0x66)));
        assert_eq!(parse_hex_rgb("309030"), Some((0x30, 0x90, 0x30)));
        assert_eq!(parse_hex_rgb("#000000"), Some((0, 0, 0)));
    }

    #[test]
    fn rejects_bad_hex() {
        assert_eq!(parse_hex_rgb("#12345"), None, "too short");
        assert_eq!(parse_hex_rgb("#GGGGGG"), None, "non-hex");
        assert_eq!(parse_hex_rgb(""), None);
    }

    #[test]
    fn parses_a_full_theme_json() {
        let json = r##"{
            "name": "Test Theme",
            "text": "#66FF66",
            "dim": "#309030",
            "accent": "#FFFF33",
            "border": "#309030",
            "border_focused": "#66FF66",
            "field_idle": "#309030",
            "field_selected": "#66FF66",
            "editing_fg": "#000000",
            "editing_bg": "#FFFF33",
            "off_note": "#309030",
            "spectrum": "#66FF66",
            "waterfall": ["#000000", "#005000", "#00FF00", "#FFFF00"]
        }"##;
        let spec = ThemeSpec::from_json(json).expect("parse");
        assert_eq!(spec.name, "Test Theme");
        let theme = spec.to_theme().expect("convert");
        assert_eq!(theme.name, "Test Theme");
        assert_eq!(theme.text, Color::Rgb(0x66, 0xFF, 0x66));
        assert_eq!(theme.waterfall.len(), 4);
        assert_eq!(theme.waterfall[1], (0, 0x50, 0));
    }

    #[test]
    fn waterfall_accepts_arbitrary_stop_count() {
        // Six stops, not four: the gradient must not be fixed-length.
        let json = r##"{
            "name": "Six Stop",
            "text": "#FFFFFF", "dim": "#808080", "accent": "#FFFF00",
            "border": "#808080", "border_focused": "#FFFF00",
            "field_idle": "#808080", "field_selected": "#FFFFFF",
            "editing_fg": "#000000", "editing_bg": "#FFFF00",
            "off_note": "#808080", "spectrum": "#00FFFF",
            "waterfall": ["#000000","#101030","#2060A0","#20C0C0","#FFFF00","#FF0000"]
        }"##;
        let theme = ThemeSpec::from_json(json).unwrap().to_theme().unwrap();
        assert_eq!(theme.waterfall.len(), 6, "arbitrary stop count supported");
        // Sampler still spans the full range.
        assert_eq!(theme.waterfall_color(0.0), (0, 0, 0));
        assert_eq!(theme.waterfall_color(1.0), (0xFF, 0, 0));
    }

    #[test]
    fn invalid_color_reports_field() {
        let json = r##"{
            "name": "Bad",
            "text": "nope",
            "dim": "#309030", "accent": "#FFFF33",
            "border": "#309030", "border_focused": "#66FF66",
            "field_idle": "#309030", "field_selected": "#66FF66",
            "editing_fg": "#000000", "editing_bg": "#FFFF33",
            "off_note": "#309030", "spectrum": "#66FF66",
            "waterfall": ["#000000"]
        }"##;
        let err = ThemeSpec::from_json(json).unwrap().to_theme().unwrap_err();
        assert_eq!(
            err,
            ThemeParseError::Color {
                field: "text".into(),
                value: "nope".into()
            }
        );
    }

    #[test]
    fn round_trips_through_json() {
        let json = r##"{
            "name": "RT",
            "text": "#66FF66", "dim": "#309030", "accent": "#FFFF33",
            "border": "#309030", "border_focused": "#66FF66",
            "field_idle": "#309030", "field_selected": "#66FF66",
            "editing_fg": "#000000", "editing_bg": "#FFFF33",
            "off_note": "#309030", "spectrum": "#66FF66",
            "waterfall": ["#000000","#00FF00"]
        }"##;
        let spec = ThemeSpec::from_json(json).unwrap();
        let back = ThemeSpec::from_json(&spec.to_json()).unwrap();
        assert_eq!(spec, back);
    }
}
