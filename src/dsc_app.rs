//! The DSC VFO in the app (0.2.1, spec 14): the message service's
//! lifecycle, decoded calls from the engine, and the Work Area view's
//! commands (function keys, the F9 menu, the focused view's keys, prompts).
//! The view itself is drawn by `ui::dsc_view`.

use crate::app::{AppState, PromptTarget};
use crate::dsc::record::TimeSource;
use crate::dsc::service::{DscPrefs, DscService, Reception};
use crate::vfo::types::{DSC, DataItem, DecoderMeter};

/// What a function key or menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DscCmd {
    Help,
    Save,
    Layout,
    Copy,
    Detail,
    Search,
    Follow,
    Menu,
    DistressOnly,
    ClearFilters,
    Location,
    Lookup,
    ApiKey,
    Endpoint,
    Sound,
    Urgency,
    Notify,
    ReloadStations,
    Ack,
    About,
}

/// The function-key bar, left to right (spec 14.6).
pub const FKEYS: [(u8, DscCmd); 8] = [
    (1, DscCmd::Help),
    (2, DscCmd::Save),
    (3, DscCmd::Layout),
    (5, DscCmd::Copy),
    (6, DscCmd::Detail),
    (7, DscCmd::Search),
    (8, DscCmd::Follow),
    (9, DscCmd::Menu),
];

/// One menu item: its label, its key hint, and its command.
pub struct MenuItem {
    pub label: &'static str,
    pub key: &'static str,
    pub cmd: DscCmd,
}

const fn item(label: &'static str, key: &'static str, cmd: DscCmd) -> MenuItem {
    MenuItem { label, key, cmd }
}

/// The F9 menu bar: titles and their items.
pub const MENUS: [(&str, &[MenuItem]); 4] = [
    (
        "View",
        &[
            item("Detail", "F6", DscCmd::Detail),
            item("Detail layout", "F3", DscCmd::Layout),
            item("Follow / Hold", "F8 f", DscCmd::Follow),
        ],
    ),
    (
        "Filter",
        &[
            item("Search...", "F7 /", DscCmd::Search),
            item("Distress only", "d", DscCmd::DistressOnly),
            item("Clear filters", "Esc", DscCmd::ClearFilters),
        ],
    ),
    (
        "Tools",
        &[
            item("Set my location...", "", DscCmd::Location),
            item("Online ship lookup", "", DscCmd::Lookup),
            item("Set aprs.fi API key...", "", DscCmd::ApiKey),
            item("Set lookup endpoint...", "", DscCmd::Endpoint),
            item("Alert sound", "b", DscCmd::Sound),
            item("Alert on urgency", "", DscCmd::Urgency),
            item("Desktop notifications", "", DscCmd::Notify),
            item("Reload stations", "", DscCmd::ReloadStations),
            item("Acknowledge alert", "a", DscCmd::Ack),
        ],
    ),
    (
        "Help",
        &[
            item("Keys", "F1", DscCmd::Help),
            item("About", "", DscCmd::About),
        ],
    ),
];

/// Which part of the view the focused Work Area acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DscPart {
    #[default]
    Messages,
    Detail,
}

/// The DSC view's own UI state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DscUi {
    pub part: DscPart,
    pub detail_open: bool,
    /// First message row shown, and rows the list showed last frame.
    pub list_top: usize,
    pub list_rows: usize,
    /// First detail line shown, and lines the detail showed last frame.
    pub detail_scroll: usize,
    pub detail_rows: usize,
    /// The open F9 menu: (title, item).
    pub menu: Option<(usize, usize)>,
    /// The latest notice and when it goes away (Unix seconds).
    pub notice: Option<(String, u64)>,
    /// The decoder's state (from the engine while it runs).
    pub meter: Option<DecoderMeter>,
}

/// How long a notice stays (s).
const NOTICE_SECS: u64 = 6;

/// The channel label of a DSC frequency: `8414.5 kHz`.
pub fn channel_label(hz: u64) -> String {
    format!("{:.1} kHz", hz as f64 / 1000.0)
}

impl AppState {
    /// Whether the active VFO is a DSC decoder.
    pub fn dsc_active(&self) -> bool {
        let v = self.radio.active();
        v.vfo_type == DSC && v.type_def().is_some()
    }

