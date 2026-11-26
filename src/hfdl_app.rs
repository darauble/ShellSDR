//! The HFDL VFO in the app (0.4.5, HFDL-ACARS seed): the message service's
//! lifecycle, decoded frames from the engine, and the Work Area view's
//! commands (function keys, the F9 menu, the focused view's keys, prompts).
//! The view itself is drawn by `ui::hfdl_view`.

use crate::app::{AppState, PromptTarget};
use crate::dsc::record::TimeSource;
use crate::hfdl::service::{HfdlPrefs, HfdlService, Reception, Tab};
use crate::vfo::types::{DataItem, HFDL};

/// What a function key or menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HfdlCmd {
    Help,
    Save,
    NextView,
    Messages,
    Aircraft,
    Stations,
    Watch,
    Copy,
    Detail,
    Search,
    Follow,
    Menu,
    HideLink,
    HideJoined,
    AlertsOnly,
    ClearFilters,
    AircraftMessages,
    WatchList,
    Location,
    KeywordsEmergency,
    KeywordsCaution,
    Bookmarks,
    AircraftFile,
    AirlinesFile,
    AirportsFile,
    LookupHexdb,
    LookupAdsbdb,
    Sound,
    AlertCaution,
    WatchAlerts,
    Notify,
    Ack,
    About,
}

/// The function-key bar, left to right.
pub const FKEYS: [(u8, HfdlCmd); 9] = [
    (1, HfdlCmd::Help),
    (2, HfdlCmd::Save),
    (3, HfdlCmd::NextView),
    (4, HfdlCmd::Watch),
    (5, HfdlCmd::Copy),
    (6, HfdlCmd::Detail),
    (7, HfdlCmd::Search),
    (8, HfdlCmd::Follow),
    (9, HfdlCmd::Menu),
];

/// The label of a function key.
pub fn fkey_label(cmd: HfdlCmd) -> &'static str {
    match cmd {
        HfdlCmd::Help => "Help",
        HfdlCmd::Save => "Save",
        HfdlCmd::NextView => "View",
        HfdlCmd::Watch => "Watch",
        HfdlCmd::Copy => "Copy",
        HfdlCmd::Detail => "Detail",
        HfdlCmd::Search => "Search",
        HfdlCmd::Follow => "Follow",
        HfdlCmd::Menu => "Menu",
        _ => "",
    }
}

/// One menu item: its label, its key hint, and its command.
pub struct MenuItem {
    pub label: &'static str,
    pub key: &'static str,
    pub cmd: HfdlCmd,
}

const fn item(label: &'static str, key: &'static str, cmd: HfdlCmd) -> MenuItem {
    MenuItem { label, key, cmd }
}

/// The F9 menu bar: titles and their items.
pub const MENUS: [(&str, &[MenuItem]); 5] = [
    (
        "View",
        &[
            item("Messages", "", HfdlCmd::Messages),
            item("Aircraft", "", HfdlCmd::Aircraft),
            item("Ground stations", "", HfdlCmd::Stations),
            item("Next view", "F3", HfdlCmd::NextView),
            item("Detail", "F6", HfdlCmd::Detail),
            item("Follow / Hold", "F8 f", HfdlCmd::Follow),
        ],
    ),
    (
        "Filter",
        &[
            item("Search...", "F7 /", HfdlCmd::Search),
            item("Hide link traffic", "n", HfdlCmd::HideLink),
            item("Hide joined blocks", "j", HfdlCmd::HideJoined),
            item("Alerts only", "e", HfdlCmd::AlertsOnly),
            item("Clear filters", "Esc", HfdlCmd::ClearFilters),
        ],
    ),
    (
        "Aircraft",
        &[
            item("Watch / unwatch selected", "F4", HfdlCmd::Watch),
            item("Messages of selected", "Enter", HfdlCmd::AircraftMessages),
            item("Watch list...", "", HfdlCmd::WatchList),
            item("Aircraft file...", "", HfdlCmd::AircraftFile),
            item("Airlines file...", "", HfdlCmd::AirlinesFile),
            item("Airports file...", "", HfdlCmd::AirportsFile),
            item("Online lookup: hexdb.io", "", HfdlCmd::LookupHexdb),
            item("Online lookup: adsbdb.com", "", HfdlCmd::LookupAdsbdb),
        ],
    ),
    (
        "Tools",
        &[
            item("Set my location...", "", HfdlCmd::Location),
            item("Emergency keywords...", "", HfdlCmd::KeywordsEmergency),
            item("Caution keywords...", "", HfdlCmd::KeywordsCaution),
            item("Add HFDL band bookmarks", "", HfdlCmd::Bookmarks),
            item("Alert sound", "b", HfdlCmd::Sound),
            item("Alert on caution", "", HfdlCmd::AlertCaution),
            item("Alert on watched", "", HfdlCmd::WatchAlerts),
            item("Desktop notifications", "", HfdlCmd::Notify),
            item("Acknowledge alert", "a", HfdlCmd::Ack),
        ],
    ),
    (
        "Help",
        &[
            item("Keys", "F1", HfdlCmd::Help),
            item("About", "", HfdlCmd::About),
        ],
    ),
];

