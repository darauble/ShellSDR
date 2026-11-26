//! Help popup: every key binding, grouped by section (pure model + render).
//!
//! Opened with `?` or the Global bar's `[?] Help` button. It is modal: the
//! arrows, PgUp/PgDn, Home/End and the wheel scroll it; Esc, ENTER, `?` or a
//! click outside close it. The content is static text kept next to the key
//! translation (`ui::run::translate_key`): when a binding changes, update the
//! table here (a test checks that every global accelerator is listed).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};

use crate::ui::theme::Theme;

/// One help section: a heading and its (keys, description) rows.
pub struct HelpSection {
    pub title: &'static str,
    pub rows: &'static [(&'static str, &'static str)],
}

/// The help content, in display order.
pub const SECTIONS: &[HelpSection] = &[
    HelpSection {
        title: "Global",
        rows: &[
            ("?", "Show / hide this help"),
            ("q", "Quit (stop the source first)"),
            ("Space", "Start / stop the source"),
            ("m", "Mute / unmute the audio"),
            ("[", "Spectrum on / off"),
            ("]", "Waterfall on / off"),
            ("c", "Center / Free (what a spectrum click does)"),
            ("t", "Next theme"),
            ("w", "Work Area: Spectrum / VFO tab"),
            ("l", "Work Area: Tabs / Split layout"),
            ("o", "Open the focused module's page"),
            ("Ctrl+Shift+←/→", "Left panel narrower / wider"),
            ("/", "Find a bookmark (focuses Bookmarks)"),
        ],
    },
    HelpSection {
        title: "Navigation",
        rows: &[
            ("Tab / Shift+Tab", "Next / previous region"),
            ("Up / Down", "Previous / next control in the region"),
            ("Left / Right", "Change the selected control"),
            ("Enter", "Activate: toggle, open a list, edit"),
            ("Esc", "Cancel the edit / close the list"),
            ("Esc in a page", "Give the Work Area back to the VFO"),
            ("+ / -", "Expand / collapse the focused module"),
            ("PgUp / PgDn", "Scroll the left panel"),
        ],
    },
    HelpSection {
        title: "Frequency editing",
        rows: &[
            ("Enter", "Start editing / apply"),
            ("Left / Right", "Select a digit"),
            ("Up / Down", "Increase / decrease the digit"),
            ("0-9", "Type a digit"),
            ("z", "Zero the digits right of the cursor"),
            ("Esc", "Cancel (nothing is retuned)"),
        ],
    },
    HelpSection {
        title: "Source: baseband file",
        rows: &[
            ("Enter on Kind", "SoapySDR / File / Network"),
            ("Enter on File", "Open the file dialog"),
            ("Left / Right", "Select a transport / skip button"),
            ("Enter", "Press it (Start/Stop, Max, Rewind, skip)"),
            ("[Max] / [1×]", "As fast as decoding allows / real time"),
        ],
    },
    HelpSection {
        title: "Source: network",
        rows: &[
            ("Enter on Server", "host:port of a shellsdr --serve"),
            ("Enter on Token", "The server's --token"),
            ("← / → on Audio", "Opus bit rate or PCM"),
            ("Enter on Device", "Choose the server's input"),
            ("Space", "Start / stop it on the server"),
            ("[Open window]", "A sharing ShellSDR's screen here"),
        ],
    },
    HelpSection {
        title: "Sharing and windows",
        rows: &[
            ("Enter on Share", "Share this screen: On / Off"),
            ("Enter on Listen", "Address (0.0.0.0: every interface)"),
            ("Enter on Token", "The secret a viewer must give"),
            ("[Disconnect]", "Close the viewer's window"),
            ("F12 / Ctrl+]", "In a window: back to this ShellSDR"),
            ("Alt+M", "In a window: mute it here"),
            ("Alt+C", "In a window: its compression"),
            ("Ctrl+Up / Down", "In a window: its volume here"),
        ],
    },
    HelpSection {
        title: "File dialog",
        rows: &[
            ("Up / Down", "Move in the list"),
            ("PgUp / PgDn", "Page through the list"),
            ("Enter / →", "Open the folder / choose the file"),
            ("← / Backspace", "Parent folder ([Up])"),
            ("a-z", "Jump to the next name starting with it"),
            ("~", "Home folder ([Home])"),
            ("Tab", "List / buttons ([Path…] types a path)"),
            ("Esc", "Close ([Cancel])"),
        ],
    },
    HelpSection {
        title: "Spectrum / Waterfall",
        rows: &[
            ("a", "dB range: auto / manual"),
            (", / .", "dB ceiling down / up"),
            ("< / >", "dB floor down / up"),
        ],
    },
    HelpSection {
        title: "Bookmarks list",
        rows: &[
            ("Up / Down", "Move (past the ends: Find / [Manager])"),
            ("Left / Right", "Previous / next bookmark list"),
            ("Home / End", "First / last bookmark"),
            ("PgUp / PgDn", "Page through the bookmarks"),
            ("Enter", "Tune the selected bookmark"),
            ("/", "Type a filter (Enter keeps, Esc clears)"),
            ("[Quick Add]", "The VFO into the list, no editor"),
        ],
    },
    HelpSection {
        title: "Bookmarks page ([Manager], o)",
        rows: &[
            ("Up / Down", "Lists, buttons, settings, table"),
            ("Left / Right", "Buttons, steppers; list on the rows"),
            ("Enter", "Press / open the list / tune"),
            ("F2 / F3", "Add (from the VFO) / edit, in the page"),
            ("F4 / F5", "Copy / move to another list"),
            ("F6 / F7", "Tune / find"),
            ("F8 / Delete", "Remove the bookmark (asks first)"),
            ("Esc", "Back to the VFO view"),
        ],
    },
    HelpSection {
        title: "Schedule",
        rows: &[
            ("Up / Down", "Move between controls and jobs"),
            ("Home / End", "First / last job"),
            ("Enter", "Switch / open the job on the page"),
            ("Scheduler: [On]", "Also resumes the current job"),
        ],
    },
    HelpSection {
        title: "Report page ([Report], o)",
        rows: &[
            ("F3", "Today, Month, Lifetime, Log, Settings"),
            ("F4 / F5, ← / →", "Older / newer day or month"),
            ("↑ ↓ PgUp PgDn", "Scroll"),
            ("F6", "Log: all, warnings, errors, a category"),
            ("F7", "Local time / UTC"),
            ("Esc", "Back to the VFO view"),
        ],
    },
    HelpSection {
        title: "Schedule page ([Manager], o)",
        rows: &[
            ("Up / Down", "Switches, lists, list buttons, jobs"),
            ("Left / Right", "Switches, buttons; list elsewhere"),
            ("Enter", "Flip / press / edit the job"),
            ("[Activate]", "Only the active list (●) runs"),
            ("F6", "Move the job to another list"),
            ("F2 / F3", "Add / edit, in the page"),
            ("F4", "A new job from the selected one"),
            ("F5", "Switch the job off / on"),
            ("F8 / Delete", "Remove the job (asks first)"),
            ("Esc", "Back to the VFO view"),
        ],
    },
    HelpSection {
        title: "HFDL view (Work Area)",
        rows: &[
            ("F1", "This help"),
            ("F2 / F5", "Save the message as text / copy it"),
            ("F3 / v", "Messages, aircraft, ground stations"),
            ("F4", "Watch / unwatch the selected aircraft"),
            ("F6 / Enter", "Open / close the message details"),
            ("F7 / /", "Search (all words must match)"),
            ("F8 / f", "Follow the newest / hold the selection"),
            ("F9", "Menu (filters, watch, files, lookups...)"),
            ("Up/Down/PgUp/PgDn", "Move (the detail when it has focus)"),
            ("Home / End", "First / newest"),
            ("n / j / e", "Hide link / joined blocks; alerts only"),
            ("Enter", "On Aircraft: the aircraft's messages"),
            ("a", "Acknowledge the alert"),
            ("b", "Alert sound on / off"),
            ("Esc", "Clear the search and the alerts filter"),
            ("Tuning", "The VFO on a band center (HFDL bookmarks)"),
        ],
    },
    HelpSection {
        title: "DSC view (Work Area)",
        rows: &[
            ("F1", "This help"),
            ("F2 / F5", "Save the message as text / copy it"),
            ("F3", "Detail beside / below the list"),
            ("F6 / Enter", "Open / close the message details"),
            ("F7 / /", "Search (all words must match)"),
            ("F8 / f", "Follow the newest / hold the selection"),
            ("F9", "Menu (Tools: location, aprs.fi lookup...)"),
            ("Up/Down/PgUp/PgDn", "Move (the detail when it has focus)"),
            ("Home / End", "First / newest message"),
            ("d", "Distress and urgency only"),
            ("a", "Acknowledge the alert"),
            ("b", "Alert sound on / off"),
            ("Esc", "Clear the search and the filter"),
            ("Tab", "Messages, details, then the next region"),
        ],
    },
    HelpSection {
        title: "FAX view (Work Area)",
        rows: &[
            ("F1", "This help"),
            ("F2", "Save the picture shown, free its memory"),
            ("F3", "Real picture (Kitty...) / half blocks"),
            ("F4 / F5", "Clock - / + (Auto goes off)"),
            ("F6", "Auto slant on / off"),
            ("F7 / F8", "Older / newer picture (also Left/Right)"),
            ("F9", "Menu"),
            ("Up/Down/PgUp/PgDn", "Scroll the picture (holds it)"),
            ("Home / End", "Oldest lines / follow the newest"),
            ("s / 0", "Clock step 0.1 / 1 ppm; to 0"),
        ],
    },
    HelpSection {
        title: "Digital view (Work Area)",
        rows: &[
            ("F1", "This help"),
            ("F2", "Save the session as Markdown"),
            ("F3", "Next mode (MFSK-4 ... MFSK-128L)"),
            ("F4", "Sideband: Upper / Lower / Both (AM)"),
            ("F5 / F6", "AFC / RSID on or off"),
            ("F7 / F8", "Squelch - / + 5 (0 = off)"),
            ("F9", "Menu"),
            ("Waterfall click", "Set the carrier there"),
            ("Up/Down/PgUp/PgDn", "Scroll the text (holds it)"),
            ("Home / End", "Oldest text / follow the newest"),
            ("Left / Right", "Older / newer picture"),
            ("c", "Clear the text and pictures"),
        ],
    },
    HelpSection {
        title: "Editor forms and prompts",
        rows: &[
            ("Tab / Up / Down", "Next / previous field"),
            ("Enter", "Edit the field / press the button"),
            ("Esc", "Revert the field; again: cancel"),
            ("(any key)", "Is text while a form is open"),
        ],
    },
    HelpSection {
        title: "Mouse",
        rows: &[
            ("Click", "Focus, select and activate a control"),
            ("Click a digit", "Edit that frequency digit"),
            ("Click spectrum", "Tune there (also the waterfall)"),
            ("Wheel", "Change the selected control"),
            ("Wheel elsewhere", "Scroll the left panel"),
            ("Click a label", "Tune that bookmark (strip)"),
            ("Shift+drag", "Terminal text selection"),
        ],
    },
];