    /// Whether the DSC view is on screen (it then takes the F-keys).
    pub fn dsc_view_shown(&self) -> bool {
        self.dsc_active() && self.vfo_view_visible
    }

    /// Keep the service in step with the VFO: start it when the VFO becomes
    /// DSC, stop it (writing what is pending) when it stops being one,
    /// follow the message folder and the Tools settings; with `tick`, do
    /// the periodic work (midnight, the day file, lookups).
    pub fn sync_dsc(&mut self, now: i64, tick: bool) {
        self.nav.set_work_area(self.work_area_view_shown());
        if !self.dsc_active() {
            if self.dsc.is_some() {
                self.dsc = None;
                self.dsc_ui.menu = None;
            }
            return;
        }
        let settings = self.radio.active().type_settings();
        let rec = std::path::PathBuf::from(crate::app::expand_home(
            self.rec_dir_of(crate::vfo::types::DSC),
        ));
        let dir = crate::vfo::types::dsc::message_dir(&settings, &rec);
        let mut prefs = DscPrefs::from_settings(&settings);
        // the location is the program's (0.4.5): a DSC table's own is
        // adopted once, when none is set
        if self.location.is_none() && prefs.location.is_some() {
            self.location = prefs.location;
        }
        prefs.location = self.location;
        match &mut self.dsc {
            None => {
                self.dsc = Some(DscService::new(dir, local_tz(), prefs, now));
            }
            Some(s) => {
                if s.dir() != dir {
                    s.set_dir(dir, now);
                }
                if s.prefs() != &prefs {
                    s.set_prefs(prefs);
                }
                if tick {
                    s.tick(now);
                }
            }
        }
        if let Some(n) = self.dsc.as_mut().and_then(|s| s.take_notice()) {
            self.dsc_notice(n);
        }
        if self
            .dsc_ui
            .notice
            .as_ref()
            .is_some_and(|(_, until)| (now.max(0) as u64) >= *until)
        {
            self.dsc_ui.notice = None;
        }
    }

    /// Show a notice in the view (seed 07-ui "Notices").
    pub fn dsc_notice(&mut self, text: String) {
        self.dsc_ui.notice = Some((text, self.now_secs + NOTICE_SECS));
    }

    /// Whether to ring the bell (a new alert with the sound on).
    pub fn dsc_take_bell(&mut self) -> bool {
        self.dsc.as_mut().is_some_and(|s| s.take_effects().bell)
    }

    /// Calls decoded by the engine. `now` is the wall clock (UTC seconds);
    /// during file playback the time comes from the file (its name, else its
    /// modification time) plus the play position (seed TIM-02).
    pub fn dsc_receive(&mut self, items: Vec<DataItem>, now: f64) {
        if items.is_empty() {
            return;
        }
        if self.dsc.is_none() {
            self.sync_dsc(now as i64, false);
        }
        let label = channel_label(self.radio.active().frequency_hz);
        let file = self.dsc_file_time();
        let Some(svc) = self.dsc.as_mut() else {
            return;
        };
        for item in items {
            let DataItem::DscCall { symbols, age_s } = item else {
                continue;
            };
            let (utc, time_source, file_name) = match &file {
                Some((at, ts, name)) => ((at - age_s).floor() as i64, *ts, Some(name.clone())),
                None => ((now - age_s).floor() as i64, TimeSource::Wallclock, None),
            };
            svc.accept(
                Reception {
                    symbols,
                    utc,
                    time_source,
                    label: label.clone(),
                    file: file_name,
                },
                now as i64,
            );
        }
    }

    /// File playback: the instant the play position stands for, its time
    /// source and the file name (None for a live source).
    pub(crate) fn dsc_file_time(&self) -> Option<(f64, TimeSource, String)> {
        use crate::source::model::SourceKind;
        if self.source.kind != SourceKind::File {
            return None;
        }
        let path = self.source.file.path.as_ref()?;
        let meta = self.source.file.meta.as_ref()?;
        let p = std::path::Path::new(path);
        let (base, ts) = crate::dsc::decoder::file_base_time(p, &local_tz());
        let at =
            base as f64 + self.source.file.position as f64 / f64::from(meta.sample_rate.max(1));
        let name = p
            .file_name()
            .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
        Some((at, ts, name))
    }