/// Which part of the view the focused Work Area acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HfdlPart {
    #[default]
    List,
    Detail,
}

/// The HFDL view's own UI state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HfdlUi {
    pub part: HfdlPart,
    pub detail_open: bool,
    /// First list row shown, and rows the list showed last frame.
    pub list_top: usize,
    pub list_rows: usize,
    pub detail_scroll: usize,
    pub detail_rows: usize,
    /// The selected row of the aircraft and ground-station views.
    pub ac_sel: usize,
    pub gs_sel: usize,
    /// The open F9 menu: (title, item).
    pub menu: Option<(usize, usize)>,
    /// The latest notice and when it goes away (Unix seconds).
    pub notice: Option<(String, u64)>,
    /// The VFO frequency the channels were counted at.
    pub vfo_hz: u64,
}

/// How long a notice stays (s).
const NOTICE_SECS: u64 = 6;

/// The source label of a VFO frequency: `HFDL 8902.5 kHz`.
pub fn source_label(hz: u64) -> String {
    format!("HFDL {:.1} kHz", hz as f64 / 1000.0)
}

impl AppState {
    /// Whether the active VFO is an HFDL decoder.
    pub fn hfdl_active(&self) -> bool {
        let v = self.radio.active();
        v.vfo_type == HFDL && v.type_def().is_some()
    }

    /// Whether the HFDL view is on screen (it then takes the F-keys).
    pub fn hfdl_view_shown(&self) -> bool {
        self.hfdl_active() && self.vfo_view_visible
    }

    /// Keep the service in step with the VFO: start it when the VFO becomes
    /// HFDL, stop it when it stops being one, follow the message folder,
    /// the menus' settings and the location; with `tick`, the periodic work.
    pub fn sync_hfdl(&mut self, now: i64, tick: bool) {
        if !self.hfdl_active() {
            if self.hfdl.is_some() {
                self.hfdl = None;
                self.hfdl_ui.menu = None;
            }
            return;
        }
        let settings = self.radio.active().type_settings();
        let rec = std::path::PathBuf::from(crate::app::expand_home(self.rec_dir_of(HFDL)));
        let dir = crate::vfo::types::hfdl::message_dir(&settings, &rec);
        let prefs = HfdlPrefs::from_settings(&settings);
        let loc = self.location;
        match &mut self.hfdl {
            None => {
                self.hfdl = Some(HfdlService::new(
                    dir,
                    crate::dsc_app::local_tz(),
                    prefs,
                    loc,
                    now,
                ));
            }
            Some(s) => {
                if s.dir() != dir {
                    s.set_dir(dir, now);
                }
                if s.prefs() != &prefs {
                    s.set_prefs(prefs);
                }
                if s.location() != loc {
                    s.set_location(loc);
                }
                if tick {
                    s.tick(now);
                }
            }
        }
        let vfo = self.radio.active().frequency_hz;
        if self.hfdl_ui.vfo_hz != vfo {
            self.hfdl_ui.vfo_hz = vfo;
            if let Some(s) = self.hfdl.as_mut() {
                s.clear_channels();
            }
        }
        if let Some(n) = self.hfdl.as_mut().and_then(|s| s.take_notice()) {
            self.hfdl_notice(n);
        }
        if self
            .hfdl_ui
            .notice
            .as_ref()
            .is_some_and(|(_, until)| (now.max(0) as u64) >= *until)
        {
            self.hfdl_ui.notice = None;
        }
    }

    pub fn hfdl_notice(&mut self, text: String) {
        self.hfdl_ui.notice = Some((text, self.now_secs + NOTICE_SECS));
    }

    /// Whether to ring the bell (a new alert with the sound on).
    pub fn hfdl_take_bell(&mut self) -> bool {
        self.hfdl.as_mut().is_some_and(|s| s.take_effects().bell)
    }

    /// Frames and channel states from the engine. `now` is the wall clock
    /// (UTC s); during file playback the time comes from the file.
    pub fn hfdl_receive(&mut self, items: Vec<DataItem>, now: f64) {
        if items.is_empty() {
            return;
        }
        if self.hfdl.is_none() {
            self.sync_hfdl(now as i64, false);
        }
        let label = source_label(self.radio.active().frequency_hz);
        let file = self.dsc_file_time();
        let Some(svc) = self.hfdl.as_mut() else {
            return;
        };
        svc.set_clock(file.as_ref().map_or(now, |(at, _, _)| *at));
        let mut frames = Vec::new();
        for item in items {
            match item {
                DataItem::HfdlFrame {
                    freq_khz,
                    m1,
                    octets,
                    age_s,
                    level_db,
                    noise_db,
                    ferr_hz,
                } => {
                    let (utc, time_source, file_name) = match &file {
                        Some((at, ts, name)) => (at - age_s, *ts, Some(name.clone())),
                        None => (now - age_s, TimeSource::Wallclock, None),
                    };
                    // milliseconds are enough
                    let utc = (utc * 1000.0).round() / 1000.0;
                    frames.push(Reception {
                        freq_khz,
                        m1,
                        octets,
                        utc,
                        time_source,
                        level_db,
                        noise_db,
                        ferr_hz,
                        src: label.clone(),
                        file: file_name,
                    });
                }
                DataItem::HfdlStatus { channels } => svc.set_status(channels),
                _ => {}
            }
        }
        svc.accept(frames, now as i64);
    }