/// Width of the keys column.
const KEY_COL: usize = 17;
/// Popup width (borders included), clamped to the screen.
const WIDTH: u16 = 62;

/// A rendered help line: a section heading, a key row, or a blank spacer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLine {
    Heading(&'static str),
    Row(&'static str, &'static str),
    Blank,
}

/// The help content flattened into lines (blank line between sections).
pub fn lines() -> Vec<HelpLine> {
    let mut out = Vec::new();
    for (i, s) in SECTIONS.iter().enumerate() {
        if i > 0 {
            out.push(HelpLine::Blank);
        }
        out.push(HelpLine::Heading(s.title));
        out.extend(s.rows.iter().map(|(k, d)| HelpLine::Row(k, d)));
    }
    out
}

/// The open help popup: its scroll position and the screen it was last
/// drawn on (set by `reconcile` each frame, so key handling can clamp and
/// page without knowing the terminal size).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HelpView {
    /// Lines hidden above the top of the popup.
    pub scroll: u16,
    /// The full screen area of the last frame.
    pub area: Rect,
}

impl HelpView {
    /// Scroll by `delta` lines, clamped to `0..=max_scroll`.
    pub fn scroll_by(&mut self, delta: i32) {
        let max = i32::from(max_scroll(self.area));
        self.scroll = (i32::from(self.scroll) + delta).clamp(0, max) as u16;
    }

