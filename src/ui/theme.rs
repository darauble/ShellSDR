//! Color themes (pure) and the theme registry.
//!
//! A `Theme` maps semantic UI roles to concrete colors so the renderer never
//! hardcodes a color. Themes are defined in external JSON files (see
//! `theme_spec`), with two built-ins embedded via `include_str!` as the
//! always-available fallback. The `ThemeRegistry` holds the available themes
//! by name (embedded built-ins plus any discovered on disk) and supports
//! lookup and cycling.

use ratatui::style::{Color, Modifier, Style};

use super::theme_spec::ThemeSpec;

/// Embedded copies of the shipped themes. These are the same JSON files that
/// live in `assets/themes/` for packaging and user reference; embedding them
/// guarantees the app always has Default and Retro even with no files on disk.
pub const EMBEDDED_DEFAULT: &str = include_str!("../../assets/themes/default.json");
pub const EMBEDDED_RETRO: &str = include_str!("../../assets/themes/retro.json");

/// Concrete colors for all UI roles.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Human-readable name (from the JSON `name` field).
    pub name: String,
    pub text: Color,
    pub dim: Color,
    pub accent: Color,
    pub border: Color,
    pub border_focused: Color,
    /// Field-state: not selected.
    pub field_idle: Color,
    /// Field-state: selected (cursor on it, not editing).
    pub field_selected: Color,
    /// Field-state: editing (foreground + background).
    pub editing_fg: Color,
    pub editing_bg: Color,
    /// OFF note on disabled spectrum/waterfall.
    pub off_note: Color,
    /// Spectrum trace color.
    pub spectrum: Color,
    /// Waterfall gradient color stops (low power -> high power). Any length.
    pub waterfall: Vec<(u8, u8, u8)>,
}

impl Theme {
    /// The embedded Default theme (always available).
    pub fn builtin_default() -> Self {
        ThemeSpec::from_json(EMBEDDED_DEFAULT)
            .and_then(|s| s.to_theme())
            .expect("embedded default theme must be valid")
    }

    /// The embedded Retro terminal theme (always available).
    pub fn builtin_retro() -> Self {
        ThemeSpec::from_json(EMBEDDED_RETRO)
            .and_then(|s| s.to_theme())
            .expect("embedded retro theme must be valid")
    }

    /// Border style for a region, highlighted when focused.
    pub fn border_style(&self, focused: bool) -> Style {
        Style::default().fg(if focused {
            self.border_focused
        } else {
            self.border
        })
    }

    /// Primary text style.
    pub fn text_style(&self) -> Style {
        Style::default().fg(self.text)
    }

    /// Dim/secondary text style.
    pub fn dim_style(&self) -> Style {
        Style::default().fg(self.dim)
    }

    /// Style for the OFF note on a disabled display area.
    pub fn off_style(&self) -> Style {
        Style::default().fg(self.off_note).bg(Color::Black)
    }

    /// Style for a field in the idle / selected / editing states.
    pub fn field_style(&self, selected: bool, editing: bool) -> Style {
        if editing {
            Style::default().fg(self.editing_fg).bg(self.editing_bg)
        } else if selected {
            Style::default().fg(self.field_selected)
        } else {
            Style::default().fg(self.field_idle)
        }
    }

    /// Style for a single highlighted frequency digit while editing:
    /// black text on white (per spec), independent of theme tint.
    pub fn digit_highlight_style(&self) -> Style {
        Style::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD)
    }

    /// Sample the waterfall gradient at `t` in [0,1], linearly interpolating
    /// between the RGB stops (same approach as SDR++). Works for any number of
    /// stops (2..N).
    // Consumed by the waterfall renderer in step 7.
    #[allow(dead_code)]
    pub fn waterfall_color(&self, t: f32) -> (u8, u8, u8) {
        let stops = &self.waterfall;
        if stops.is_empty() {
            return (0, 0, 0);
        }
        if stops.len() == 1 {
            return stops[0];
        }
        let t = t.clamp(0.0, 1.0);
        let scaled = t * (stops.len() - 1) as f32;
        let i = scaled.floor() as usize;
        if i >= stops.len() - 1 {
            return stops[stops.len() - 1];
        }
        let frac = scaled - i as f32;
        let (r0, g0, b0) = stops[i];
        let (r1, g1, b1) = stops[i + 1];
        let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * frac).round() as u8;
        (lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
    }
}