    /// Change the menus' settings: kept in the VFO's HFDL table and handed
    /// to the service.
    pub fn hfdl_set_prefs(&mut self, prefs: HfdlPrefs) {
        let mut s = self.radio.active().type_settings();
        prefs.write_to(&mut s);
        self.radio.set_active_type_settings(&s);
        if let Some(svc) = self.hfdl.as_mut() {
            svc.set_prefs(prefs);
        }
    }

    /// A function key (only for the HFDL view).
    pub fn hfdl_fkey(&mut self, n: u8) {
        if !self.hfdl_view_shown() {
            return;
        }
        if let Some((_, cmd)) = FKEYS.iter().find(|(k, _)| *k == n) {
            self.hfdl_command(*cmd);
        }
    }

    fn hfdl_prompt(&mut self, target: PromptTarget, title: &str, value: String) {
        self.prompt = Some(crate::ui::prompt::TextPrompt::new(title, value));
        self.prompt_target = Some(target);
    }

    /// The identity of the selected aircraft (aircraft view) or of the
    /// selected message's aircraft.
    fn hfdl_selected_identity(&self) -> Option<String> {
        let svc = self.hfdl.as_ref()?;
        match svc.tab {
            Tab::Aircraft => {
                let a = *svc.aircraft().get(self.hfdl_ui.ac_sel)?;
                a.icao.clone().or_else(|| a.registration.clone())
            }
            _ => svc.selected_identity(),
        }
    }

