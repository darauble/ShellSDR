//! The FAX VFO in the app (0.2.2, spec 15): the fax service's lifecycle,
//! audio from the engine, and the Work Area view's commands (function keys,
//! the F9 menu, the focused view's keys). The view is drawn by
//! `ui::fax_view`.

use serde_json::{Value, json};

use crate::app::AppState;
use crate::fax::ffi::Config;
use crate::fax::service::{FaxService, RecNaming};
use crate::vfo::types::fax::{self as faxtype, FaxSettings};
use crate::vfo::types::{DataItem, FAX};

/// What a function key or menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaxCmd {
    Help,
    Save,
    Display,
    PpmDown,
    PpmUp,
    Auto,
    Prev,
    Next,
    Menu,
    Follow,
    Step,
    PpmZero,
    About,
}

/// The function-key bar, left to right (spec 15.6).
pub const FKEYS: [(u8, FaxCmd); 9] = [
    (1, FaxCmd::Help),
    (2, FaxCmd::Save),
    (3, FaxCmd::Display),
    (4, FaxCmd::PpmDown),
    (5, FaxCmd::PpmUp),
    (6, FaxCmd::Auto),
    (7, FaxCmd::Prev),
    (8, FaxCmd::Next),
    (9, FaxCmd::Menu),
];

/// One menu item: its label, its key hint, and its command.
pub struct MenuItem {
    pub label: &'static str,
    pub key: &'static str,
    pub cmd: FaxCmd,
}

const fn item(label: &'static str, key: &'static str, cmd: FaxCmd) -> MenuItem {
    MenuItem { label, key, cmd }
}

/// The F9 menu bar: titles and their items.
pub const MENUS: [(&str, &[MenuItem]); 3] = [
    (
        "Picture",
        &[
            item("Save the picture", "F2", FaxCmd::Save),
            item("Older picture", "F7", FaxCmd::Prev),
            item("Newer picture", "F8", FaxCmd::Next),
            item("Follow the newest", "End", FaxCmd::Follow),
            item("Pixels / blocks", "F3", FaxCmd::Display),
        ],
    ),
    (
        "Slant",
        &[
            item("Auto slant", "F6", FaxCmd::Auto),
            item("Clock -", "F4", FaxCmd::PpmDown),
            item("Clock +", "F5", FaxCmd::PpmUp),
            item("Step 0.1 / 1 ppm", "s", FaxCmd::Step),
            item("Clock to 0 ppm", "0", FaxCmd::PpmZero),
        ],
    ),
    (
        "Help",
        &[
            item("Keys", "F1", FaxCmd::Help),
            item("About", "", FaxCmd::About),
        ],
    ),
];

/// How the picture is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    /// Unicode half blocks, two grey pixels per cell (any terminal).
    #[default]
    Blocks,
    /// A real picture (Kitty graphics protocol).
    Pixels,
}

/// The FAX view's own UI state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FaxUi {
    /// The open F9 menu: (title, item).
    pub menu: Option<(usize, usize)>,
    /// The latest notice and when it goes away (Unix seconds).
    pub notice: Option<(String, u64)>,
    /// The first picture line on screen; None = follow the newest lines.
    pub top: Option<i32>,
    /// Picture lines the view showed last frame.
    pub visible_lines: i32,
    /// The terminal can show real pictures.
    pub kitty: bool,
    /// Real pictures may be drawn this frame (no popup or menu over them).
    pub kitty_allowed: bool,
    /// A terminal cell in pixels (w, h), 0 = unknown.
    pub cell: (u16, u16),
    /// Set by the view: the picture to place after the frame.
    pub kitty_request: std::cell::RefCell<Option<crate::ui::fax_view::KittyRequest>>,
    /// The half-block pixels of the last frame and what they show.
    #[allow(clippy::type_complexity)]
    pub blocks: std::cell::RefCell<Option<((u64, i32, i32, usize, usize, i32, i32, u64), Vec<u8>)>>,
}

impl FaxUi {
    /// A cell's size in pixels (8 x 16 if the terminal does not say).
    pub fn cell_px(&self) -> (u16, u16) {
        if self.cell.0 == 0 || self.cell.1 == 0 {
            (8, 16)
        } else {
            self.cell
        }
    }
}

/// How long a notice stays (s).
const NOTICE_SECS: u64 = 6;

impl AppState {
    /// Whether the active VFO is a fax receiver (and the library loaded).
    pub fn fax_active(&self) -> bool {
        let v = self.radio.active();
        v.vfo_type == FAX && crate::vfo::types::usable(FAX).is_some()
    }