    /// Change the Tools settings: kept in the VFO's DSC table and handed to
    /// the service.
    pub fn dsc_set_prefs(&mut self, prefs: DscPrefs) {
        let mut s = self.radio.active().type_settings();
        prefs.write_to(&mut s);
        self.radio.set_active_type_settings(&s);
        if let Some(svc) = self.dsc.as_mut() {
            svc.set_prefs(prefs);
        }
    }

    fn dsc_prefs(&self) -> DscPrefs {
        DscPrefs::from_settings(&self.radio.active().type_settings())
    }

    /// A function key (only for the DSC view; ShellSDR binds none).
    pub fn dsc_fkey(&mut self, n: u8) {
        if !self.dsc_view_shown() {
            return;
        }
        if let Some((_, cmd)) = FKEYS.iter().find(|(k, _)| *k == n) {
            self.dsc_command(*cmd);
        }
    }

    /// Carry out a command of the view.
    pub fn dsc_command(&mut self, cmd: DscCmd) {
        self.dsc_ui.menu = None;
        let Some(svc) = self.dsc.as_mut() else {
            return;
        };
        let mut prefs = svc.prefs().clone();
        match cmd {
            DscCmd::Help => self.toggle_help(),
            DscCmd::Save => match svc.default_save_path() {
                Some(p) => self.dsc_prompt(
                    PromptTarget::DscSave,
                    "Save message as",
                    p.display().to_string(),
                ),
                None => self.dsc_notice("no message selected".into()),
            },
            DscCmd::Layout => {
                prefs.detail_layout = prefs.detail_layout.next();
                self.dsc_set_prefs(prefs);
            }
            DscCmd::Copy => {
                let r = match svc.detail_text() {
                    None => "no message selected".to_string(),
                    Some(t) => match crate::dsc::service::copy_to_clipboard(&t) {
                        Ok(tool) => format!("copied ({tool})"),
                        Err(e) => e,
                    },
                };
                self.dsc_notice(r);
            }
            DscCmd::Detail => {
                self.dsc_ui.detail_open = !self.dsc_ui.detail_open;
                self.dsc_ui.detail_scroll = 0;
                if !self.dsc_ui.detail_open {
                    self.dsc_ui.part = crate::dsc_app::DscPart::Messages;
                }
            }
            DscCmd::Search => {
                let q = svc.view().query().to_string();
                self.dsc_prompt(
                    PromptTarget::DscSearch,
                    "Search (all words; empty clears)",
                    q,
                );
            }
            DscCmd::Follow => {
                let v = svc.view_mut();
                let on = !v.follow();
                v.set_follow(on);
            }
            DscCmd::Menu => self.dsc_ui.menu = Some((0, 0)),
            DscCmd::DistressOnly => {
                let v = svc.view_mut();
                let on = !v.distress_only();
                v.set_distress_only(on);
            }
            DscCmd::ClearFilters => svc.view_mut().clear_filters(),
            DscCmd::Location => {
                let loc = prefs
                    .location
                    .map(|(a, b)| format!("{a}, {b}"))
                    .unwrap_or_default();
                self.dsc_prompt(
                    PromptTarget::DscLocation,
                    "My location (lat, lon; empty clears)",
                    loc,
                );
            }
            DscCmd::Lookup => {
                prefs.lookup = !prefs.lookup;
                self.dsc_set_prefs(prefs);
            }
            DscCmd::ApiKey => {
                let k = prefs.api_key.clone();
                self.dsc_prompt(PromptTarget::DscApiKey, "aprs.fi API key", k);
            }
            DscCmd::Endpoint => {
                let e = prefs.endpoint.clone();
                self.dsc_prompt(
                    PromptTarget::DscEndpoint,
                    "Lookup URL with {mmsi} and {apikey} (empty = aprs.fi)",
                    e,
                );
            }
            DscCmd::Sound => {
                prefs.alert_sound = !prefs.alert_sound;
                self.dsc_set_prefs(prefs);
            }
            DscCmd::Urgency => {
                prefs.alert_urgency = !prefs.alert_urgency;
                self.dsc_set_prefs(prefs);
            }
            DscCmd::Notify => {
                prefs.desktop_notify = !prefs.desktop_notify;
                self.dsc_set_prefs(prefs);
            }
            DscCmd::ReloadStations => svc.reload_stations(),
            DscCmd::Ack => svc.acknowledge_alert(),
            DscCmd::About => self.show_notice(about_text()),
        }
    }