    /// Carry out a command of the view.
    pub fn hfdl_command(&mut self, cmd: HfdlCmd) {
        self.hfdl_ui.menu = None;
        let ident = self.hfdl_selected_identity();
        let Some(svc) = self.hfdl.as_mut() else {
            return;
        };
        let mut prefs = svc.prefs().clone();
        match cmd {
            HfdlCmd::Help => self.toggle_help(),
            HfdlCmd::Save => match svc.default_save_path() {
                Some(p) => self.hfdl_prompt(
                    PromptTarget::HfdlSave,
                    "Save message as",
                    p.display().to_string(),
                ),
                None => self.hfdl_notice("no message selected".into()),
            },
            HfdlCmd::NextView => svc.tab = svc.tab.next(),
            HfdlCmd::Messages => svc.tab = Tab::Messages,
            HfdlCmd::Aircraft => svc.tab = Tab::Aircraft,
            HfdlCmd::Stations => svc.tab = Tab::Stations,
            HfdlCmd::Watch => match ident {
                None => self.hfdl_notice("no aircraft selected".into()),
                Some(id) => {
                    let had = prefs.watch.iter().any(|w| w.eq_ignore_ascii_case(&id));
                    if had {
                        prefs.watch.retain(|w| !w.eq_ignore_ascii_case(&id));
                    } else {
                        prefs.watch.push(id.clone());
                    }
                    self.hfdl_set_prefs(prefs);
                    self.hfdl_notice(format!(
                        "{id} {}",
                        if had { "no longer watched" } else { "watched" }
                    ));
                }
            },
            HfdlCmd::Copy => {
                let r = match svc.detail_text() {
                    None => "no message selected".to_string(),
                    Some(t) => match crate::hfdl::service::copy_to_clipboard(&t) {
                        Ok(tool) => format!("copied ({tool})"),
                        Err(e) => e,
                    },
                };
                self.hfdl_notice(r);
            }
            HfdlCmd::Detail => {
                let ui = &mut self.hfdl_ui;
                ui.detail_open = !ui.detail_open;
                if !ui.detail_open {
                    ui.part = HfdlPart::List;
                }
                ui.detail_scroll = 0;
                svc.tab = Tab::Messages;
            }
            HfdlCmd::Search => {
                let q = svc.query().to_string();
                self.hfdl_prompt(
                    PromptTarget::HfdlSearch,
                    "Search (words: text, aircraft, flight, label, GS, kHz...)",
                    q,
                );
            }
            HfdlCmd::Follow => {
                let on = !svc.follow();
                svc.set_follow(on);
            }
            HfdlCmd::Menu => self.hfdl_ui.menu = Some((0, 0)),
            HfdlCmd::HideLink => {
                prefs.filter_link = !prefs.filter_link;
                self.hfdl_set_prefs(prefs);
            }
            HfdlCmd::HideJoined => {
                prefs.filter_joined = !prefs.filter_joined;
                self.hfdl_set_prefs(prefs);
            }
            HfdlCmd::AlertsOnly => {
                let on = !svc.alerts_only();
                svc.set_alerts_only(on);
            }
            HfdlCmd::ClearFilters => svc.clear_filters(),
            HfdlCmd::AircraftMessages => match ident {
                None => self.hfdl_notice("no aircraft selected".into()),
                Some(id) => {
                    svc.set_query(&id);
                    svc.tab = Tab::Messages;
                }
            },
            HfdlCmd::WatchList => {
                let w = prefs.watch.join(" ");
                self.hfdl_prompt(
                    PromptTarget::HfdlWatch,
                    "Watch list (ICAO addresses, registrations, flights)",
                    w,
                );
            }
            HfdlCmd::Location => {
                let loc = self
                    .location
                    .map(|(a, b)| format!("{a}, {b}"))
                    .unwrap_or_default();
                self.hfdl_prompt(
                    PromptTarget::HfdlLocation,
                    "My location (lat, lon; empty clears)",
                    loc,
                );
            }
            HfdlCmd::KeywordsEmergency => {
                let k = prefs.keywords.emergency.join(", ");
                self.hfdl_prompt(
                    PromptTarget::HfdlKeywordsEmergency,
                    "Emergency keywords (comma separated; empty = defaults)",
                    k,
                );
            }
            HfdlCmd::KeywordsCaution => {
                let k = prefs.keywords.caution.join(", ");
                self.hfdl_prompt(
                    PromptTarget::HfdlKeywordsCaution,
                    "Caution keywords (comma separated; empty = defaults)",
                    k,
                );
            }
            HfdlCmd::Bookmarks => {
                let n = self.add_hfdl_bookmarks();
                self.hfdl_notice(if n == 0 {
                    format!("the list {BOOKMARK_LIST} has every HFDL band")
                } else {
                    format!("{n} HFDL band bookmarks added to the list {BOOKMARK_LIST}")
                });
            }
            HfdlCmd::AircraftFile => self.hfdl_prompt(
                PromptTarget::HfdlAircraftFile,
                "Aircraft CSV file (OpenSky aircraftDatabase.csv; empty = none)",
                prefs.aircraft_file.clone(),
            ),
            HfdlCmd::AirlinesFile => self.hfdl_prompt(
                PromptTarget::HfdlAirlinesFile,
                "Airlines CSV file (OpenFlights airlines with a header; empty = none)",
                prefs.airlines_file.clone(),
            ),
            HfdlCmd::AirportsFile => self.hfdl_prompt(
                PromptTarget::HfdlAirportsFile,
                "Airports CSV file (OurAirports airports.csv; empty = none)",
                prefs.airports_file.clone(),
            ),
            HfdlCmd::LookupHexdb => {
                prefs.lookup_hexdb = !prefs.lookup_hexdb;
                if prefs.lookup_hexdb {
                    self.hfdl_notice("online lookups at hexdb.io: aircraft and routes are asked for (cached 24 h / 6 h)".into());
                }
                self.hfdl_set_prefs(prefs);
            }
            HfdlCmd::LookupAdsbdb => {
                prefs.lookup_adsbdb = !prefs.lookup_adsbdb;
                if prefs.lookup_adsbdb {
                    self.hfdl_notice("online lookups at adsbdb.com: aircraft and routes are asked for (cached 24 h / 6 h)".into());
                }
                self.hfdl_set_prefs(prefs);
            }
            HfdlCmd::Sound => {
                prefs.alert_sound = !prefs.alert_sound;
                self.hfdl_set_prefs(prefs);
            }
            HfdlCmd::AlertCaution => {
                prefs.alert_on_caution = !prefs.alert_on_caution;
                self.hfdl_set_prefs(prefs);
            }
            HfdlCmd::WatchAlerts => {
                prefs.watch_alerts = !prefs.watch_alerts;
                self.hfdl_set_prefs(prefs);
            }
            HfdlCmd::Notify => {
                prefs.notifications = !prefs.notifications;
                self.hfdl_set_prefs(prefs);
            }
            HfdlCmd::Ack => svc.acknowledge_alert(),
            HfdlCmd::About => self.hfdl_notice(about_text()),
        }
    }