    /// Whether the FAX view is on screen (it then takes the F-keys).
    pub fn fax_view_shown(&self) -> bool {
        self.fax_active() && self.vfo_view_visible
    }

    /// Whether a Work Area view with its own keys is on screen.
    pub fn work_area_view_shown(&self) -> bool {
        self.dsc_view_shown()
            || self.hfdl_view_shown()
            || self.fax_view_shown()
            || self.digital_view_shown()
            || self.page_shown()
    }

    fn fax_settings(&self) -> FaxSettings {
        FaxSettings::from_settings(&self.radio.active().type_settings())
    }

    /// The view mode from the VFO's settings (Pixels only where it works).
    pub fn fax_display(&self) -> Display {
        let s = self.radio.active().type_settings();
        match s.get(faxtype::DISPLAY).and_then(Value::as_str) {
            Some("Pixels") if self.fax_ui.kitty => Display::Pixels,
            _ => Display::Blocks,
        }
    }

    /// The ppm step of F4/F5 (0.1 or 1).
    pub fn fax_ppm_step(&self) -> f64 {
        let s = self.radio.active().type_settings();
        match s.get(faxtype::PPM_STEP).and_then(Value::as_f64) {
            Some(x) if x >= 1.0 => 1.0,
            _ => 0.1,
        }
    }

    fn fax_set(&mut self, key: &str, value: Value) {
        let mut s = self.radio.active().type_settings();
        s.insert(key.into(), value);
        self.radio.set_active_type_settings(&s);
    }

    /// Keep the service in step with the VFO: start it when the VFO becomes
    /// FAX, drop it when it stops being one, follow LPM/IOC/auto, and bring
    /// a slant found automatically into the settings.
    pub fn sync_fax(&mut self, now: i64) {
        self.nav.set_work_area(self.work_area_view_shown());
        if !self.fax_active() {
            if self.fax.is_some() {
                self.fax = None;
                self.fax_ui.menu = None;
                self.fax_ui.top = None;
            }
            return;
        }
        let f = self.fax_settings();
        let config = Config {
            lpm: f.lpm,
            width: f.width(),
            clock_ppm: f.clock_ppm,
        };
        match &mut self.fax {
            None => {
                let Ok(lib) = crate::fax::ffi::shared() else {
                    return;
                };
                self.fax = Some(FaxService::new(
                    lib,
                    config,
                    f.auto_slant,
                    crate::fax::service::default_cache_limit(),
                ));
            }
            Some(s) => {
                let c = s.config();
                if (c.lpm, c.width) != (config.lpm, config.width) {
                    s.set_config(config);
                    self.fax_ui.top = None;
                }
                if s.auto() != f.auto_slant {
                    s.set_auto(f.auto_slant);
                }
                s.tick();
            }
        }
        let svc = self.fax.as_mut().expect("created above");
        let found = svc.take_ppm_found();
        let notices = svc.take_notices();
        let rec_error = svc.take_rec_error();
        if let Some(ppm) = found {
            self.fax_set(faxtype::CLOCK_PPM, json!((ppm * 100.0).round() / 100.0));
        }
        for n in notices {
            self.fax_notice(n);
        }
        if let Some(e) = rec_error {
            self.radio.stop_recording();
            self.rec_error = Some(e);
        }
        if self
            .fax_ui
            .notice
            .as_ref()
            .is_some_and(|(_, until)| (now.max(0) as u64) >= *until)
        {
            self.fax_ui.notice = None;
        }
    }

    pub fn fax_notice(&mut self, text: String) {
        self.fax_ui.notice = Some((text, self.now_secs + NOTICE_SECS));
    }