    /// Scroll a page (the popup's inner height) down / up.
    pub fn page(&mut self, down: bool) {
        let n = i32::from(popup_rect(self.area).height.saturating_sub(2).max(1));
        self.scroll_by(if down { n } else { -n });
    }

    /// Jump to the top / bottom.
    pub fn home(&mut self) {
        self.scroll = 0;
    }

    pub fn end(&mut self) {
        self.scroll = max_scroll(self.area);
    }

    /// Record the screen size and keep the scroll valid after a resize.
    pub fn reconcile(&mut self, area: Rect) {
        self.area = area;
        self.scroll = self.scroll.min(max_scroll(area));
    }
}

/// The popup's on-screen rectangle inside `area` (centered).
pub fn popup_rect(area: Rect) -> Rect {
    let w = WIDTH.min(area.width);
    let want_h = (lines().len() as u16).saturating_add(2);
    let h = want_h.min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    Rect::new(x, y, w, h)
}

/// The largest useful scroll offset for the popup in `area`.
pub fn max_scroll(area: Rect) -> u16 {
    let inner_h = popup_rect(area).height.saturating_sub(2);
    (lines().len() as u16).saturating_sub(inner_h)
}

/// Draw the help popup over `area`; returns its frame rect (for hit tests).
pub fn render_help(view: &HelpView, area: Rect, theme: &Theme, buf: &mut Buffer) -> Rect {
    let rect = popup_rect(area);
    Clear.render(rect, buf);
    let all = lines();
    let inner_h = rect.height.saturating_sub(2);
    let more_below = usize::from(view.scroll) + usize::from(inner_h) < all.len();
    let hint = match (view.scroll > 0, more_below) {
        (false, false) => "",
        (false, true) => " ↓ more ",
        (true, false) => " ↑ more ",
        (true, true) => " ↑↓ more ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(true))
        .title(" Help: keys ")
        .title_bottom(Line::from(format!("{hint}Esc/?/Enter close ")).right_aligned());
    let inner = block.inner(rect);
    block.render(rect, buf);

    let heading = theme
        .text_style()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    for (row, line) in all
        .iter()
        .skip(usize::from(view.scroll))
        .take(usize::from(inner.height))
        .enumerate()
    {
        let l = match line {
            HelpLine::Heading(t) => Line::from(Span::styled(*t, heading)),
            HelpLine::Row(k, d) => Line::from(vec![
                Span::styled(format!(" {k:<KEY_COL$}"), theme.text_style()),
                Span::styled(*d, theme.dim_style()),
            ]),
            HelpLine::Blank => Line::default(),
        };
        buf.set_line(inner.x, inner.y + row as u16, &l, inner.width);
    }
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<&'static str> {
        SECTIONS
            .iter()
            .flat_map(|s| s.rows.iter().map(|(k, _)| *k))
            .collect()
    }

    #[test]
    fn every_global_accelerator_is_listed() {
        let k = keys();
        for key in [
            "?", "q", "Space", "m", "[", "]", "c", "t", "w", "l", "/", "a", "z",
        ] {
            assert!(k.contains(&key), "help is missing {key:?}");
        }
        for key in [
            ", / .",
            "< / >",
            "+ / -",
            "PgUp / PgDn",
            "Tab / Shift+Tab",
            "Ctrl+Shift+←/→",
        ] {
            assert!(k.contains(&key), "help is missing {key:?}");
        }
    }

    #[test]
    fn rows_fit_the_popup() {
        // Leading space + key column + description must fit inside borders.
        for (k, d) in SECTIONS.iter().flat_map(|s| s.rows.iter()) {
            assert!(k.chars().count() <= KEY_COL, "key too long: {k}");
            let w = 1 + KEY_COL + d.chars().count();
            assert!(w <= usize::from(WIDTH - 2), "row too wide: {k} {d}");
        }
    }

    #[test]
    fn scroll_is_clamped_to_the_content() {
        let small = Rect::new(0, 0, 80, 20);
        let mut v = HelpView::default();
        v.reconcile(small);
        v.scroll_by(-5);
        assert_eq!(v.scroll, 0);
        v.scroll_by(1000);
        assert_eq!(v.scroll, max_scroll(small));
        assert!(v.scroll > 0, "content is taller than a 20-row screen");
        v.home();
        v.page(true);
        assert_eq!(v.scroll, 18, "a page is the inner height");
        v.end();
        assert_eq!(v.scroll, max_scroll(small));
        // A tall screen shows everything: nothing to scroll.
        v.reconcile(Rect::new(0, 0, 80, 200));
        assert_eq!(v.scroll, 0);
    }

    #[test]
    fn popup_is_centered_and_fits_tiny_screens() {
        let r = popup_rect(Rect::new(0, 0, 30, 5));
        assert_eq!((r.width, r.height), (30, 5));
        let r = popup_rect(Rect::new(0, 0, 100, 200));
        assert_eq!(r.width, WIDTH);
        assert_eq!(r.x, (100 - WIDTH) / 2);
    }
}