/// A set of available themes, in display order, with a selected index.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeRegistry {
    themes: Vec<Theme>,
    selected: usize,
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::builtin_only()
    }
}

impl ThemeRegistry {
    /// A registry containing only the embedded built-in themes.
    pub fn builtin_only() -> Self {
        Self {
            themes: vec![Theme::builtin_default(), Theme::builtin_retro()],
            selected: 0,
        }
    }

    /// Build a registry from an explicit list of themes (built-ins first,
    /// then any loaded from disk). De-duplicates by name, keeping the first
    /// occurrence so a user file can be appended without clobbering a built-in
    /// unless it shares the name (then the earlier one wins — callers that
    /// want overrides should order accordingly).
    pub fn from_themes(themes: Vec<Theme>) -> Self {
        let mut out: Vec<Theme> = Vec::new();
        for t in themes {
            if !out.iter().any(|e| e.name == t.name) {
                out.push(t);
            }
        }
        if out.is_empty() {
            out.push(Theme::builtin_default());
        }
        Self {
            themes: out,
            selected: 0,
        }
    }

    /// The names of all available themes, in order.
    // Used by tests and the theme-selector UI / persistence (step 10).
    #[allow(dead_code)]
    pub fn names(&self) -> Vec<String> {
        self.themes.iter().map(|t| t.name.clone()).collect()
    }

    /// The currently selected theme.
    pub fn current(&self) -> &Theme {
        &self.themes[self.selected]
    }

    /// Advance to the next theme (wraps).
    pub fn cycle(&mut self) {
        if !self.themes.is_empty() {
            self.selected = (self.selected + 1) % self.themes.len();
        }
    }

    /// Select a theme by name; returns true if found.
    // Used by tests and by config-driven theme selection (step 10).
    #[allow(dead_code)]
    pub fn select_by_name(&mut self, name: &str) -> bool {
        if let Some(i) = self.themes.iter().position(|t| t.name == name) {
            self.selected = i;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_builtins_parse() {
        let d = Theme::builtin_default();
        assert_eq!(d.name, "Default");
        let r = Theme::builtin_retro();
        assert_eq!(r.name, "Retro terminal");
    }

    #[test]
    fn retro_waterfall_keeps_zalias_stops() {
        let t = Theme::builtin_retro();
        assert_eq!(
            t.waterfall,
            vec![(0, 0, 0), (0, 0x50, 0), (0, 0xFF, 0), (0xFF, 0xFF, 0)],
            "retro waterfall keeps authentic SDR++ Zalias stops"
        );
    }

    #[test]
    fn retro_ui_green_is_readable() {
        let t = Theme::builtin_retro();
        if let Color::Rgb(_, g, _) = t.field_idle {
            assert!(g > 0x50, "idle green lightened above raw Zalias dark green");
        } else {
            panic!("retro field_idle should be an RGB green");
        }
    }

    #[test]
    fn field_style_reflects_state() {
        let t = Theme::builtin_retro();
        let idle = t.field_style(false, false);
        let editing = t.field_style(true, true);
        assert_eq!(idle.fg, Some(t.field_idle));
        assert_eq!(editing.fg, Some(t.editing_fg));
        assert_eq!(editing.bg, Some(t.editing_bg));
    }

    #[test]
    fn digit_highlight_is_black_on_white() {
        let t = Theme::builtin_default();
        let s = t.digit_highlight_style();
        assert_eq!(s.fg, Some(Color::Black));
        assert_eq!(s.bg, Some(Color::White));
    }

    #[test]
    fn registry_cycles_and_selects_by_name() {
        let mut reg = ThemeRegistry::builtin_only();
        assert_eq!(reg.current().name, "Default");
        reg.cycle();
        assert_eq!(reg.current().name, "Retro terminal");
        reg.cycle();
        assert_eq!(reg.current().name, "Default", "wraps");
        assert!(reg.select_by_name("Retro terminal"));
        assert_eq!(reg.current().name, "Retro terminal");
        assert!(!reg.select_by_name("Nonexistent"));
    }

    #[test]
    fn registry_dedups_by_name() {
        let reg = ThemeRegistry::from_themes(vec![
            Theme::builtin_default(),
            Theme::builtin_retro(),
            Theme::builtin_default(),
        ]);
        assert_eq!(reg.names(), vec!["Default", "Retro terminal"]);
    }
}