    fn dsc_prompt(&mut self, target: PromptTarget, title: &str, value: String) {
        self.prompt = Some(crate::ui::prompt::TextPrompt::new(title, value));
        self.prompt_target = Some(target);
    }

    /// ENTER on a DSC prompt: apply it, or say why not (the prompt stays).
    pub(crate) fn dsc_prompt_confirm(
        &mut self,
        target: PromptTarget,
        text: &str,
    ) -> Result<(), String> {
        let mut prefs = self.dsc_prefs();
        let t = text.trim();
        match target {
            PromptTarget::DscSearch => {
                if let Some(s) = self.dsc.as_mut() {
                    s.view_mut().set_query(t);
                }
            }
            PromptTarget::DscLocation => {
                prefs.location = if t.is_empty() {
                    None
                } else {
                    Some(crate::dsc::geo::parse_location(t).ok_or_else(|| {
                        format!("not a location: \"{t}\" (two signed decimal degrees, e.g. 54.687 25.280)")
                    })?)
                };
                self.location = prefs.location;
                self.dsc_set_prefs(prefs);
            }
            PromptTarget::DscApiKey => {
                prefs.api_key = crate::dsc::settings::clean_key(t);
                self.dsc_set_prefs(prefs);
            }
            PromptTarget::DscEndpoint => {
                if !t.is_empty() && !(t.starts_with("http://") || t.starts_with("https://")) {
                    return Err("the endpoint must be an http:// or https:// URL".into());
                }
                prefs.endpoint = t.to_string();
                self.dsc_set_prefs(prefs);
            }
            PromptTarget::DscSave => {
                let path = std::path::PathBuf::from(crate::app::expand_home(t));
                let svc = self.dsc.as_mut().ok_or("not a DSC VFO")?;
                svc.save_selected(&path)?;
                let prefs = svc.prefs().clone();
                self.dsc_set_prefs(prefs);
                self.dsc_notice(format!("saved to {}", path.display()));
            }
            _ => {}
        }
        Ok(())
    }

    /// The toggle state of a menu command (None: not a toggle).
    pub fn dsc_toggle_state(&self, cmd: DscCmd) -> Option<bool> {
        let svc = self.dsc.as_ref()?;
        let p = svc.prefs();
        match cmd {
            DscCmd::DistressOnly => Some(svc.view().distress_only()),
            DscCmd::Follow => Some(svc.view().follow()),
            DscCmd::Lookup => Some(p.lookup),
            DscCmd::Sound => Some(p.alert_sound),
            DscCmd::Urgency => Some(p.alert_urgency),
            DscCmd::Notify => Some(p.desktop_notify),
            _ => None,
        }
    }