    /// Data from the engine: DSC calls to the DSC service, fax audio to the
    /// fax service.
    pub fn receive_data(&mut self, items: Vec<DataItem>, now: f64) {
        if items.is_empty() {
            return;
        }
        let (digital, items): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|i| matches!(i, DataItem::DigitalIq { .. }));
        self.digital_receive(digital, now);
        let (fax, other): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|i| matches!(i, DataItem::FaxAudio { .. }));
        let (hfdl, other): (Vec<_>, Vec<_>) = other
            .into_iter()
            .partition(|i| matches!(i, DataItem::HfdlFrame { .. } | DataItem::HfdlStatus { .. }));
        self.hfdl_receive(hfdl, now);
        self.dsc_receive(other, now);
        if fax.is_empty() {
            return;
        }
        if self.fax.is_none() {
            self.sync_fax(now as i64);
        }
        let Some(svc) = self.fax.as_mut() else {
            return;
        };
        for item in fax {
            if let DataItem::FaxAudio { samples, rate } = item {
                svc.push(&samples, rate, now);
            }
        }
    }

    /// Start or stop the PNG recording of the fax (spec 15.4); called by
    /// the recording reconciler with the folder and the name template.
    pub fn fax_record(&mut self, on: bool, dir: &std::path::Path) {
        let v = self.radio.active();
        let naming = RecNaming {
            dir: dir.to_path_buf(),
            template: v.rec_template.clone(),
            vfo_name: v.name.clone(),
            freq_hz: v.frequency_hz as f64,
        };
        let Some(svc) = self.fax.as_mut() else {
            return;
        };
        match (on, svc.recording()) {
            (true, false) => svc.start_recording(naming),
            (false, true) => svc.stop_recording(),
            _ => {}
        }
    }

    /// A function key (only for the FAX view).
    pub fn fax_fkey(&mut self, n: u8) {
        if !self.fax_view_shown() {
            return;
        }
        if let Some((_, cmd)) = FKEYS.iter().find(|(k, _)| *k == n) {
            self.fax_command(*cmd);
        }
    }

    /// Carry out a command of the view.
    pub fn fax_command(&mut self, cmd: FaxCmd) {
        self.fax_ui.menu = None;
        if self.fax.is_none() {
            return;
        }
        match cmd {
            FaxCmd::Help => self.toggle_help(),
            FaxCmd::Save => self.fax_save(),
            FaxCmd::Display => {
                if !self.fax_ui.kitty {
                    self.fax_notice(
                        "this terminal shows no pictures (Kitty, WezTerm, Ghostty do)".into(),
                    );
                    return;
                }
                let next = match self.fax_display() {
                    Display::Blocks => "Pixels",
                    Display::Pixels => "Blocks",
                };
                self.fax_set(faxtype::DISPLAY, json!(next));
            }
            FaxCmd::PpmDown | FaxCmd::PpmUp | FaxCmd::PpmZero => {
                let step = self.fax_ppm_step();
                let cur = self
                    .fax
                    .as_ref()
                    .and_then(|s| s.current())
                    .map_or(self.fax_settings().clock_ppm, |p| p.ppm);
                let ppm = match cmd {
                    FaxCmd::PpmDown => cur - step,
                    FaxCmd::PpmUp => cur + step,
                    _ => 0.0,
                };
                let ppm =
                    ((ppm * 100.0).round() / 100.0).clamp(-faxtype::MAX_PPM, faxtype::MAX_PPM);
                // a manual correction: auto slant would undo it
                if self.fax_settings().auto_slant {
                    self.fax_set(faxtype::AUTO_SLANT, json!(false));
                    self.fax_notice("auto slant off".into());
                }
                self.fax_set(faxtype::CLOCK_PPM, json!(ppm));
                if let Some(s) = self.fax.as_mut() {
                    s.set_auto(false);
                    s.set_ppm(ppm);
                }
            }
            FaxCmd::Auto => {
                let on = !self.fax_settings().auto_slant;
                self.fax_set(faxtype::AUTO_SLANT, json!(on));
                if let Some(s) = self.fax.as_mut() {
                    s.set_auto(on);
                }
            }
            FaxCmd::Prev | FaxCmd::Next => {
                if let Some(s) = self.fax.as_mut() {
                    s.select_by(if cmd == FaxCmd::Prev { -1 } else { 1 });
                }
                self.fax_ui.top = None;
            }
            FaxCmd::Follow => {
                if let Some(s) = self.fax.as_mut() {
                    s.follow();
                }
                self.fax_ui.top = None;
            }
            FaxCmd::Step => {
                let next = if self.fax_ppm_step() >= 1.0 { 0.1 } else { 1.0 };
                self.fax_set(faxtype::PPM_STEP, json!(next));
                self.fax_notice(format!("clock step {next} ppm"));
            }
            FaxCmd::Menu => self.fax_ui.menu = Some((0, 0)),
            FaxCmd::About => self.fax_notice(about_text()),
        }
    }

    /// F2: the picture on screen (from its oldest line in memory down to
    /// the last line shown) to a PNG in the FAX recordings folder (`Dir:`);
    /// the memory before it is freed.
    fn fax_save(&mut self) {
        let dir = std::path::PathBuf::from(crate::app::expand_home(self.active_rec_dir()));
        let (name, freq) = {
            let v = self.radio.active();
            (v.name.clone(), v.frequency_hz)
        };
        let top = self.fax_ui.top;
        let shown = self.fax_ui.visible_lines;
        let Some(svc) = self.fax.as_mut() else { return };
        let Some(cur) = svc.current() else {
            self.fax_notice("no picture yet".into());
            return;
        };
        let end = match top {
            Some(t) => t + shown,
            None => cur.lines,
        };
        let t =
            crate::record::naming::UtcDateTime::from_unix(svc.picture_time(&cur).max(0.0) as u64);
        let stem = format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}-{name}-{freq}Hz-FAX",
            t.year, t.month, t.day, t.hour, t.minute, t.second
        );
        let (msg, saved) = match svc.save(&dir, &stem, end) {
            Ok(p) => (format!("saved {}", p.display()), Some(p)),
            Err(e) => (format!("save failed: {e}"), None),
        };
        self.fax_notice(msg);
        if let Some(p) = saved {
            use crate::report::model::{Category, Level, keys};
            self.report_count(keys::FAX_SAVED, 1);
            self.report_event(
                Level::Info,
                Category::Fax,
                "Picture saved",
                Some(p.display().to_string()),
            );
        }
    }

    /// The toggle state of a menu command (None: not a toggle).
    pub fn fax_toggle_state(&self, cmd: FaxCmd) -> Option<bool> {
        let svc = self.fax.as_ref()?;
        match cmd {
            FaxCmd::Auto => Some(svc.auto()),
            FaxCmd::Follow => Some(svc.following() && self.fax_ui.top.is_none()),
            FaxCmd::Display => Some(self.fax_display() == Display::Pixels),
            _ => None,
        }
    }

    /// Keys for the open F9 menu. Returns whether the menu took the key.
    pub fn fax_menu_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::run::Action;
        let Some((top, item)) = self.fax_ui.menu else {
            return false;
        };
        let n = MENUS.len();
        let items = MENUS[top].1.len();
        match action {
            Action::Left => self.fax_ui.menu = Some(((top + n - 1) % n, 0)),
            Action::Right => self.fax_ui.menu = Some(((top + 1) % n, 0)),
            Action::Up => self.fax_ui.menu = Some((top, (item + items - 1) % items)),
            Action::Down => self.fax_ui.menu = Some((top, (item + 1) % items)),
            Action::Enter => self.fax_command(MENUS[top].1[item].cmd),
            Action::Escape | Action::FKey(9) => self.fax_ui.menu = None,
            Action::Quit => {
                self.fax_ui.menu = None;
                self.request_quit();
            }
            _ => {}
        }
        true
    }

    /// Keys for the focused FAX view: scrolling the picture, older/newer
    /// pictures. Returns whether it took the key.
    pub fn fax_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::run::Action;
        let Some(svc) = self.fax.as_ref() else {
            return false;
        };
        let Some(cur) = svc.current() else {
            return false;
        };
        let shown = self.fax_ui.visible_lines.max(1);
        let page = (shown * 3 / 4).max(1);
        let bottom_top = (cur.lines - shown).max(cur.first_line);
        let top = self.fax_ui.top.unwrap_or(bottom_top);
        let to = match action {
            Action::Up => top - shown / 20 - 1,
            Action::Down => top + shown / 20 + 1,
            Action::PageUp => top - page,
            Action::PageDown => top + page,
            Action::Home => cur.first_line,
            Action::End => {
                self.fax_command(FaxCmd::Follow);
                return true;
            }
            Action::Left => {
                self.fax_command(FaxCmd::Prev);
                return true;
            }
            Action::Right => {
                self.fax_command(FaxCmd::Next);
                return true;
            }
            Action::Char('s') => {
                self.fax_command(FaxCmd::Step);
                return true;
            }
            Action::TypeDigit(0) => {
                self.fax_command(FaxCmd::PpmZero);
                return true;
            }
            _ => return false,
        };
        let to = to.clamp(cur.first_line, bottom_top);
        // scrolling to the bottom of the picture being received follows it
        self.fax_ui.top = (to < bottom_top || cur.open == 0).then_some(to);
        true
    }
}

/// The About notice.
pub fn about_text() -> String {
    match crate::fax::ffi::shared() {
        Ok(l) => format!(
            "FAX library {} (GPL v3, from OpenCPN weatherfax via KiwiSDR)",
            l.version()
        ),
        Err(e) => e,
    }
}
