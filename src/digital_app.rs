//! The Digital VFO in the app (0.2.5, spec 16): the service's lifecycle, IQ
//! from the engine, the settings it moves (RSID, AFC), recording, and the
//! Work Area view's commands. The view is drawn by `ui::digital_view`.

use serde_json::json;
use std::cell::Cell;

use crate::app::AppState;
use crate::digital::service::{DigitalService, RecNaming, Tuning};
use crate::mfsk::modes::MODES;
use crate::vfo::types::digital::{self as dtype};
use crate::vfo::types::{DIGITAL, DataItem};

/// What a function key or menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigiCmd {
    Help,
    Save,
    Mode,
    Sideband,
    Afc,
    Rsid,
    SqlDown,
    SqlUp,
    Menu,
    Follow,
    Clear,
    Prev,
    Next,
    About,
}

/// The function-key bar, left to right (spec 16.9).
pub const FKEYS: [(u8, DigiCmd); 9] = [
    (1, DigiCmd::Help),
    (2, DigiCmd::Save),
    (3, DigiCmd::Mode),
    (4, DigiCmd::Sideband),
    (5, DigiCmd::Afc),
    (6, DigiCmd::Rsid),
    (7, DigiCmd::SqlDown),
    (8, DigiCmd::SqlUp),
    (9, DigiCmd::Menu),
];

/// One menu item: its label, its key hint, and its command.
pub struct MenuItem {
    pub label: &'static str,
    pub key: &'static str,
    pub cmd: DigiCmd,
}

const fn item(label: &'static str, key: &'static str, cmd: DigiCmd) -> MenuItem {
    MenuItem { label, key, cmd }
}

/// The F9 menu bar: titles and their items.
pub const MENUS: [(&str, &[MenuItem]); 3] = [
    (
        "Text",
        &[
            item("Save the session", "F2", DigiCmd::Save),
            item("Follow the newest", "End", DigiCmd::Follow),
            item("Older picture", "Left", DigiCmd::Prev),
            item("Newer picture", "Right", DigiCmd::Next),
            item("Clear the text", "c", DigiCmd::Clear),
        ],
    ),
    (
        "Modem",
        &[
            item("Next mode", "F3", DigiCmd::Mode),
            item("Sideband", "F4", DigiCmd::Sideband),
            item("AFC", "F5", DigiCmd::Afc),
            item("RSID", "F6", DigiCmd::Rsid),
            item("Squelch -5", "F7", DigiCmd::SqlDown),
            item("Squelch +5", "F8", DigiCmd::SqlUp),
        ],
    ),
    (
        "Help",
        &[
            item("Keys", "F1", DigiCmd::Help),
            item("About", "", DigiCmd::About),
        ],
    ),
];

/// The Digital view's own UI state.
#[derive(Debug, Clone, Default)]
pub struct DigitalUi {
    /// The open F9 menu: (title, item).
    pub menu: Option<(usize, usize)>,
    /// The latest notice and when it goes away (Unix seconds).
    pub notice: Option<(String, u64)>,
    /// Text rows scrolled back from the newest (0 = follow).
    pub back: usize,
    /// The most rows there are to scroll back (set by the view).
    pub back_max: Cell<usize>,
    /// Text rows the view showed last frame.
    pub rows: Cell<usize>,
    /// The picture shown; None = the one being received, else the newest.
    pub picture: Option<u64>,
}

/// How long a notice stays (s).
const NOTICE_SECS: u64 = 6;

impl AppState {
    /// Whether the active VFO is a Digital receiver.
    pub fn digital_active(&self) -> bool {
        self.radio.active().vfo_type == DIGITAL
    }

    /// Whether the Digital view is on screen (it then takes the F-keys).
    pub fn digital_view_shown(&self) -> bool {
        self.digital_active() && self.vfo_view_visible
    }

    fn digital_set(&mut self, key: &str, value: serde_json::Value) {
        let mut s = self.radio.active().type_settings();
        s.insert(key.into(), value);
        self.radio.set_active_type_settings(&s);
    }

    fn digital_tuning(&self) -> Tuning {
        let v = self.radio.active();
        Tuning {
            vfo_hz: v.frequency_hz,
            sideband: dtype::config(&v.type_settings()).sideband,
        }
    }

    /// Keep the service in step with the VFO: start it when the VFO becomes
    /// Digital, drop it when it stops being one, hand it the settings, and
    /// bring the mode and carrier it moved to into the settings.
    pub fn sync_digital(&mut self, now: i64) {
        if !self.digital_active() {
            if self.digital.is_some() {
                self.digital = None;
                self.digital_ui.menu = None;
            }
            return;
        }
        let tuning = self.digital_tuning();
        let svc = self.digital.get_or_insert_with(|| {
            DigitalService::new(dtype::config(&self.radio.active().type_settings()))
        });
        svc.tick(tuning);
        let found = svc.take_found();
        let rec_error = svc.take_rec_error();
        if let Some(c) = found {
            self.digital_set(dtype::MODE, json!(c.mode.name));
            self.digital_set(dtype::CARRIER, json!((c.carrier * 10.0).round() / 10.0));
        }
        let c = dtype::config(&self.radio.active().type_settings());
        if let Some(svc) = self.digital.as_mut() {
            svc.set_config(c);
        }
        if let Some(e) = rec_error {
            self.radio.stop_recording();
            self.rec_error = Some(e);
        }
        if self
            .digital_ui
            .notice
            .as_ref()
            .is_some_and(|(_, until)| (now.max(0) as u64) >= *until)
        {
            self.digital_ui.notice = None;
        }
    }