    /// Keys for the open F9 menu. Returns whether the menu took the key.
    pub fn dsc_menu_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::run::Action;
        let Some((top, item)) = self.dsc_ui.menu else {
            return false;
        };
        let n = MENUS.len();
        let items = MENUS[top].1.len();
        match action {
            Action::Left => self.dsc_ui.menu = Some(((top + n - 1) % n, 0)),
            Action::Right => self.dsc_ui.menu = Some(((top + 1) % n, 0)),
            Action::Up => self.dsc_ui.menu = Some((top, (item + items - 1) % items)),
            Action::Down => self.dsc_ui.menu = Some((top, (item + 1) % items)),
            Action::Enter => self.dsc_command(MENUS[top].1[item].cmd),
            Action::Escape | Action::FKey(9) => self.dsc_ui.menu = None,
            Action::Quit => {
                self.dsc_ui.menu = None;
                self.request_quit();
            }
            _ => {}
        }
        true
    }

    /// Keys for the focused DSC view. Returns whether it took the key
    /// (others keep their ShellSDR meaning).
    pub fn dsc_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::nav::NavKey;
        use crate::ui::run::Action;
        let Some(svc) = self.dsc.as_mut() else {
            return false;
        };
        let ui = &mut self.dsc_ui;
        let page = ui.list_rows.max(1) as isize;
        let dpage = ui.detail_rows.max(1);
        match (ui.part, action) {
            // TAB visits the detail before leaving the view.
            (DscPart::Messages, Action::Nav(NavKey::Tab)) if ui.detail_open => {
                ui.part = DscPart::Detail
            }
            (DscPart::Detail, Action::Nav(NavKey::BackTab)) => ui.part = DscPart::Messages,
            (DscPart::Detail, Action::Nav(NavKey::Tab)) => {
                ui.part = DscPart::Messages;
                return false;
            }
            (_, Action::Nav(_)) => return false,
            (DscPart::Messages, Action::Up) => svc.view_mut().move_by(-1),
            (DscPart::Messages, Action::Down) => svc.view_mut().move_by(1),
            (DscPart::Messages, Action::PageUp) => svc.view_mut().move_by(-page),
            (DscPart::Messages, Action::PageDown) => svc.view_mut().move_by(page),
            (DscPart::Messages, Action::Home) => svc.view_mut().first(),
            (DscPart::Messages, Action::End) => svc.view_mut().last(),
            (DscPart::Detail, Action::Up) => ui.detail_scroll = ui.detail_scroll.saturating_sub(1),
            (DscPart::Detail, Action::Down) => ui.detail_scroll += 1,
            (DscPart::Detail, Action::PageUp) => {
                ui.detail_scroll = ui.detail_scroll.saturating_sub(dpage)
            }
            (DscPart::Detail, Action::PageDown) => ui.detail_scroll += dpage,
            (DscPart::Detail, Action::Home) => ui.detail_scroll = 0,
            (DscPart::Detail, Action::End) => ui.detail_scroll = usize::MAX / 2,
            (_, Action::Enter) => {
                self.dsc_command(DscCmd::Detail);
                return true;
            }
            (_, Action::Escape) => svc.view_mut().clear_filters(),
            (_, Action::Find) => {
                self.dsc_command(DscCmd::Search);
                return true;
            }
            (_, Action::ToggleDbAuto) => svc.acknowledge_alert(),
            (_, Action::Char('d')) => {
                self.dsc_command(DscCmd::DistressOnly);
                return true;
            }
            (_, Action::Char('f')) => {
                self.dsc_command(DscCmd::Follow);
                return true;
            }
            (_, Action::Char('b')) => {
                self.dsc_command(DscCmd::Sound);
                return true;
            }
            _ => return false,
        }
        true
    }

    /// Before drawing: keep the selected message inside the list window and
    /// the detail scroll in range (`rows` / `detail_rows` / `detail_lines`
    /// from the view's layout).
    pub fn reconcile_dsc(&mut self, rows: usize, detail_rows: usize, detail_lines: usize) {
        let ui = &mut self.dsc_ui;
        ui.list_rows = rows;
        ui.detail_rows = detail_rows;
        ui.detail_scroll = ui
            .detail_scroll
            .min(detail_lines.saturating_sub(detail_rows));
        let Some(svc) = &self.dsc else {
            return;
        };
        let n = svc.view().len();
        let sel = svc.view().selected_row().unwrap_or(n.saturating_sub(1));
        if rows == 0 {
            return;
        }
        if sel < ui.list_top {
            ui.list_top = sel;
        } else if sel >= ui.list_top + rows {
            ui.list_top = sel + 1 - rows;
        }
        ui.list_top = ui.list_top.min(n.saturating_sub(rows));
    }

    /// A click on message row `row` (index into the filtered list).
    pub fn dsc_click_row(&mut self, row: usize) {
        self.nav.focus = crate::ui::nav::Focus::WorkArea;
        self.dsc_ui.part = DscPart::Messages;
        if let Some(s) = self.dsc.as_mut() {
            let again = s.view().selected_row() == Some(row);
            s.view_mut().select_row(row);
            if again {
                self.dsc_command(DscCmd::Detail);
            }
        }
    }
}

