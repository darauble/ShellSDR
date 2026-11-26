//! Focus / navigation state machine (pure).
//!
//! Step 1 seam: TAB / Shift+TAB cycle focus across the focusable regions, and
//! `+` / `-` collapse or expand the focused left-panel module. This module is
//! deliberately free of any terminal I/O so it can be unit-tested directly;
//! crossterm key events are translated into `NavKey` at the UI edge.

/// The focusable regions of the screen, in TAB order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Top global-controls bar (Start/Stop, volume, toggles, frequency).
    Global,
    /// Source module (left panel).
    Source,
    /// Radio / VFOs module (left panel).
    Radio,
    /// Bookmarks module (left panel).
    Bookmarks,
    /// Schedule module (left panel).
    Schedule,
    /// Report module (left panel, 0.3.0).
    Report,
    /// The active VFO's Work Area view, when it takes keys (DSC, spec 14.6)
    /// and is on screen.
    WorkArea,
}

impl Focus {
    /// TAB order as a fixed cycle.
    const ORDER: [Focus; 7] = [
        Focus::Global,
        Focus::Source,
        Focus::Radio,
        Focus::Bookmarks,
        Focus::Schedule,
        Focus::Report,
        Focus::WorkArea,
    ];

    /// Next region in TAB order, wrapping around.
    fn next(self) -> Focus {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }

    /// Previous region in TAB order, wrapping around.
    fn prev(self) -> Focus {
        let n = Self::ORDER.len();
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Self::ORDER[(i + n - 1) % n]
    }

    /// Whether this region is a collapsible left-panel module.
    fn is_collapsible(self) -> bool {
        matches!(
            self,
            Focus::Source | Focus::Radio | Focus::Bookmarks | Focus::Schedule | Focus::Report
        )
    }
}

/// Abstract navigation inputs, decoupled from crossterm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKey {
    /// TAB: focus next region.
    Tab,
    /// Shift+TAB: focus previous region.
    BackTab,
    /// `+`: expand the focused module.
    Expand,
    /// `-`: collapse the focused module.
    Collapse,
}

/// Collapsed/expanded state of the three left-panel modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Collapsed {
    pub source: bool,
    pub radio: bool,
    pub bookmarks: bool,
    pub schedule: bool,
    pub report: bool,
}

impl Collapsed {
    fn set(&mut self, focus: Focus, value: bool) {
        match focus {
            Focus::Source => self.source = value,
            Focus::Radio => self.radio = value,
            Focus::Bookmarks => self.bookmarks = value,
            Focus::Schedule => self.schedule = value,
            Focus::Report => self.report = value,
            Focus::Global | Focus::WorkArea => {}
        }
    }
}

/// Navigation state: which region is focused and the modules' collapse flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavState {
    pub focus: Focus,
    pub collapsed: Collapsed,
    /// Whether the Work Area takes part in the TAB cycle (its view is on
    /// screen and takes keys).
    pub work_area: bool,
}

impl Default for NavState {
    fn default() -> Self {
        Self {
            focus: Focus::Global,
            collapsed: Collapsed::default(),
            work_area: false,
        }
    }
}

impl NavState {
    /// Enable or disable the Work Area stop; a focused Work Area that goes
    /// away hands the focus to Global.
    pub fn set_work_area(&mut self, on: bool) {
        self.work_area = on;
        if !on && self.focus == Focus::WorkArea {
            self.focus = Focus::Global;
        }
    }
}

/// Apply a navigation key, returning the new state. Pure function.
pub fn handle_nav(state: NavState, key: NavKey) -> NavState {
    let mut s = state;
    match key {
        NavKey::Tab => {
            s.focus = s.focus.next();
            if s.focus == Focus::WorkArea && !s.work_area {
                s.focus = s.focus.next();
            }
        }
        NavKey::BackTab => {
            s.focus = s.focus.prev();
            if s.focus == Focus::WorkArea && !s.work_area {
                s.focus = s.focus.prev();
            }
        }
        NavKey::Expand => {
            if s.focus.is_collapsible() {
                s.collapsed.set(s.focus, false);
            }
        }
        NavKey::Collapse => {
            if s.focus.is_collapsible() {
                s.collapsed.set(s.focus, true);
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_cycles_forward_and_wraps() {
        let s = NavState::default();
        assert_eq!(s.focus, Focus::Global);
        let s = handle_nav(s, NavKey::Tab);
        assert_eq!(s.focus, Focus::Source);
        let s = handle_nav(s, NavKey::Tab);
        assert_eq!(s.focus, Focus::Radio);
        let s = handle_nav(s, NavKey::Tab);
        assert_eq!(s.focus, Focus::Bookmarks);
        let s = handle_nav(s, NavKey::Tab);
        assert_eq!(s.focus, Focus::Schedule);
        let s = handle_nav(s, NavKey::Tab);
        assert_eq!(s.focus, Focus::Report);
        let s = handle_nav(s, NavKey::Tab);
        assert_eq!(s.focus, Focus::Global, "TAB wraps back to Global");
    }

    #[test]
    fn back_tab_cycles_backward_and_wraps() {
        let s = NavState::default();
        let s = handle_nav(s, NavKey::BackTab);
        assert_eq!(
            s.focus,
            Focus::Report,
            "Shift+TAB from Global wraps to Report"
        );
        let s = handle_nav(s, NavKey::BackTab);
        assert_eq!(s.focus, Focus::Schedule);
    }

    #[test]
    fn the_work_area_joins_the_cycle_only_when_enabled() {
        let mut s = NavState {
            focus: Focus::Report,
            ..NavState::default()
        };
        s.set_work_area(true);
        let s2 = handle_nav(s, NavKey::Tab);
        assert_eq!(s2.focus, Focus::WorkArea);
        assert_eq!(handle_nav(s2, NavKey::Tab).focus, Focus::Global);
        let back = handle_nav(
            NavState {
                focus: Focus::Global,
                ..s
            },
            NavKey::BackTab,
        );
        assert_eq!(back.focus, Focus::WorkArea);
        let mut s3 = s2;
        s3.set_work_area(false);
        assert_eq!(s3.focus, Focus::Global, "focus leaves a hidden view");
    }

    #[test]
    fn collapse_and_expand_focused_module() {
        let s = NavState {
            focus: Focus::Radio,
            ..NavState::default()
        };
        let s = handle_nav(s, NavKey::Collapse);
        assert!(s.collapsed.radio, "focused Radio collapses");
        assert!(!s.collapsed.source, "other modules unaffected");
        let s = handle_nav(s, NavKey::Expand);
        assert!(!s.collapsed.radio, "focused Radio expands again");
    }

    #[test]
    fn collapse_on_global_is_noop() {
        let s = NavState::default();
        assert_eq!(s.focus, Focus::Global);
        let after = handle_nav(s, NavKey::Collapse);
        assert_eq!(after, s, "Global is not collapsible; state unchanged");
    }
}