    pub fn digital_notice(&mut self, text: String) {
        self.digital_ui.notice = Some((text, self.now_secs + NOTICE_SECS));
    }

    /// Channel IQ from the engine to the service; during file playback the
    /// time is the file's (as DSC).
    pub fn digital_receive(&mut self, items: Vec<DataItem>, now: f64) {
        if items.is_empty() {
            return;
        }
        if self.digital.is_none() {
            self.sync_digital(now as i64);
        }
        let time = self.dsc_file_time().map_or(now, |(at, _, _)| at);
        let Some(svc) = self.digital.as_mut() else {
            return;
        };
        for item in items {
            if let DataItem::DigitalIq { samples, rate } = item {
                svc.push(samples, rate, time);
            }
        }
    }

    /// Start or stop the Markdown recording (spec 16.7); called by the
    /// recording reconciler with the folder. Returns the finished
    /// recording's status when one stops.
    pub fn digital_record(
        &mut self,
        on: bool,
        dir: &std::path::Path,
        now: f64,
    ) -> Option<crate::digital::service::RecStatus> {
        let naming = self.digital_naming(dir);
        let svc = self.digital.as_mut()?;
        match (on, svc.recording()) {
            (true, false) => {
                svc.start_recording(naming, now);
                None
            }
            (false, true) => svc.stop_recording(),
            _ => None,
        }
    }

    /// How the active VFO names its recordings and pictures in `dir`.
    fn digital_naming(&self, dir: &std::path::Path) -> RecNaming {
        let v = self.radio.active();
        RecNaming {
            dir: dir.to_path_buf(),
            template: v.rec_template.clone(),
            vfo_name: v.name.clone(),
            freq_hz: v.frequency_hz as f64,
            mode: v.mode_label(),
        }
    }

    /// A function key (only for the Digital view).
    pub fn digital_fkey(&mut self, n: u8) {
        if !self.digital_view_shown() {
            return;
        }
        if let Some((_, cmd)) = FKEYS.iter().find(|(k, _)| *k == n) {
            self.digital_command(*cmd);
        }
    }

    /// Carry out a command of the view.
    pub fn digital_command(&mut self, cmd: DigiCmd) {
        self.digital_ui.menu = None;
        let c = dtype::config(&self.radio.active().type_settings());
        match cmd {
            DigiCmd::Help => self.toggle_help(),
            DigiCmd::Save => self.digital_save(),
            DigiCmd::Mode => {
                let i = MODES
                    .iter()
                    .position(|m| m.name == c.mode.name)
                    .unwrap_or(0);
                let next = MODES[(i + 1) % MODES.len()];
                self.digital_set(dtype::MODE, json!(next.name));
                self.digital_notice(format!("mode {}", next.name));
            }
            DigiCmd::Sideband => {
                use crate::mfsk::frontend::Sideband;
                let all = Sideband::ALL;
                let i = all.iter().position(|b| *b == c.sideband).unwrap_or(0);
                let next = all[(i + 1) % all.len()];
                self.digital_set(dtype::SIDEBAND, json!(next.name()));
                self.digital_notice(format!("sideband {}", next.name()));
            }
            DigiCmd::Afc => self.digital_set(dtype::AFC, json!(!c.afc)),
            DigiCmd::Rsid => self.digital_set(dtype::RSID, json!(!c.rsid)),
            DigiCmd::SqlDown | DigiCmd::SqlUp => {
                let step = if cmd == DigiCmd::SqlUp { 5.0 } else { -5.0 };
                let sql = (c.squelch + step).clamp(0.0, 100.0);
                self.digital_set(dtype::SQUELCH, json!(sql as i64));
                self.digital_notice(if sql == 0.0 {
                    "squelch off".into()
                } else {
                    format!("squelch {sql:.0}")
                });
            }
            DigiCmd::Menu => self.digital_ui.menu = Some((0, 0)),
            DigiCmd::Follow => {
                self.digital_ui.back = 0;
                self.digital_ui.picture = None;
            }
            DigiCmd::Clear => {
                if let Some(s) = self.digital.as_mut() {
                    s.clear();
                }
                self.digital_ui.back = 0;
                self.digital_ui.picture = None;
            }
            DigiCmd::Prev | DigiCmd::Next => self.digital_select_picture(cmd == DigiCmd::Prev),
            DigiCmd::About => self.digital_notice(about_text()),
        }
    }