    /// ENTER on an HFDL prompt: apply it, or say why not (the prompt stays).
    pub(crate) fn hfdl_prompt_confirm(
        &mut self,
        target: PromptTarget,
        text: &str,
    ) -> Result<(), String> {
        let Some(svc) = self.hfdl.as_mut() else {
            return Err("not an HFDL VFO".into());
        };
        let mut prefs = svc.prefs().clone();
        let t = text.trim();
        let list = |t: &str| -> Vec<String> {
            t.split(',')
                .map(|w| w.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|w| !w.is_empty())
                .collect()
        };
        match target {
            PromptTarget::HfdlSearch => svc.set_query(t),
            PromptTarget::HfdlLocation => {
                let loc = if t.is_empty() {
                    None
                } else {
                    Some(crate::dsc::geo::parse_location(t).ok_or_else(|| {
                        format!("not a location: \"{t}\" (two signed decimal degrees, e.g. 54.687 25.280)")
                    })?)
                };
                self.set_location(loc);
            }
            PromptTarget::HfdlSave => {
                let path = std::path::PathBuf::from(crate::app::expand_home(t));
                svc.save_selected(&path)?;
                self.hfdl_notice(format!("saved to {}", path.display()));
            }
            PromptTarget::HfdlWatch => {
                prefs.watch = t
                    .split([',', ' '])
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                    .map(str::to_ascii_uppercase)
                    .collect();
                self.hfdl_set_prefs(prefs);
            }
            PromptTarget::HfdlKeywordsEmergency => {
                prefs.keywords.emergency = if t.is_empty() {
                    crate::hfdl::msg::Keywords::default().emergency
                } else {
                    list(t)
                };
                self.hfdl_set_prefs(prefs);
            }
            PromptTarget::HfdlKeywordsCaution => {
                prefs.keywords.caution = if t.is_empty() {
                    crate::hfdl::msg::Keywords::default().caution
                } else {
                    list(t)
                };
                self.hfdl_set_prefs(prefs);
            }
            PromptTarget::HfdlAircraftFile
            | PromptTarget::HfdlAirlinesFile
            | PromptTarget::HfdlAirportsFile => {
                if !t.is_empty() {
                    let p = std::path::PathBuf::from(crate::app::expand_home(t));
                    if !p.is_file() {
                        return Err(format!("no such file: {}", p.display()));
                    }
                }
                match target {
                    PromptTarget::HfdlAircraftFile => prefs.aircraft_file = t.into(),
                    PromptTarget::HfdlAirlinesFile => prefs.airlines_file = t.into(),
                    _ => prefs.airports_file = t.into(),
                }
                if !t.is_empty() {
                    self.hfdl_notice(format!("reading {t}..."));
                }
                self.hfdl_set_prefs(prefs);
            }
            _ => {}
        }
        Ok(())
    }

    /// The toggle state of a menu command (None: not a toggle).
    pub fn hfdl_toggle_state(&self, cmd: HfdlCmd) -> Option<bool> {
        let svc = self.hfdl.as_ref()?;
        let p = svc.prefs();
        match cmd {
            HfdlCmd::HideLink => Some(p.filter_link),
            HfdlCmd::HideJoined => Some(p.filter_joined),
            HfdlCmd::AlertsOnly => Some(svc.alerts_only()),
            HfdlCmd::Follow => Some(svc.follow()),
            HfdlCmd::Sound => Some(p.alert_sound),
            HfdlCmd::AlertCaution => Some(p.alert_on_caution),
            HfdlCmd::WatchAlerts => Some(p.watch_alerts),
            HfdlCmd::Notify => Some(p.notifications),
            HfdlCmd::LookupHexdb => Some(p.lookup_hexdb),
            HfdlCmd::LookupAdsbdb => Some(p.lookup_adsbdb),
            HfdlCmd::Messages => Some(svc.tab == Tab::Messages),
            HfdlCmd::Aircraft => Some(svc.tab == Tab::Aircraft),
            HfdlCmd::Stations => Some(svc.tab == Tab::Stations),
            _ => None,
        }
    }