/// The computer's time zone.
pub fn local_tz() -> crate::dsc::time::TimeZone {
    crate::dsc::time::TimeZone::system()
}

/// The About text (seed 07-ui "about": credits).
pub fn about_text() -> String {
    format!(
        "ShellSDR {} DSC decoder (ITU-R M.493). Decoder ported from DSQrust, built from the DSC seed. Signal core after fldigi's NAVTEX/DSC modem (Rik van Riel AB1KW), as adapted by Franco Venturi IW2DHW. GPL v3 or later.",
        crate::version::VERSION
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsc::enrich::samples;
    use crate::ui::run::Action;

    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A state whose Master is DSC, writing to a fresh folder, with the view
    /// on screen.
    fn dsc_state(name: &str) -> (AppState, Tmp) {
        let dir =
            std::env::temp_dir().join(format!("shellsdr-dscapp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = AppState::default();
        s.radio.set_type(DSC);
        let mut st = s.radio.active().type_settings();
        st.insert(
            "message_dir".into(),
            dir.to_string_lossy().into_owned().into(),
        );
        st.insert("desktop_notify".into(), false.into());
        s.radio.set_active_type_settings(&st);
        s.vfo_view_visible = true;
        s.now_secs = 1_792_497_600;
        s.sync_dsc(1_792_497_600, false);
        (s, Tmp(dir))
    }

    fn call(symbols: &[u8], age_s: f64) -> DataItem {
        DataItem::DscCall {
            symbols: symbols.to_vec(),
            age_s,
        }
    }

    #[test]
    fn the_service_lives_while_the_vfo_is_dsc() {
        let (mut s, _t) = dsc_state("life");
        assert!(s.dsc.is_some());
        assert!(s.nav.work_area, "the view joins the TAB cycle");
        s.radio.set_type("Voice");
        s.sync_dsc(1_792_497_601, true);
        assert!(s.dsc.is_none());
        assert!(!s.nav.work_area);
    }

    #[test]
    fn decoded_calls_get_the_wall_clock_and_the_vfo_label() {
        let (mut s, t) = dsc_state("receive");
        s.radio.active_mut_freq(8_414_500);
        s.dsc_receive(vec![call(&samples::SAFETY, 3.4)], 1_792_497_610.0);
        let svc = s.dsc.as_ref().unwrap();
        let e = svc.view().selected().unwrap();
        assert_eq!(e.record.utc, 1_792_497_606);
        assert_eq!(e.record.time_source, TimeSource::Wallclock);
        assert_eq!(e.record.source_label.as_deref(), Some("8414.5 kHz"));
        svc.flush();
        assert!(t.0.join("2026-10-20.ndjson").exists());
    }

    #[test]
    fn function_keys_act_only_while_the_view_is_shown() {
        let (mut s, _t) = dsc_state("fkeys");
        s.dsc_receive(vec![call(&samples::SAFETY, 0.0)], 1_792_497_610.0);
        s.dsc_fkey(6);
        assert!(s.dsc_ui.detail_open);
        s.dsc_fkey(8);
        assert!(!s.dsc.as_ref().unwrap().view().follow(), "hold");
        s.dsc_fkey(9);
        assert_eq!(s.dsc_ui.menu, Some((0, 0)));
        s.vfo_view_visible = false;
        s.dsc_fkey(6);
        assert!(s.dsc_ui.detail_open, "hidden view: F6 does nothing");
    }

    #[test]
    fn the_menu_moves_wraps_and_runs_its_items() {
        let (mut s, _t) = dsc_state("menu");
        s.dsc_fkey(9);
        assert!(s.dsc_menu_key(Action::Left));
        assert_eq!(s.dsc_ui.menu, Some((3, 0)), "wraps to Help");
        s.dsc_menu_key(Action::Left);
        s.dsc_menu_key(Action::Down);
        assert_eq!(s.dsc_ui.menu, Some((2, 1)), "Tools > Online ship lookup");
        assert_eq!(s.dsc_toggle_state(DscCmd::Lookup), Some(false));
        s.dsc_menu_key(Action::Enter);
        assert_eq!(s.dsc_ui.menu, None);
        assert_eq!(s.dsc_toggle_state(DscCmd::Lookup), Some(true));
        assert_eq!(
            s.radio.active().type_settings()["lookup"],
            true,
            "kept in the VFO's DSC table"
        );
        s.dsc_fkey(9);
        s.dsc_menu_key(Action::Escape);
        assert_eq!(s.dsc_ui.menu, None);
    }

    #[test]
    fn prompts_validate_and_persist() {
        let (mut s, _t) = dsc_state("prompts");
        s.dsc_command(DscCmd::Location);
        assert_eq!(s.prompt_target, Some(PromptTarget::DscLocation));
        assert!(
            s.dsc_prompt_confirm(PromptTarget::DscLocation, "north")
                .is_err()
        );
        s.dsc_prompt_confirm(PromptTarget::DscLocation, "54.687 25.28")
            .unwrap();
        assert_eq!(
            s.dsc.as_ref().unwrap().prefs().location,
            Some((54.687, 25.28))
        );
        assert!(
            s.dsc_prompt_confirm(PromptTarget::DscEndpoint, "ftp://x")
                .is_err()
        );
        s.dsc_prompt_confirm(PromptTarget::DscSearch, "safety")
            .unwrap();
        assert_eq!(s.dsc.as_ref().unwrap().view().query(), "safety");
    }

    #[test]
    fn focused_keys_move_open_and_filter() {
        let (mut s, _t) = dsc_state("keys");
        s.dsc_receive(
            vec![call(&samples::SAFETY, 2.0), call(&samples::SAFETY, 1.0)],
            1_792_497_610.0,
        );
        s.nav.focus = crate::ui::nav::Focus::WorkArea;
        assert!(s.dsc_key(Action::Up));
        assert_eq!(s.dsc.as_ref().unwrap().view().selected_row(), Some(0));
        assert!(s.dsc_key(Action::Enter));
        assert!(s.dsc_ui.detail_open);
        assert!(
            s.dsc_key(Action::Nav(crate::ui::nav::NavKey::Tab)),
            "into the detail"
        );
        assert_eq!(s.dsc_ui.part, DscPart::Detail);
        assert!(s.dsc_key(Action::Down));
        assert_eq!(s.dsc_ui.detail_scroll, 1);
        assert!(
            !s.dsc_key(Action::Nav(crate::ui::nav::NavKey::Tab)),
            "leaves the view"
        );
        assert!(s.dsc_key(Action::Char('d')));
        assert!(s.dsc.as_ref().unwrap().view().distress_only());
        assert!(s.dsc_key(Action::Escape));
        assert!(!s.dsc.as_ref().unwrap().view().distress_only());
        assert!(!s.dsc_key(Action::ToggleRun), "Space keeps its meaning");
    }

    #[test]
    fn the_list_window_follows_the_selection() {
        let (mut s, _t) = dsc_state("window");
        let calls = (0..30)
            .map(|i| call(&samples::SAFETY, 60.0 - i as f64))
            .collect();
        s.dsc_receive(calls, 1_792_497_610.0);
        s.reconcile_dsc(10, 0, 0);
        assert_eq!(s.dsc_ui.list_top, 20, "follow: the newest at the bottom");
        s.dsc.as_mut().unwrap().view_mut().first();
        s.reconcile_dsc(10, 0, 0);
        assert_eq!(s.dsc_ui.list_top, 0);
    }

    #[test]
    fn a_distress_call_rings_once() {
        let (mut s, _t) = dsc_state("bell");
        s.dsc_receive(vec![call(&samples::DISTRESS, 0.0)], 1_792_497_610.0);
        assert!(s.dsc_take_bell());
        assert!(!s.dsc_take_bell());
        s.nav.focus = crate::ui::nav::Focus::WorkArea;
        s.dsc_key(Action::ToggleDbAuto);
        assert!(
            s.dsc.as_ref().unwrap().alert().is_none(),
            "`a` acknowledges"
        );
    }

    #[test]
    fn channel_labels_read_like_dsc_frequencies() {
        assert_eq!(channel_label(8_414_500), "8414.5 kHz");
        assert_eq!(channel_label(12_577_000), "12577.0 kHz");
    }
}