    fn digital_select_picture(&mut self, older: bool) {
        let Some(svc) = self.digital.as_ref() else {
            return;
        };
        let ids: Vec<u64> = svc.pictures().iter().map(|p| p.id).collect();
        if ids.is_empty() {
            self.digital_notice("no picture yet".into());
            return;
        }
        let cur = self
            .digital_ui
            .picture
            .and_then(|id| ids.iter().position(|&i| i == id));
        self.digital_ui.picture = match (cur, older) {
            (None, true) => Some(ids[ids.len() - 1]),
            (None, false) => None,
            (Some(0), true) => Some(ids[0]),
            (Some(i), true) => Some(ids[i - 1]),
            (Some(i), false) if i + 1 < ids.len() => Some(ids[i + 1]),
            (Some(_), false) => None,
        };
    }

    /// F2: the session so far (text, marks, pictures) as Markdown in the
    /// Digital recordings folder (`Dir:`).
    fn digital_save(&mut self) {
        let dir = std::path::PathBuf::from(crate::app::expand_home(self.active_rec_dir()));
        let (name, freq) = {
            let v = self.radio.active();
            (v.name.clone(), v.frequency_hz)
        };
        let t = crate::record::naming::UtcDateTime::now();
        let stem = format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}-{name}-{freq}Hz-Digital",
            t.year, t.month, t.day, t.hour, t.minute, t.second
        );
        let naming = self.digital_naming(&dir);
        let Some(svc) = self.digital.as_mut() else {
            return;
        };
        if svc.lines().is_empty() {
            self.digital_notice("nothing received yet".into());
            return;
        }
        let msg = match svc.save(&dir, &stem, &naming) {
            Ok(p) => format!("saved {}", p.display()),
            Err(e) => format!("save failed: {e}"),
        };
        self.digital_notice(msg);
    }

    /// The toggle state of a menu command (None: not a toggle).
    pub fn digital_toggle_state(&self, cmd: DigiCmd) -> Option<bool> {
        let c = dtype::config(&self.radio.active().type_settings());
        match cmd {
            DigiCmd::Afc => Some(c.afc),
            DigiCmd::Rsid => Some(c.rsid),
            DigiCmd::Follow => Some(self.digital_ui.back == 0),
            _ => None,
        }
    }

    /// Keys for the open F9 menu. Returns whether the menu took the key.
    pub fn digital_menu_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::run::Action;
        let Some((top, item)) = self.digital_ui.menu else {
            return false;
        };
        let n = MENUS.len();
        let items = MENUS[top].1.len();
        match action {
            Action::Left => self.digital_ui.menu = Some(((top + n - 1) % n, 0)),
            Action::Right => self.digital_ui.menu = Some(((top + 1) % n, 0)),
            Action::Up => self.digital_ui.menu = Some((top, (item + items - 1) % items)),
            Action::Down => self.digital_ui.menu = Some((top, (item + 1) % items)),
            Action::Enter => self.digital_command(MENUS[top].1[item].cmd),
            Action::Escape | Action::FKey(9) => self.digital_ui.menu = None,
            Action::Quit => {
                self.digital_ui.menu = None;
                self.request_quit();
            }
            _ => {}
        }
        true
    }

    /// Keys for the focused Digital view: scrolling the text, older/newer
    /// pictures, clear. Returns whether it took the key.
    pub fn digital_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::run::Action;
        if self.digital.is_none() {
            return false;
        }
        let rows = self.digital_ui.rows.get().max(1);
        let page = (rows * 3 / 4).max(1);
        let max = self.digital_ui.back_max.get();
        let back = self.digital_ui.back;
        self.digital_ui.back = match action {
            Action::Up => (back + 1).min(max),
            Action::Down => back.saturating_sub(1),
            Action::PageUp => (back + page).min(max),
            Action::PageDown => back.saturating_sub(page),
            Action::Home => max,
            Action::End => {
                self.digital_command(DigiCmd::Follow);
                return true;
            }
            Action::Left => {
                self.digital_command(DigiCmd::Prev);
                return true;
            }
            Action::Right => {
                self.digital_command(DigiCmd::Next);
                return true;
            }
            Action::Char('c') => {
                self.digital_command(DigiCmd::Clear);
                return true;
            }
            _ => return false,
        };
        true
    }

    /// A click in the waterfall at `fraction` (0..1) of 0..`span` Hz sets
    /// the carrier there.
    pub fn digital_click_carrier(&mut self, hz: f64) {
        let hz = hz.clamp(
            crate::mfsk::decoder::CARRIER_RANGE.0,
            crate::mfsk::decoder::CARRIER_RANGE.1,
        );
        let hz = (hz * 10.0).round() / 10.0;
        self.digital_set(dtype::CARRIER, json!(hz));
        self.digital_notice(format!("carrier {hz:.0} Hz"));
    }
}

/// The About notice (with the credits, spec 16.1).
pub fn about_text() -> String {
    "Digital: MFSK with pictures and RSID, ported from fldigi (W1HKJ, M0GLD, VK2ETA; gMFSK OH2BNS; RSID F6CTE), GPL v3 or later".into()
}