    /// Keys for the open F9 menu. Returns whether the menu took the key.
    pub fn hfdl_menu_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::run::Action;
        let Some((top, item)) = self.hfdl_ui.menu else {
            return false;
        };
        let n = MENUS.len();
        let items = MENUS[top].1.len();
        match action {
            Action::Left => self.hfdl_ui.menu = Some(((top + n - 1) % n, 0)),
            Action::Right => self.hfdl_ui.menu = Some(((top + 1) % n, 0)),
            Action::Up => self.hfdl_ui.menu = Some((top, (item + items - 1) % items)),
            Action::Down => self.hfdl_ui.menu = Some((top, (item + 1) % items)),
            Action::Enter => self.hfdl_command(MENUS[top].1[item].cmd),
            Action::Escape | Action::FKey(9) => self.hfdl_ui.menu = None,
            Action::Quit => {
                self.hfdl_ui.menu = None;
                self.request_quit();
            }
            _ => {}
        }
        true
    }

    /// Keys for the focused HFDL view. Returns whether it took the key.
    pub fn hfdl_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::nav::NavKey;
        use crate::ui::run::Action;
        let Some(svc) = self.hfdl.as_mut() else {
            return false;
        };
        let ui = &mut self.hfdl_ui;
        let page = ui.list_rows.max(1) as isize;
        let dpage = ui.detail_rows.max(1);
        let tab = svc.tab;
        // the aircraft and ground-station lists
        let side_len = match tab {
            Tab::Aircraft => svc.aircraft_count(),
            Tab::Stations => crate::hfdl::stations::count(),
            Tab::Messages => 0,
        };
        let side = match tab {
            Tab::Aircraft => Some(&mut ui.ac_sel),
            Tab::Stations => Some(&mut ui.gs_sel),
            Tab::Messages => None,
        };
        let mv = |sel: &mut usize, d: isize| {
            *sel = (*sel as isize + d).clamp(0, side_len.saturating_sub(1) as isize) as usize;
        };
        match (ui.part, action) {
            (HfdlPart::List, Action::Nav(NavKey::Tab)) if ui.detail_open => {
                ui.part = HfdlPart::Detail
            }
            (HfdlPart::Detail, Action::Nav(NavKey::BackTab)) => ui.part = HfdlPart::List,
            (HfdlPart::Detail, Action::Nav(NavKey::Tab)) => {
                ui.part = HfdlPart::List;
                return false;
            }
            (_, Action::Nav(_)) => return false,
            (
                HfdlPart::List,
                Action::Up
                | Action::Down
                | Action::PageUp
                | Action::PageDown
                | Action::Home
                | Action::End,
            ) => {
                let d = match action {
                    Action::Up => -1,
                    Action::Down => 1,
                    Action::PageUp => -page,
                    Action::PageDown => page,
                    Action::Home => isize::MIN / 2,
                    _ => isize::MAX / 2,
                };
                match side {
                    Some(sel) => mv(sel, d),
                    None => match action {
                        Action::Home => svc.first(),
                        Action::End => svc.last(),
                        _ => svc.move_by(d),
                    },
                }
            }
            (HfdlPart::Detail, Action::Up) => ui.detail_scroll = ui.detail_scroll.saturating_sub(1),
            (HfdlPart::Detail, Action::Down) => ui.detail_scroll += 1,
            (HfdlPart::Detail, Action::PageUp) => {
                ui.detail_scroll = ui.detail_scroll.saturating_sub(dpage)
            }
            (HfdlPart::Detail, Action::PageDown) => ui.detail_scroll += dpage,
            (HfdlPart::Detail, Action::Home) => ui.detail_scroll = 0,
            (HfdlPart::Detail, Action::End) => ui.detail_scroll = usize::MAX / 2,
            (_, Action::Enter) => {
                let cmd = if tab == Tab::Aircraft {
                    HfdlCmd::AircraftMessages
                } else {
                    HfdlCmd::Detail
                };
                self.hfdl_command(cmd);
                return true;
            }
            (_, Action::Escape) => svc.clear_filters(),
            (_, Action::Find) => {
                self.hfdl_command(HfdlCmd::Search);
                return true;
            }
            (_, Action::ToggleDbAuto) => svc.acknowledge_alert(),
            (_, Action::Char(c)) => {
                let Some(cmd) = hfdl_letter(c) else {
                    return false;
                };
                self.hfdl_command(cmd);
                return true;
            }
            _ => return false,
        }
        true
    }

    /// Before drawing: keep the selected row inside the list window and the
    /// detail scroll in range.
    pub fn reconcile_hfdl(&mut self, rows: usize, detail_rows: usize, detail_lines: usize) {
        let ui = &mut self.hfdl_ui;
        ui.list_rows = rows;
        ui.detail_rows = detail_rows;
        ui.detail_scroll = ui
            .detail_scroll
            .min(detail_lines.saturating_sub(detail_rows));
        let Some(svc) = &self.hfdl else {
            return;
        };
        let (n, sel) = match svc.tab {
            Tab::Messages => {
                let n = svc.len();
                (n, svc.selected_row().unwrap_or(n.saturating_sub(1)))
            }
            Tab::Aircraft => {
                let n = svc.aircraft_count();
                ui.ac_sel = ui.ac_sel.min(n.saturating_sub(1));
                (n, ui.ac_sel)
            }
            Tab::Stations => {
                let n = crate::hfdl::stations::count();
                ui.gs_sel = ui.gs_sel.min(n.saturating_sub(1));
                (n, ui.gs_sel)
            }
        };
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

    /// A click on list row `row`.
    pub fn hfdl_click_row(&mut self, row: usize) {
        self.nav.focus = crate::ui::nav::Focus::WorkArea;
        self.hfdl_ui.part = HfdlPart::List;
        let Some(s) = self.hfdl.as_mut() else {
            return;
        };
        match s.tab {
            Tab::Messages => {
                let again = s.selected_row() == Some(row);
                s.select_row(row);
                if again {
                    self.hfdl_command(HfdlCmd::Detail);
                }
            }
            Tab::Aircraft => self.hfdl_ui.ac_sel = row,
            Tab::Stations => self.hfdl_ui.gs_sel = row,
        }
    }
}

impl AppState {
    /// Set the user's location (shared by DSC and HFDL; saved with the
    /// configuration).
    pub fn set_location(&mut self, loc: Option<(f64, f64)>) {
        self.location = loc;
        if let Some(h) = self.hfdl.as_mut() {
            h.set_location(loc);
        }
        if self.dsc_active()
            && let Some(d) = self.dsc.as_ref()
        {
            let mut p = d.prefs().clone();
            p.location = loc;
            self.dsc_set_prefs(p);
        }
    }
}

/// The bookmark list of the HFDL bands.
pub const BOOKMARK_LIST: &str = "HFDL";

/// The bookmark of an HFDL band: the VFO on its center, the span covering
/// its channels.
pub fn band_bookmark(
    name: &str,
    center_hz: u64,
    span_khz: u32,
) -> crate::bookmarks::model::Bookmark {
    let mut settings = crate::vfo::types::Settings::new();
    // a little room on both sides, in the schema's 10 kHz steps
    let span = (span_khz + 10).div_ceil(10) * 10;
    settings.insert(
        crate::vfo::types::hfdl::SPAN_KHZ.into(),
        serde_json::json!(f64::from(span.max(10))),
    );
    crate::bookmarks::model::Bookmark {
        name: format!("HFDL {name}"),
        frequency_hz: center_hz,
        vfo_type: HFDL.to_string(),
        settings,
        notes: "HFDL band center: every channel of the band in one VFO".into(),
        ..Default::default()
    }
}

