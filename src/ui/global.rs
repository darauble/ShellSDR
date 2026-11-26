//! Global-controls field navigation model (pure).
//!
//! The top Global Controls bar is a focus region with a cursor over its
//! actionable controls. Up/Down (or Left/Right) move the cursor; ENTER
//! activates a toggle or enters edit mode for the center-frequency field;
//! in-field arrows act on the focused control. This mirrors the Radio
//! module's idle/selected/editing model so interaction is uniform, and keeps
//! every control reachable by keyboard (no hidden shortcuts).

/// The focusable controls in the Global Controls bar, in navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlobalField {
    #[default]
    StartStop,
    Volume,
    Mute,
    Spectrum,
    Waterfall,
    /// The Work Area layout: `[Tabs]` / `[Split]` (spec 13.5).
    Layout,
    Frequency,
    CenterFree,
    Theme,
    Panel,
    Help,
}

impl GlobalField {
    pub const ORDER: [GlobalField; 11] = [
        GlobalField::StartStop,
        GlobalField::Volume,
        GlobalField::Mute,
        GlobalField::Spectrum,
        GlobalField::Waterfall,
        GlobalField::Layout,
        GlobalField::Frequency,
        GlobalField::CenterFree,
        GlobalField::Theme,
        GlobalField::Panel,
        GlobalField::Help,
    ];

    pub fn next(self) -> Self {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }

    pub fn prev(self) -> Self {
        let n = Self::ORDER.len();
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(i + n - 1) % n]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_cycles_all_controls() {
        let mut f = GlobalField::StartStop;
        for _ in 0..GlobalField::ORDER.len() {
            f = f.next();
        }
        assert_eq!(f, GlobalField::StartStop, "wraps after a full cycle");
    }

    #[test]
    fn prev_is_inverse_of_next() {
        assert_eq!(GlobalField::StartStop.next().prev(), GlobalField::StartStop);
        assert_eq!(GlobalField::Volume.next(), GlobalField::Mute);
        assert_eq!(GlobalField::Theme.next(), GlobalField::Panel);
        assert_eq!(GlobalField::Panel.next(), GlobalField::Help);
        assert_eq!(GlobalField::Help.next(), GlobalField::StartStop);
        assert_eq!(GlobalField::StartStop.prev(), GlobalField::Help);
    }
}