impl AppState {
    /// Add the HFDL bands' bookmarks to the list `HFDL` (made if missing);
    /// those already there by name are kept. Returns how many were added.
    pub fn add_hfdl_bookmarks(&mut self) -> usize {
        let m = &mut self.bookmarks;
        let list = match m.store.list_index(BOOKMARK_LIST) {
            Some(i) => i,
            None => {
                if m.create_list(BOOKMARK_LIST).is_err() {
                    return 0;
                }
                m.store.lists.len() - 1
            }
        };
        let mut added = 0;
        for (name, hz, span) in crate::hfdl::stations::BANDS {
            let bm = band_bookmark(name, hz, span);
            if m.store.lists[list]
                .bookmarks
                .iter()
                .any(|b| b.name == bm.name)
            {
                continue;
            }
            if m.save_bookmark(None, list, bm).is_ok() {
                added += 1;
            }
        }
        added
    }
}

/// The About text (credits).
pub fn about_text() -> String {
    format!(
        "ShellSDR {} HFDL-ACARS decoder, built from the HFDL-ACARS seed. Method after dumphfdl and libacars by Tomasz Lemiech (GPL v3). GPL v3 or later.",
        crate::version::VERSION
    )
}

/// The view's own letter keys. Only letters ShellSDR itself leaves as plain
/// characters reach the view (`l`, `w`, `m` and others are global keys), so
/// each one here must translate to `Action::Char` (see the test).
const HFDL_LETTERS: [(char, HfdlCmd); 6] = [
    ('f', HfdlCmd::Follow),
    ('b', HfdlCmd::Sound),
    ('n', HfdlCmd::HideLink),
    ('j', HfdlCmd::HideJoined),
    ('e', HfdlCmd::AlertsOnly),
    ('v', HfdlCmd::NextView),
];

fn hfdl_letter(c: char) -> Option<HfdlCmd> {
    HFDL_LETTERS
        .iter()
        .find(|(k, _)| *k == c)
        .map(|(_, cmd)| *cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::run::Action;

    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn hfdl_state(name: &str) -> (AppState, Tmp) {
        let dir =
            std::env::temp_dir().join(format!("shellsdr-hfdlapp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = AppState::default();
        s.radio.set_type(HFDL);
        let mut st = s.radio.active().type_settings();
        st.insert(
            "message_dir".into(),
            dir.to_string_lossy().into_owned().into(),
        );
        st.insert("notifications".into(), false.into());
        s.radio.set_active_type_settings(&st);
        s.vfo_view_visible = true;
        s.now_secs = 1_795_600_689;
        s.sync_hfdl(1_795_600_689, false);
        (s, Tmp(dir))
    }

    /// The seed's 50 kHz I/Q excerpt through the VFO type's processor, the
    /// engine's data items and the app into the service: messages in view
    /// and in the day file, channel counters, aircraft.
    #[test]
    fn a_recording_decodes_end_to_end() {
        use crate::vfo::types::VfoType;
        let seed = std::env::var("HFDL_SEED")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../HFDL-ACARS-Decoder-Seed")
            });
        let wav = seed.join("conformance/iq/20261125T095809_iq50k_8961000Hz.wav");
        let Ok(b) = std::fs::read(&wav) else {
            eprintln!("seed not found: skipped");
            return;
        };
        // the excerpt: 16-bit stereo I/Q, data from the "data" chunk
        let p = b.windows(4).position(|w| w == b"data").unwrap();
        let x: Vec<num_complex::Complex32> = b[p + 8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                num_complex::Complex32::new(
                    i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0,
                    i16::from_le_bytes([c[2], c[3]]) as f32 / 32768.0,
                )
            })
            .collect();
        let (mut s, _t) = hfdl_state("e2e");
        s.radio.active_mut_freq(8_961_000);
        let mut st = s.radio.active().type_settings();
        st.insert("_vfo_hz".into(), 8_961_000.0.into());
        st.insert("_center_hz".into(), 8_961_000.0.into());
        let mut proc = crate::vfo::types::hfdl::HfdlType.build_processor(&st, 50_000.0, 0.0);
        let start = 1_795_600_689.0;
        let mut fed = 0usize;
        for blk in x.chunks(5000) {
            proc.process(blk);
            fed += blk.len();
            let now = start + fed as f64 / 50_000.0;
            s.hfdl_receive(proc.take_data(), now);
        }
        let svc = s.hfdl.as_mut().unwrap();
        assert!(svc.total() >= 15, "{} messages", svc.total());
        assert_eq!(
            svc.channels().keys().copied().collect::<Vec<_>>(),
            [8939, 8942, 8948, 8957, 8977]
        );
        let ok: u32 = svc.channels().values().map(|c| c.frames_ok).sum();
        assert!(ok >= 15, "{ok} frames");
        assert!(svc.aircraft_count() > 0);
        svc.flush();
        let lines = std::fs::read_to_string(svc.day_file()).unwrap();
        assert_eq!(lines.lines().count(), svc.total());
        // times follow the recording
        let first = svc.row(0).map(|e| e.msg.utc).unwrap_or(start);
        assert!((start..start + 120.0).contains(&first), "{first}");
    }

    #[test]
    fn hfdl_letters_are_not_global_keys() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for (c, _) in HFDL_LETTERS {
            let a =
                crate::ui::run::translate_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            assert!(
                matches!(a, Some(Action::Char(x)) if x == c),
                "'{c}' is taken: {a:?}"
            );
        }
        // every letter shown in the menu works
        for (_, items) in MENUS {
            for it in items {
                for k in it.key.split(' ').filter(|k| k.chars().count() == 1) {
                    let c = k.chars().next().unwrap();
                    // 'a' arrives as ToggleDbAuto and acknowledges on purpose
                    if c.is_ascii_lowercase() && c != 'a' {
                        assert_eq!(hfdl_letter(c), Some(it.cmd), "menu key '{c}'");
                    }
                }
            }
        }
    }

    #[test]
    fn band_bookmarks_are_added_once() {
        let (mut s, _t) = hfdl_state("bookmarks");
        assert_eq!(s.add_hfdl_bookmarks(), 13);
        assert_eq!(s.add_hfdl_bookmarks(), 0);
        let i = s.bookmarks.store.list_index(BOOKMARK_LIST).unwrap();
        let b = &s.bookmarks.store.lists[i].bookmarks;
        let b89 = b.iter().find(|b| b.name == "HFDL 8.9 MHz").unwrap();
        assert_eq!(b89.frequency_hz, 8_902_500);
        assert_eq!(b89.vfo_type, HFDL);
        assert_eq!(b89.settings["span_khz"], 170.0);
        // the span covers every channel of each band around its center
        for (_, hz, span) in crate::hfdl::stations::BANDS {
            let bm = band_bookmark("x", hz, span);
            let s = crate::vfo::types::hfdl::span_khz(&bm.settings);
            let ch = crate::vfo::types::hfdl::channels(hz as f64, s, hz as f64, 2_000_000.0);
            let all: Vec<u32> = crate::hfdl::stations::all_frequencies_khz()
                .into_iter()
                .filter(|&k| (k as f64 * 1000.0 - hz as f64).abs() < 400_000.0)
                .collect();
            for k in all {
                assert!(ch.contains(&k), "{hz}: {k} not in {ch:?}");
            }
        }
    }

    #[test]
    fn the_service_lives_while_the_vfo_is_hfdl() {
        let (mut s, _t) = hfdl_state("life");
        assert!(s.hfdl.is_some());
        s.radio.set_type(crate::vfo::types::VOICE);
        s.sync_hfdl(1_795_600_689, false);
        assert!(s.hfdl.is_none());
    }

    #[test]
    fn menus_toggle_and_persist_in_the_vfo_table() {
        let (mut s, _t) = hfdl_state("menu");
        assert_eq!(s.hfdl_toggle_state(HfdlCmd::HideLink), Some(true));
        s.hfdl_fkey(9);
        assert_eq!(s.hfdl_ui.menu, Some((0, 0)));
        s.hfdl_menu_key(Action::Right);
        s.hfdl_menu_key(Action::Down);
        s.hfdl_menu_key(Action::Enter);
        assert_eq!(s.hfdl_toggle_state(HfdlCmd::HideLink), Some(false));
        assert_eq!(s.radio.active().type_settings()["filter_link"], false);
        s.hfdl_fkey(3);
        assert_eq!(s.hfdl.as_ref().unwrap().tab, Tab::Aircraft);
    }

    #[test]
    fn prompts_validate_and_persist() {
        let (mut s, _t) = hfdl_state("prompts");
        assert!(
            s.hfdl_prompt_confirm(PromptTarget::HfdlLocation, "north")
                .is_err()
        );
        s.hfdl_prompt_confirm(PromptTarget::HfdlLocation, "54.687 25.28")
            .unwrap();
        assert_eq!(s.location, Some((54.687, 25.28)));
        s.sync_hfdl(1_795_600_689, false);
        assert_eq!(s.hfdl.as_ref().unwrap().location(), Some((54.687, 25.28)));
        s.hfdl_prompt_confirm(PromptTarget::HfdlWatch, "ra73824, 152060")
            .unwrap();
        assert_eq!(
            s.hfdl.as_ref().unwrap().prefs().watch,
            ["RA73824", "152060"]
        );
        s.hfdl_prompt_confirm(PromptTarget::HfdlKeywordsCaution, "icing,  low   fuel")
            .unwrap();
        assert_eq!(
            s.hfdl.as_ref().unwrap().prefs().keywords.caution,
            ["icing", "low fuel"]
        );
        s.hfdl_prompt_confirm(PromptTarget::HfdlKeywordsCaution, "")
            .unwrap();
        assert_eq!(s.hfdl.as_ref().unwrap().prefs().keywords.caution.len(), 14);
    }
}
