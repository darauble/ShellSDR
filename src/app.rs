//! Application state (UI-facing).
//!
//! Step 1 scope: the state the renderer and event loop need: navigation focus,
//! module collapse flags, and the global UI toggles (spectrum/waterfall on/off,
//! center/free). DSP, source, and VFO state arrive in later steps.

use crate::bookmarks::editor::{BookmarkEditor, EditorOutcome};
use crate::bookmarks::manager::{BookmarkManager, Request};
use crate::bookmarks::model::{Bookmark, StoreFile, UtcNow};
use crate::freq::FreqEntry;
use crate::source::model::SourceModule;
use crate::ui::global::GlobalField;
use crate::ui::nav::{NavKey, NavState, handle_nav};
use crate::ui::popup::ListPopup;
use crate::ui::prompt::TextPrompt;
use crate::ui::theme::{Theme, ThemeRegistry};
use crate::vfo::radio::RadioModule;

/// Tuning behavior for spectrum clicks and out-of-bounds frequency entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TuneMode {
    /// Clicking the spectrum re-centers the source.
    Center,
    /// Clicking the spectrum moves only the active VFO.
    Free,
}

/// Whether the stream is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Stopped,
    Running,
}

/// Default Master VFO frequency at first start (100 MHz).
pub const DEFAULT_FREQ_HZ: u64 = 100_000_000;

/// Step a wrapping list index by `dir` (+1/-1) within `len` items.
fn step_index(idx: usize, dir: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let n = len as i32;
    (((idx as i32 + dir) % n + n) % n) as usize
}

/// What an open selection popup is choosing for. Drives how a confirmed index
/// is applied back to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupTarget {
    /// The active VFO's type (index into `RadioModule::type_options`).
    VfoType,
    /// A choice setting of the active VFO's type (the row's schema index;
    /// the item index is the option index).
    VfoSetting(u8),
    /// The active VFO's audio output device (0 = default, else named device).
    VfoAudio,
    /// The source device (index into `source.devices`).
    SourceDevice,
    /// The Network source's audio bit rate (index into `NET_BITRATES`).
    SourceNetAudio,
    /// The source sample rate (index into the view's sample-rate list).
    SourceSampleRate,
    /// The source bandwidth (index into the view's bandwidth list).
    SourceBandwidth,
    /// The source antenna (index into the view's antenna list).
    SourceAntenna,
    /// The Source decimation ratio (index into `power_decimator::RATIOS`).
    SourceDecimation,
    /// The selected bookmark list (index into the store's lists).
    BookmarkList,
    /// Confirm deleting the selected bookmark list (item 1 = delete).
    ConfirmDeleteList,
    /// Confirm removing bookmark `idx` of the selected list (item 1 = remove).
    ConfirmRemoveBookmark(usize),
    /// The list to copy / move bookmark `idx` of the selected list to.
    CopyBookmark(usize),
    MoveBookmark(usize),
    /// The schedule list to show; the list to move job `idx` to.
    ScheduleList,
    MoveJob(usize),
    /// Confirm deleting the schedule list shown; activating it while a job
    /// records (item 1 = yes).
    ConfirmDeleteSchedList,
    ConfirmActivate,
    /// Cancel / Move / Start fresh for a new report folder.
    ReportRelocate,
    /// The schedule editor's bookmark (index into `AppState::sched_pick`).
    ScheduleBookmark,
    /// Confirm removing scheduled job `idx` (item 1 = remove).
    ConfirmRemoveJob(usize),
    /// Quitting was refused because the source runs (only [OK]).
    QuitBlocked,
    /// A message with only [OK].
    Notice,
}

/// Expand a leading `~` / `~/` to the home directory.
pub fn expand_home(path: &str) -> String {
    let rest = match path.strip_prefix('~') {
        Some(r) if r.is_empty() || r.starts_with('/') => r,
        _ => return path.to_string(),
    };
    match directories::UserDirs::new() {
        Some(u) => format!("{}{rest}", u.home_dir().to_string_lossy()),
        None => path.to_string(),
    }
}

/// What an open text prompt names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTarget {
    NewList,
    RenameList,
    /// A new schedule list's name / the shown schedule list's new name.
    NewSchedList,
    RenameSchedList,
    /// The Report's folder; one of its retention numbers (0..3); the hook.
    ReportDir,
    ReportKeep(u8),
    ReportHook,
    /// The active VFO's recording file-name template.
    RecTemplate,
    /// The recordings folder.
    RecDir,
    /// The baseband file to play (File source).
    FilePath,
    /// The Network source's server address and its token.
    NetServer,
    NetToken,
    /// Sharing: the address to listen on and the token (0.4.1).
    ShareListen,
    ShareToken,
    /// A folder setting of the active VFO's type (its schema row).
    VfoSetting(u8),
    /// DSC view: the search query, the user's location, the aprs.fi key,
    /// the lookup endpoint, and where to save the shown message.
    DscSearch,
    DscLocation,
    DscApiKey,
    DscEndpoint,
    DscSave,
    /// HFDL view (0.4.5): search, location, save, watch list, keywords.
    HfdlSearch,
    HfdlLocation,
    HfdlSave,
    HfdlWatch,
    HfdlKeywordsEmergency,
    HfdlKeywordsCaution,
    HfdlAircraftFile,
    HfdlAirlinesFile,
    HfdlAirportsFile,
}

/// What the Radio module shows for a running recording.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RecView {
    /// File name being written (no folder).
    pub file: String,
    /// Recorded time in seconds.
    pub seconds: f64,
    /// Bytes of sample data written.
    pub bytes: u64,
    /// Sample frames dropped because the disk fell behind.
    pub dropped: u64,
}

/// Items of a delete confirmation popup (Cancel first, so a stray ENTER is
/// harmless).
fn confirm_items(action: &str) -> Vec<String> {
    vec!["Cancel".to_string(), action.to_string()]
}

/// Top-level UI state.
#[derive(Debug)]
pub struct AppState {
    pub nav: NavState,
    pub source: SourceModule,
    pub radio: RadioModule,
    /// Source center frequency editor (top panel).
    pub center_freq: FreqEntry,
    /// Focused control in the Global Controls bar.
    pub global_field: GlobalField,
    /// Whether the focused global control is being edited.
    pub global_editing: bool,
    pub themes: ThemeRegistry,
    pub spectrum_on: bool,
    pub waterfall_on: bool,
    pub tune_mode: TuneMode,
    pub run: RunState,
    /// Bookmarks: the shared store plus the left-panel module state.
    pub bookmarks: BookmarkManager,
    /// The shared bookmarks file (None in tests: nothing is written).
    pub bookmark_file: Option<StoreFile>,
    /// Current UTC time (refreshed by the event loop) for on-air coloring.
    pub utc: UtcNow,
    /// Local wall-clock time (HHMM) shown next to UTC; None if unknown.
    pub local_hhmm: Option<u16>,
    /// An open text prompt (list name) and what it names.
    pub prompt: Option<TextPrompt>,
    pub prompt_target: Option<PromptTarget>,
    /// An open bookmark editor form.
    pub bm_editor: Option<BookmarkEditor>,
    /// The open file-open dialog (File source).
    pub file_browser: Option<crate::ui::file_browser::FileBrowser>,
    /// Master output volume (0.0..=1.0).
    pub volume: f32,
    /// Global mute: the engine gets 0 while set; `volume` is kept.
    pub muted: bool,
    /// The open help popup (`?` / `[?] Help`), if any.
    pub help: Option<crate::ui::help::HelpView>,
    /// Requested left panel width (persisted; the layout clamps it to the
    /// screen via `ui::layout::left_panel_width`).
    pub left_panel_width: u16,
    /// Width of the last drawn screen (0 = not drawn yet), so a resize step
    /// starts from the width actually shown.
    pub screen_width: u16,
    /// Scroll offset of the left panel (virtual rows hidden above the top).
    pub left_scroll: u16,
    /// Last rendered height of the left-panel viewport, for paging math.
    pub left_viewport: u16,
    /// An open modal selection popup and what it targets, if any.
    pub popup: Option<ListPopup>,
    /// What the open popup selects (meaningful only while `popup` is Some).
    pub popup_target: Option<PopupTarget>,
    /// Latest power-spectrum frame (DC-centered dB) from the engine, for the
    /// spectrum/waterfall display. Empty when stopped or in low-fi.
    pub spectrum: Vec<f32>,
    /// The spectrum's visible span in Hz (the source sample rate).
    pub spectrum_span_hz: f64,
    /// dB display window for the spectrum/waterfall (auto-range + manual).
    pub db_range: crate::ui::spectrum::DbRange,
    /// Rolling history of recent frames for the waterfall (newest first).
    pub waterfall: crate::ui::waterfall::WaterfallHistory,
    /// Persisted per-device Source settings (keyed by device args), loaded
    /// from config and updated with the current device on save.
    pub source_devices: std::collections::BTreeMap<String, crate::config::DeviceSettings>,
    /// Engine health for the status bar: driver overflows since Start (lost
    /// IQ blocks) and DSP load (fraction of real time).
    pub dsp_overflows: u64,
    pub dsp_load: f32,
    /// Folder recordings are written to, for VFO types without their own.
    pub rec_dir: String,
    /// Recordings folders of single VFO types, by type id.
    pub rec_dirs: std::collections::BTreeMap<String, String>,
    /// The running recording, refreshed by the event loop (None = idle).
    pub rec_view: Option<RecView>,
    /// Why the last recording could not start or stopped with an error.
    pub rec_error: Option<String>,
    /// This session's finished recordings, newest last (the Voice view's
    /// recordings list).
    pub rec_history: Vec<RecView>,
    /// The Schedule module (jobs, scheduler runtime, view).
    pub schedule: crate::schedule::module::ScheduleModule,
    /// The per-instance schedule file (None in tests: nothing is written).
    pub schedule_path: Option<std::path::PathBuf>,
    /// An open schedule job editor.
    pub sched_editor: Option<crate::schedule::editor::ScheduleEditor>,
    /// The bookmarks listed in the schedule's bookmark picker, as (list,
    /// index), in popup order.
    pub sched_pick: Vec<(usize, usize)>,
    /// Wall clock (Unix seconds) and the local clock's UTC offset (minutes),
    /// refreshed by the event loop.
    pub now_secs: u64,
    pub utc_offset_min: i32,
    /// The recording choice a scheduled job replaced (restored when it
    /// ends).
    pub sched_prev_rec: Option<crate::record::RecChoice>,
    /// The VFO type a job switched away from (restored when no job runs).
    pub sched_prev_type: Option<String>,
    /// The VFO mute a muting job replaced (restored when it ends).
    pub sched_prev_muted: Option<bool>,
    /// The Work Area layout and tab (spec 13.5).
    pub work_area: crate::config::WorkAreaConfig,
    /// Whether the spectrum stack is on screen (not hidden behind the VFO
    /// tab), from the last frame: a hidden spectrum costs no FFT.
    pub spectrum_visible: bool,
    /// Whether the VFO view is on screen (its spectra run only then).
    pub vfo_view_visible: bool,
    /// Whether the Work Area's page slot (the VFO view's place) is on
    /// screen, whoever owns it (spec 19).
    pub work_slot_visible: bool,
    /// Who the page slot belongs to: the VFO, or a module's page.
    pub work_owner: crate::ui::page::WorkOwner,
    /// The Report's settings (persisted) and the open Report (spec 20;
    /// None until `open_report`, so tests write no files).
    pub report_config: crate::report::ReportConfig,
    pub report: Option<crate::report::Report>,
    /// What the report saw at the last tick (to log the changes).
    pub report_seen: crate::report_app::Seen,
    /// The Report page's tab, period and filter.
    pub report_ui: crate::report_app::ReportUi,
    /// The Voice view's data: the latest frame, the zoomed channel's span
    /// (Hz, centered on the channel), its spectrum (cropped) and waterfall,
    /// and the audio waterfall (0..`AUDIO_VIEW_HZ`).
    pub view: Option<crate::engine::VfoViewFrame>,
    pub zoom_span_hz: f64,
    pub zoom_db: Vec<f32>,
    pub zoom_history: crate::ui::waterfall::WaterfallHistory,
    pub zoom_range: crate::ui::spectrum::DbRange,
    pub audio_history: crate::ui::waterfall::WaterfallHistory,
    pub audio_range: crate::ui::spectrum::DbRange,
    /// The schedule file needs a newer ShellSDR: it is never saved.
    pub schedule_read_only: bool,
    /// A persistent warning for the status bar (a file that needs a newer
    /// ShellSDR and is therefore read-only).
    pub file_warning: Option<String>,
    /// The DSC VFO's message service (while the active VFO is DSC) and its
    /// view's state (spec 14).
    pub dsc: Option<crate::dsc::service::DscService>,
    pub dsc_ui: crate::dsc_app::DscUi,
    /// The HFDL VFO's message service and its view's state (0.4.5).
    pub hfdl: Option<crate::hfdl::service::HfdlService>,
    pub hfdl_ui: crate::hfdl_app::HfdlUi,
    /// The user's location (lat, lon), shared by the decoders (0.4.5).
    pub location: Option<(f64, f64)>,
    /// The fax receiver of a FAX VFO (0.2.2) and its view's state.
    pub fax: Option<crate::fax::service::FaxService>,
    pub fax_ui: crate::fax_app::FaxUi,
    /// The receiver of a Digital VFO (0.2.5) and its view's state.
    pub digital: Option<crate::digital::service::DigitalService>,
    pub digital_ui: crate::digital_app::DigitalUi,
    /// What a viewer's screen shows of the spectrum and the VFO view
    /// (0.4.1): the engine runs them for either screen.
    pub remote_shows: (bool, bool),
    /// The screen being drawn is painted by a graphical front (0.4.8, spec
    /// 28): the views record their pictures' pixels for it.
    pub pixel_pictures: bool,
    /// Pictures made lately, newest last (a picture is made once, not on
    /// every frame).
    pub picture_cache: std::cell::RefCell<Vec<std::sync::Arc<crate::remote::picture::Picture>>>,
    /// Set false to exit the event loop.
    pub running: bool,
}

impl AppState {
    /// The picture of `slot` named `key`: made by `make` unless it was made
    /// lately.
    pub fn cached_picture(
        &self,
        slot: u8,
        key: u64,
        make: impl FnOnce() -> Option<crate::remote::picture::Picture>,
    ) -> Option<std::sync::Arc<crate::remote::picture::Picture>> {
        let mut c = self.picture_cache.borrow_mut();
        if let Some(p) = c.iter().find(|p| p.slot == slot && p.key == key) {
            return Some(std::sync::Arc::clone(p));
        }
        let p = std::sync::Arc::new(make()?);
        if !p.valid() {
            return None;
        }
        c.push(std::sync::Arc::clone(&p));
        // two of a slot (two screens of other sizes)
        let mut seen = 0;
        for i in (0..c.len()).rev() {
            if c[i].slot == slot {
                seen += 1;
                if seen > 2 {
                    c.remove(i);
                }
            }
        }
        Some(p)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            nav: NavState::default(),
            source: SourceModule::default(),
            radio: RadioModule::new(DEFAULT_FREQ_HZ),
            center_freq: FreqEntry::new(DEFAULT_FREQ_HZ),
            global_field: GlobalField::default(),
            global_editing: false,
            themes: ThemeRegistry::default(),
            spectrum_on: true,
            waterfall_on: true,
            tune_mode: TuneMode::Center,
            run: RunState::Stopped,
            bookmarks: BookmarkManager::default(),
            bookmark_file: None,
            utc: UtcNow::default(),
            local_hhmm: None,
            prompt: None,
            prompt_target: None,
            bm_editor: None,
            file_browser: None,
            volume: 1.0,
            muted: false,
            help: None,
            left_panel_width: crate::ui::layout::LEFT_PANEL_MIN_WIDTH,
            screen_width: 0,
            left_scroll: 0,
            left_viewport: 0,
            popup: None,
            popup_target: None,
            spectrum: Vec::new(),
            spectrum_span_hz: 2_000_000.0,
            db_range: crate::ui::spectrum::DbRange::default(),
            waterfall: crate::ui::waterfall::WaterfallHistory::new(512),
            source_devices: std::collections::BTreeMap::new(),
            dsp_overflows: 0,
            dsp_load: 0.0,
            rec_dir: crate::record::default_dir(),
            rec_dirs: Default::default(),
            rec_view: None,
            rec_error: None,
            rec_history: Vec::new(),
            schedule: crate::schedule::module::ScheduleModule::default(),
            schedule_path: None,
            sched_editor: None,
            sched_pick: Vec::new(),
            now_secs: 0,
            utc_offset_min: 0,
            sched_prev_rec: None,
            sched_prev_type: None,
            sched_prev_muted: None,
            work_area: Default::default(),
            spectrum_visible: true,
            vfo_view_visible: false,
            work_slot_visible: false,
            work_owner: Default::default(),
            report_config: Default::default(),
            report: None,
            report_seen: Default::default(),
            report_ui: Default::default(),
            view: None,
            zoom_span_hz: 0.0,
            zoom_db: Vec::new(),
            zoom_history: crate::ui::waterfall::WaterfallHistory::new(256),
            zoom_range: crate::ui::spectrum::DbRange::default(),
            audio_history: crate::ui::waterfall::WaterfallHistory::new(256),
            audio_range: crate::ui::spectrum::DbRange::default(),
            schedule_read_only: false,
            file_warning: None,
            dsc: None,
            dsc_ui: Default::default(),
            hfdl: None,
            hfdl_ui: Default::default(),
            location: None,
            fax: None,
            fax_ui: Default::default(),
            digital: None,
            digital_ui: Default::default(),
            remote_shows: (false, false),
            pixel_pictures: false,
            picture_cache: Default::default(),
            running: true,
        }
    }
}

impl AppState {
    /// Build the initial state, populating the SoapySDR view from the mock
    /// device's capabilities (step 2). Step 3 replaces this with the opened
    /// real/selected device's capabilities.
    pub fn with_mock_source() -> Self {
        use crate::source::{MockSource, SdrSource};
        let mut s = Self::default();
        if let Ok(src) = MockSource::open("driver=mock,mode=ST") {
            s.source.set_soapy_caps(src.capabilities());
        }
        s
    }

    /// Replace the theme registry (e.g. with disk-discovered themes).
    pub fn set_theme_registry(&mut self, registry: ThemeRegistry) {
        self.themes = registry;
    }

    /// Set the available audio output options for the per-VFO picker.
    pub fn set_audio_options(&mut self, options: Vec<crate::engine::audio::OutputOption>) {
        self.radio.set_audio_options(options);
    }

    /// Select the theme with the given name, if present. Used at startup to
    /// restore the last-used theme from config.
    pub fn select_theme(&mut self, name: &str) -> bool {
        self.themes.select_by_name(name)
    }

    /// The name of the currently selected theme (for persistence).
    pub fn current_theme_name(&self) -> String {
        self.themes.current().name.clone()
    }

    /// Build initial state with REAL SoapySDR enumeration + the first device's
    /// capabilities. Falls back to mock caps if nothing is found.
    pub fn with_real_source() -> Self {
        use crate::source::SdrSource;
        let mut s = Self::default();
        // Silence any direct-fd prints from the driver during enumeration and
        // the probe-open (sdrplay emits `[INFO] devIdx: ...`). The no-op `log`
        // handler catches `SoapySDR_logf` output; this covers the rest.
        let _hush = crate::engine::silence::StdioSilencer::new();
        // Bounded: a hung driver module must not freeze startup. On timeout we
        // also skip the probe-open, since SoapySDR may still hold its factory
        // lock in the stuck enumeration thread.
        let enumerated =
            crate::source::soapy::enumerate_with_timeout(std::time::Duration::from_secs(5));
        let enum_ok = enumerated.is_some();
        s.source.set_devices(enumerated.unwrap_or_default());
        if enum_ok
            && let Some(args) = s.source.selected_device_args()
            && let Ok(src) = crate::source::soapy::SoapySource::open(&args)
        {
            s.source.set_soapy_caps(src.capabilities());
        }
        if s.source.soapy.is_none() {
            // No device: fall back to mock capabilities so the UI is usable.
            if let Ok(src) = crate::source::MockSource::open("driver=mock,mode=ST") {
                s.source.set_soapy_caps(src.capabilities());
            }
        }
        s
    }

    /// Apply a navigation key to the nav sub-state.
    pub fn on_nav(&mut self, key: NavKey) {
        self.nav = handle_nav(self.nav, key);
    }

    pub fn toggle_spectrum(&mut self) {
        self.spectrum_on = !self.spectrum_on;
    }

    pub fn toggle_waterfall(&mut self) {
        self.waterfall_on = !self.waterfall_on;
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
    }

    /// Make the left panel one step wider / narrower (`Panel:` stepper,
    /// Ctrl+Shift+Left/Right). The step starts from the width actually shown,
    /// so on a narrow screen the first step is visible at once and widening
    /// stops where the right side would get too narrow.
    pub fn resize_left_panel(&mut self, wider: bool) {
        use crate::ui::layout::{
            LEFT_PANEL_MAX_WIDTH, LEFT_PANEL_MIN_WIDTH, LEFT_PANEL_STEP, left_panel_width,
        };
        let screen = if self.screen_width == 0 {
            u16::MAX
        } else {
            self.screen_width
        };
        let shown = left_panel_width(self.left_panel_width, screen);
        let next = if wider {
            shown.saturating_add(LEFT_PANEL_STEP)
        } else {
            shown.saturating_sub(LEFT_PANEL_STEP)
        };
        self.left_panel_width =
            left_panel_width(next, screen).clamp(LEFT_PANEL_MIN_WIDTH, LEFT_PANEL_MAX_WIDTH);
    }

    /// The master gain the engine applies: 0 while muted globally or while
    /// the VFO being heard is muted. Recording taps the audio before this
    /// gain, so a muted VFO still records.
    // NOTE(step 6): with one audio chain the active VFO's mute zeroes the
    // master gain; per-VFO chains will carry their own gain.
    pub fn effective_volume(&self) -> f32 {
        if self.muted || self.radio.active().muted || self.muted_for_viewer() {
            0.0
        } else {
            self.volume
        }
    }

    /// Open / close the help popup.
    pub fn toggle_help(&mut self) {
        self.help = match self.help {
            Some(_) => None,
            None => Some(crate::ui::help::HelpView::default()),
        };
    }

    pub fn toggle_tune_mode(&mut self) {
        self.tune_mode = match self.tune_mode {
            TuneMode::Center => TuneMode::Free,
            TuneMode::Free => TuneMode::Center,
        };
    }

    pub fn toggle_run(&mut self) {
        // a sharing ShellSDR has no stream to start: Start opens its window
        if self.run == RunState::Stopped
            && self.source.kind == crate::source::model::SourceKind::Network
            && self.source.network.window
        {
            self.source.network.open_request = true;
            return;
        }
        self.run = match self.run {
            RunState::Stopped => RunState::Running,
            RunState::Running => {
                // Stopping the source ends the recording; it does not resume
                // by itself on the next Start.
                self.radio.stop_recording();
                // [Max] is for one playback: Start plays in real time again.
                self.source.file.fast = false;
                self.source.file.speed = None;
                RunState::Stopped
            }
        };
    }

    /// `q`: quit, unless reception or file playback is running. Then a
    /// dialog says to stop it first (closing the help popup, which would
    /// cover it); nothing is stopped for the user.
    pub fn request_quit(&mut self) {
        if self.run == RunState::Stopped {
            self.running = false;
            return;
        }
        let what = match self.source.kind {
            crate::source::model::SourceKind::File => "file playback",
            crate::source::model::SourceKind::SoapySdr
            | crate::source::model::SourceKind::Network => "reception",
        };
        self.help = None;
        self.popup = Some(ListPopup::new(
            format!("Stop {what} before quitting"),
            vec!["OK".to_string()],
            0,
        ));
        self.popup_target = Some(PopupTarget::QuitBlocked);
    }

    /// [Record] / [Stop]: switch the active VFO's recording. It writes while
    /// the source runs; pressed while stopped it waits for Start.
    pub fn toggle_recording(&mut self) {
        self.rec_error = None;
        self.radio.toggle_recording();
    }

    /// ENTER or a click on a toggle/recorder control: the VFO's Mute, the
    /// recorder type, the name/folder prompt, or [Record]/[Stop]. Returns
    /// whether `f` is such a control (other fields keep their own handling).
    pub fn activate_recorder_field(&mut self, f: crate::vfo::radio::RadioField) -> bool {
        use crate::vfo::radio::RadioField;
        match f {
            RadioField::Mute => self.radio.toggle_mute(),
            f if crate::vfo::radio::RadioModule::is_rec_check(f) => self.radio.toggle_rec(f),
            RadioField::RecName => self.open_rec_prompt(PromptTarget::RecTemplate),
            RadioField::RecDir => self.open_rec_prompt(PromptTarget::RecDir),
            RadioField::Record => self.toggle_recording(),
            RadioField::Setting(i) if self.radio.is_path(f) => {
                let Some((field, v)) = self.radio.setting_value(i as usize) else {
                    return false;
                };
                let text = v.as_str().unwrap_or("").to_string();
                self.prompt = Some(TextPrompt::new(
                    format!("{} folder (empty = default)", field.label),
                    text,
                ));
                self.prompt_target = Some(PromptTarget::VfoSetting(i));
            }
            _ => return false,
        }
        true
    }

    /// Show `msg` in a popup with only [OK].
    pub fn show_notice(&mut self, msg: String) {
        self.popup = Some(ListPopup::new(msg, vec!["OK".to_string()], 0));
        self.popup_target = Some(PopupTarget::Notice);
    }

    /// Remember a finished recording output for the session's list (the
    /// last 50 are kept).
    pub fn push_finished_recording(&mut self, entry: RecView) {
        self.report_recording(&entry.file, entry.seconds, entry.bytes, entry.dropped);
        self.rec_history.push(entry);
        let n = self.rec_history.len();
        if n > 50 {
            self.rec_history.drain(..n - 50);
        }
    }

    /// The recordings folder of VFO type `vfo_type`: its own, or the common
    /// one.
    pub fn rec_dir_of(&self, vfo_type: &str) -> &str {
        self.rec_dirs.get(vfo_type).unwrap_or(&self.rec_dir)
    }

    /// The recordings folder of the active VFO's type (the `Dir:` row).
    pub fn active_rec_dir(&self) -> &str {
        self.rec_dir_of(&self.radio.active().vfo_type)
    }

    /// ENTER on a recorder text field opens a prompt with its value.
    pub fn open_rec_prompt(&mut self, target: PromptTarget) {
        let (title, value) = match target {
            PromptTarget::RecTemplate => (
                "File name ($t $f $r $n $h $m $s $d $M $y)".to_string(),
                self.radio.active().rec_template.clone(),
            ),
            PromptTarget::RecDir => (
                format!("Recordings folder of {}", self.radio.active().vfo_type),
                self.active_rec_dir().to_string(),
            ),
            _ => return,
        };
        self.prompt = Some(TextPrompt::new(title, value));
        self.prompt_target = Some(target);
    }

    /// Decide the file names, formats and metadata of a recording starting
    /// at `now` for the active VFO: one plan per checked output (native,
    /// baseband), sharing the timestamp (pure; the event loop opens the
    /// files). Errors when nothing can be recorded.
    pub fn recording_plans(
        &self,
        now: crate::record::naming::UtcDateTime,
    ) -> Result<Vec<crate::record::RecPlan>, String> {
        use crate::record::naming::{NameFields, expand};
        use crate::record::{FORMAT_VERSION, Minimize, RecKind, RecPlan, minimized_rate};
        use crate::vfo::types::NativeOutput;
        let v = self.radio.active();
        let Some(t) = crate::vfo::types::usable(&v.vfo_type) else {
            return Err(format!(
                "type {} is not available in this build",
                v.vfo_type
            ));
        };
        let mut kinds = v.rec.kinds();
        if t.native_always_on() {
            // The native output (DSC messages) is recorded all the time;
            // [Record] records the baseband only (spec 14.4).
            kinds.retain(|k| *k != RecKind::Native);
            if kinds.is_empty() {
                return Err("Check Baseband to record".to_string());
            }
        }
        if kinds.is_empty() {
            return Err("choose Native or Baseband to record".to_string());
        }
        let settings = v.type_settings();
        let center = self.center_hz() as f64;
        let effective = self.source.effective_sample_rate().unwrap_or(0.0);
        let mode = t.mode_label(&settings);
        let mut plans = Vec::new();
        for kind in kinds {
            let minimize = (kind == RecKind::Baseband && v.rec.minimize).then(|| Minimize {
                freq_hz: v.frequency_hz as f64,
                min_rate: t.min_baseband_rate(&settings),
            });
            let (freq, channels, sample_rate, token) = match (kind, minimize) {
                // The fax service writes the pictures itself (PNG, spec 15.4).
                (RecKind::Native, _) if t.id() == crate::vfo::types::FAX => continue,
                // The Digital service writes its Markdown itself (spec 16.7).
                (RecKind::Native, _) if t.id() == crate::vfo::types::DIGITAL => continue,
                (RecKind::Native, _) => {
                    if t.native_output() != NativeOutput::Audio {
                        return Err(format!(
                            "native recording of {} is not supported yet",
                            v.vfo_type
                        ));
                    }
                    (
                        v.frequency_hz as f64,
                        1,
                        crate::engine::AUDIO_RATE,
                        t.native_token(),
                    )
                }
                (RecKind::Baseband, Some(m)) => (
                    m.freq_hz,
                    2,
                    minimized_rate(effective, m.min_rate).round() as u32,
                    "baseband",
                ),
                (RecKind::Baseband, None) => (center, 2, effective.round() as u32, "baseband"),
            };
            let name = expand(
                &v.rec_template,
                &NameFields {
                    kind,
                    native_token: t.native_token(),
                    freq_hz: freq,
                    time: now,
                    mode: &mode,
                    vfo_name: &v.name,
                },
            );
            let kv = |k: &str, val: String| (k.to_string(), val);
            let info = vec![
                kv("format_version", FORMAT_VERSION.to_string()),
                kv("rec_type", token.to_string()),
                kv("vfo_type", v.vfo_type.clone()),
                kv(
                    "vfo_settings",
                    serde_json::Value::Object(settings.clone()).to_string(),
                ),
                kv("minimized", u8::from(minimize.is_some()).to_string()),
                kv(
                    "center_freq",
                    format!(
                        "{:.0}",
                        if kind == RecKind::Baseband {
                            freq
                        } else {
                            center
                        }
                    ),
                ),
                kv("vfo_freq", v.frequency_hz.to_string()),
                kv("vfo_name", v.name.clone()),
                kv("mode", mode.clone()),
                kv("bandwidth", format!("{:.0}", v.bandwidth())),
                kv(
                    "source_sample_rate",
                    format!("{:.0}", self.source.selected_sample_rate().unwrap_or(0.0)),
                ),
                kv("decimation", self.source.decimation().to_string()),
                kv("start_time", now.iso()),
                kv("timezone", "UTC".to_string()),
            ];
            plans.push(RecPlan {
                kind,
                minimize,
                name,
                channels,
                sample_rate,
                info,
            });
        }
        Ok(plans)
    }

    /// Toggle the source kind (SoapySDR <-> File). Entering the File view
    /// with a loaded file tunes the center to it.
    pub fn toggle_source_kind(&mut self) {
        self.step_source_kind(1);
    }

    /// Step the source kind by `dir` (Left/Right on the Kind row).
    pub fn step_source_kind(&mut self, dir: i32) {
        self.source.set_kind(self.source.kind.stepped(dir));
        self.entered_source_kind();
        self.apply_file_center();
    }

    /// The range the source CENTER may take: locked to the file's center
    /// while a file with a known center is the source (SDR++ locks the
    /// waterfall center for its file source), else the device band.
    pub fn center_band(&self) -> (u64, u64) {
        match self.source.file_center() {
            Some(c) => (c, c),
            None => self.freq_band(),
        }
    }

    /// Tune the center (and the VFOs with it, keeping their offsets) to the
    /// loaded file's center frequency, when the File source is selected.
    pub(crate) fn apply_file_center(&mut self) {
        if let Some(c) = self.source.file_center() {
            if self.global_editing {
                self.global_editing = false;
                self.center_freq.cancel();
            }
            let old = self.center_hz();
            if c != old {
                self.center_freq = FreqEntry::new(c);
                self.radio.reproject_to_center(old, c);
            }
        }
    }

    /// Load the baseband WAV at `path` as the File source (spec 7.2): read
    /// its rate, length and center frequency, rewind, and tune the center to
    /// it. On failure the previous file stays loaded and the error is shown.
    pub fn load_file(&mut self, path: &str) -> Result<(), String> {
        let path = path.trim();
        if path.is_empty() {
            return Err("File name must not be empty".to_string());
        }
        let full = expand_home(path);
        match crate::source::file::probe(std::path::Path::new(&full)) {
            Ok(meta) => {
                let f = &mut self.source.file;
                f.path = Some(full);
                f.meta = Some(meta);
                f.error = None;
                f.position = 0;
                f.pending_seek = None;
                let tuning = f.meta.as_ref().and_then(|m| m.tuning.clone());
                self.apply_file_center();
                // A minimized baseband is only the VFO's channel: put the VFO
                // back on it, as recorded (spec 13.6).
                if let Some(t) = tuning
                    && self.radio.apply_type_settings(&t.vfo_type, &t.settings)
                {
                    let (lo, hi) = self.freq_band();
                    self.radio.active_mut_freq(t.vfo_hz.clamp(lo, hi));
                }
                Ok(())
            }
            Err(e) => {
                self.source.file.error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// ENTER or a click on a File transport button.
    pub fn press_file_button(&mut self, b: crate::source::model::FileButton) {
        use crate::source::model::{FileButton, SKIP_SECONDS};
        match b {
            FileButton::StartStop => self.toggle_run(),
            FileButton::Max => {
                let f = &mut self.source.file;
                if self.run == RunState::Stopped {
                    f.fast = true;
                    self.toggle_run();
                } else {
                    f.fast = !f.fast;
                }
                self.source.file.speed = None;
            }
            FileButton::Rewind => self.source.file.seek_to(0),
            FileButton::Back(i) => self.source.file.skip(-(SKIP_SECONDS[i] as i64)),
            FileButton::Fwd(i) => self.source.file.skip(SKIP_SECONDS[i] as i64),
        }
    }

    /// The source could not start or stopped working: the File view shows
    /// why.
    pub fn source_failed(&mut self, msg: String) {
        if self.source.kind == crate::source::model::SourceKind::File {
            self.source.file.error = Some(msg);
        }
    }

    /// The file played to its end: stop (spec 7.2 default) and rewind, so
    /// the next Start plays it again (SDR++ rewinds on stop).
    pub fn file_finished(&mut self) {
        let name = self
            .source
            .file
            .path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        self.report_event(
            crate::report::model::Level::Info,
            crate::report::model::Category::Source,
            format!("File played to the end: {name}"),
            None,
        );
        if self.run == RunState::Running {
            self.toggle_run();
        }
        self.source.file.position = 0;
        self.source.file.pending_seek = None;
    }

    /// The tunable band `[lo, hi]` in Hz used when committing a frequency edit.
    /// Derived from the opened device's frequency range when available, else a
    /// permissive default.
    pub fn freq_band(&self) -> (u64, u64) {
        if let Some(v) = &self.source.soapy
            && let Some((lo, hi)) = v.freq_range
        {
            return (lo.max(0.0) as u64, hi as u64);
        }
        (0, 2_000_000_000)
    }

    /// The resolved current theme colors.
    pub fn theme(&self) -> Theme {
        self.themes.current().clone()
    }

    /// Cycle to the next theme.
    pub fn cycle_theme(&mut self) {
        self.themes.cycle();
    }

    // ── Selection popups ─────────────────────────────────────────────────

    /// Whether a modal selection popup is currently open.
    pub fn popup_open(&self) -> bool {
        self.popup.is_some()
    }

    /// Open the selection popup appropriate to the currently-focused control.
    /// Radio Mode opens a mode picker; Source Device/SampleRate/Bandwidth open
    /// the matching list. Other fields are no-ops (they edit in place).
    pub fn open_popup_for_focus(&mut self) {
        use crate::ui::nav::Focus;
        use crate::vfo::radio::RadioField;
        match self.nav.focus {
            Focus::Radio if self.radio.field() == RadioField::Type => {
                if self.radio.active().recording {
                    return;
                }
                let items = self.radio.type_options();
                let selected = items
                    .iter()
                    .position(|t| *t == self.radio.active().vfo_type)
                    .unwrap_or(0);
                self.popup = Some(ListPopup::new("Type", items, selected));
                self.popup_target = Some(PopupTarget::VfoType);
            }
            Focus::Radio if self.radio.is_choice(self.radio.field()) => {
                let RadioField::Setting(i) = self.radio.field() else {
                    return;
                };
                let Some((f, v)) = self.radio.setting_value(i as usize) else {
                    return;
                };
                let crate::vfo::types::FieldKind::Choice { options } = &f.kind else {
                    return;
                };
                let items: Vec<String> = options.iter().map(|o| o.label.clone()).collect();
                let selected = options.iter().position(|o| o.value == v).unwrap_or(0);
                self.popup = Some(ListPopup::new(f.label.clone(), items, selected));
                self.popup_target = Some(PopupTarget::VfoSetting(i));
            }
            Focus::Radio if self.radio.field() == RadioField::Audio => {
                let items = self.radio.audio_option_labels();
                let selected = self.radio.selected_audio_index();
                self.popup = Some(ListPopup::new("Audio", items, selected));
                self.popup_target = Some(PopupTarget::VfoAudio);
            }
            Focus::Source => self.open_source_popup(),
            _ => {}
        }
    }

    /// Open the popup for the focused Source field, if it is a list control.
    fn open_source_popup(&mut self) {
        use crate::source::model::SourceField;
        match self.source.field() {
            SourceField::Device => {
                let items: Vec<String> = self
                    .source
                    .devices
                    .iter()
                    .map(|d| d.label.clone())
                    .collect();
                if items.is_empty() {
                    return;
                }
                self.popup = Some(ListPopup::new("Device", items, self.source.device_idx));
                self.popup_target = Some(PopupTarget::SourceDevice);
            }
            SourceField::SampleRate => {
                let Some(v) = &self.source.soapy else { return };
                let items: Vec<String> = v
                    .sample_rates
                    .iter()
                    .map(|r| format!("{:.3} MHz", r / 1e6))
                    .collect();
                if items.is_empty() {
                    return;
                }
                self.popup = Some(ListPopup::new("Sample rate", items, v.sample_rate_idx));
                self.popup_target = Some(PopupTarget::SourceSampleRate);
            }
            SourceField::Bandwidth => {
                let Some(v) = &self.source.soapy else { return };
                let items: Vec<String> = v
                    .bandwidths
                    .iter()
                    .map(|b| format!("{:.3} MHz", b / 1e6))
                    .collect();
                if items.is_empty() {
                    return;
                }
                self.popup = Some(ListPopup::new("Bandwidth", items, v.bandwidth_idx));
                self.popup_target = Some(PopupTarget::SourceBandwidth);
            }
            SourceField::Decimation => {
                use crate::dsp::power_decimator::{RATIOS, ratio_label};
                let items: Vec<String> = RATIOS.iter().map(|r| ratio_label(*r)).collect();
                let cur = RATIOS
                    .iter()
                    .position(|r| *r == self.source.decimation())
                    .unwrap_or(0);
                self.popup = Some(ListPopup::new("Decimation", items, cur));
                self.popup_target = Some(PopupTarget::SourceDecimation);
            }
            SourceField::Antenna => {
                let Some(v) = &self.source.soapy else { return };
                if v.antennas.is_empty() {
                    return;
                }
                self.popup = Some(ListPopup::new("Antenna", v.antennas.clone(), v.antenna_idx));
                self.popup_target = Some(PopupTarget::SourceAntenna);
            }
            SourceField::NetAudio => {
                use crate::source::model::{NET_BITRATES, bitrate_label};
                let items = NET_BITRATES.iter().map(|b| bitrate_label(*b)).collect();
                let cur = NET_BITRATES
                    .iter()
                    .position(|b| *b == self.source.network.bitrate)
                    .unwrap_or(0);
                self.popup = Some(ListPopup::new("Compression (network audio)", items, cur));
                self.popup_target = Some(PopupTarget::SourceNetAudio);
            }
            _ => {}
        }
    }

    /// Open the device by `args`, read its capabilities, and close it,
    /// returning the probed capabilities. Tries the mock first (for `--test`
    /// and the suite) then real SoapySDR hardware, silencing native prints.
    pub fn probe_device_caps(args: &str) -> Option<crate::source::Capabilities> {
        use crate::source::SdrSource;
        if let Ok(src) = crate::source::MockSource::open(args) {
            return Some(src.capabilities().clone());
        }
        let _hush = crate::engine::silence::StdioSilencer::new();
        crate::source::soapy::SoapySource::open(args)
            .ok()
            .map(|src| src.capabilities().clone())
    }

    /// Move the popup cursor down (if open).
    pub fn popup_down(&mut self) {
        if let Some(p) = &mut self.popup {
            p.down();
        }
    }

    /// Move the popup cursor up (if open).
    pub fn popup_up(&mut self) {
        if let Some(p) = &mut self.popup {
            p.up();
        }
    }

    /// Confirm the popup selection, applying it to the target, and close it.
    pub fn popup_confirm(&mut self) {
        let (Some(p), Some(target)) = (self.popup.take(), self.popup_target.take()) else {
            self.popup = None;
            self.popup_target = None;
            return;
        };
        if let Some(idx) = p.confirm() {
            match target {
                PopupTarget::VfoType => {
                    if let Some(id) = self.radio.type_options().get(idx).cloned() {
                        self.radio.set_type(&id);
                    }
                }
                PopupTarget::VfoSetting(i) => {
                    if let Some(crate::vfo::types::FieldKind::Choice { options }) =
                        self.radio.setting_field(i as usize).map(|f| f.kind)
                        && let Some(o) = options.get(idx)
                    {
                        self.radio.set_setting(i as usize, o.value.clone());
                    }
                }
                PopupTarget::VfoAudio => {
                    self.radio.apply_audio_index(idx);
                }
                PopupTarget::SourceDevice => self.select_source_device(idx),
                PopupTarget::SourceNetAudio => {
                    if let Some(b) = crate::source::model::NET_BITRATES.get(idx) {
                        self.source.network.bitrate = *b;
                    }
                }
                PopupTarget::SourceSampleRate => {
                    if let Some(v) = &mut self.source.soapy {
                        v.set_sample_rate_idx(idx);
                    }
                }
                PopupTarget::SourceBandwidth => {
                    if let Some(v) = &mut self.source.soapy {
                        v.set_bandwidth_idx(idx);
                    }
                }
                PopupTarget::SourceDecimation => {
                    if let Some(r) = crate::dsp::power_decimator::RATIOS.get(idx) {
                        self.source.set_decimation(*r);
                    }
                }
                PopupTarget::SourceAntenna => {
                    if let Some(v) = &mut self.source.soapy
                        && idx < v.antennas.len()
                    {
                        v.antenna_idx = idx;
                    }
                }
                PopupTarget::BookmarkList => self.bookmarks.select_list(idx),
                PopupTarget::ConfirmDeleteList => {
                    if idx == 1 {
                        self.bookmarks.delete_list();
                    }
                }
                PopupTarget::ConfirmRemoveBookmark(i) => {
                    if idx == 1 {
                        self.bookmarks.remove_bookmark(i);
                    }
                }
                PopupTarget::CopyBookmark(i) | PopupTarget::MoveBookmark(i) => {
                    let keep = matches!(target, PopupTarget::CopyBookmark(_));
                    if let Err(e) = self.bookmarks.transfer(i, idx, keep) {
                        self.show_notice(e);
                    }
                }
                PopupTarget::ScheduleBookmark => self.sched_pick_confirm(idx),
                PopupTarget::ConfirmRemoveJob(i) => {
                    if idx == 1 {
                        self.schedule.remove_job(i);
                    }
                }
                PopupTarget::ScheduleList => self.schedule.select_list(idx),
                PopupTarget::MoveJob(i) => self.schedule.move_job(i, idx),
                PopupTarget::ConfirmDeleteSchedList => {
                    if idx == 1
                        && let Err(e) = self.schedule.delete_list()
                    {
                        self.show_notice(e);
                    }
                }
                PopupTarget::ReportRelocate => self.report_relocate(idx),
                PopupTarget::ConfirmActivate => {
                    if idx == 1 {
                        self.schedule.activate_view();
                    }
                }
                PopupTarget::QuitBlocked | PopupTarget::Notice => {}
            }
        }
    }

    /// Cancel and close the popup without applying anything.
    pub fn popup_cancel(&mut self) {
        self.popup = None;
        self.popup_target = None;
    }

    // ── Source field editing (quick-cycle / adjust / toggle) ─────────────

    /// Left/Right (or Up/Down within a field) action on the focused Source
    /// field. `dir` is +1 for right/increase, -1 for left/decrease. Device,
    /// sample-rate and bandwidth quick-cycle through their lists; gains nudge
    /// by a step; Kind and AGC toggle (direction-agnostic).
    pub fn source_adjust(&mut self, dir: i32) {
        use crate::source::model::SourceField;
        const GAIN_STEP_DB: f64 = 1.0;
        match self.source.field() {
            SourceField::Kind => self.step_source_kind(dir),
            SourceField::Device => self.source_cycle_device(dir),
            SourceField::SampleRate => {
                if let Some(v) = &mut self.source.soapy {
                    let n = v.sample_rates.len();
                    if n > 0 {
                        let i = step_index(v.sample_rate_idx, dir, n);
                        v.set_sample_rate_idx(i);
                    }
                }
            }
            SourceField::Bandwidth => {
                if let Some(v) = &mut self.source.soapy {
                    let n = v.bandwidths.len();
                    if n > 0 {
                        let i = step_index(v.bandwidth_idx, dir, n);
                        v.set_bandwidth_idx(i);
                    }
                }
            }
            SourceField::Decimation => self.source.step_decimation(dir),
            SourceField::Antenna => {
                if let Some(v) = &mut self.source.soapy {
                    let n = v.antennas.len();
                    if n > 0 {
                        v.antenna_idx = step_index(v.antenna_idx, dir, n);
                    }
                }
            }
            SourceField::Gain(gi) => {
                if let Some(v) = &mut self.source.soapy {
                    v.adjust_gain(gi, GAIN_STEP_DB * dir as f64);
                }
            }
            SourceField::Agc => {
                if let Some(v) = &mut self.source.soapy {
                    v.toggle_agc();
                }
            }
            SourceField::FilePath | SourceField::NetServer | SourceField::NetToken => {}
            SourceField::NetAudio => self.source.network.step_bitrate(dir),
            SourceField::NetOpen => {}
            SourceField::ShareOn
            | SourceField::ShareMute
            | SourceField::ShareListen
            | SourceField::ShareToken
            | SourceField::ShareDisconnect => self.share_adjust(),
            f @ (SourceField::FileTransport | SourceField::FileBack | SourceField::FileFwd) => {
                self.source.file.step_button(f, dir)
            }
        }
    }

    /// ENTER on the focused Source field in the File view: the path opens a
    /// prompt, a button row presses its selected button. Returns whether the
    /// field was a File-view control.
    pub fn activate_file_field(&mut self) -> bool {
        use crate::source::model::SourceField;
        let f = self.source.field();
        if f == SourceField::FilePath {
            self.open_file_browser();
            return true;
        }
        match self.source.file.selected_button(f) {
            Some(b) => {
                self.press_file_button(b);
                true
            }
            None => false,
        }
    }

    /// Open the file dialog in the current file's folder (cursor on it), or
    /// in the recordings folder, where baseband recordings land.
    pub fn open_file_browser(&mut self) {
        let (dir, name) = match self.source.file.path.as_deref().map(std::path::Path::new) {
            Some(p) => (
                p.parent().map(|d| d.to_path_buf()).unwrap_or_default(),
                p.file_name().map(|n| n.to_string_lossy().into_owned()),
            ),
            None => (
                std::path::PathBuf::from(expand_home(self.active_rec_dir())),
                None,
            ),
        };
        self.file_browser = Some(crate::ui::file_browser::FileBrowser::open(
            &dir,
            name.as_deref(),
        ));
    }

    /// Act on what the file dialog returned: load the chosen file (the dialog
    /// stays open with the error if it is not playable), switch to typing a
    /// path, or close.
    pub fn file_browser_outcome(&mut self, outcome: crate::ui::file_browser::Outcome) {
        use crate::ui::file_browser::Outcome;
        match outcome {
            Outcome::Stay => {}
            Outcome::Cancel => self.file_browser = None,
            Outcome::Choose(p) => match self.load_file(&p.to_string_lossy()) {
                Ok(()) => self.file_browser = None,
                Err(e) => {
                    if let Some(b) = &mut self.file_browser {
                        b.error = Some(e);
                    }
                }
            },
            Outcome::TypePath(dir) => {
                self.file_browser = None;
                self.open_file_prompt(&format!("{}/", dir.display()));
            }
        }
    }

    /// Open the typed-path prompt (the dialog's [Path…] button). A folder
    /// typed there reopens the dialog in it.
    pub fn open_file_prompt(&mut self, value: &str) {
        self.prompt = Some(TextPrompt::new("Baseband WAV file or folder", value));
        self.prompt_target = Some(PromptTarget::FilePath);
    }

    /// Quick-cycle the selected device by `dir`, re-probing it (same path as
    /// the popup selection).
    fn source_cycle_device(&mut self, dir: i32) {
        let n = self.source.devices.len();
        if n == 0 {
            return;
        }
        let i = step_index(self.source.device_idx, dir, n);
        self.select_source_device(i);
    }

    /// Scroll the left panel by a signed delta (rows), clamped to content.
    pub fn scroll_left(&mut self, delta: i32) {
        let content = crate::ui::scroll::content_height(&crate::ui::panel::module_heights(self));
        let new = (self.left_scroll as i32 + delta).max(0) as u16;
        self.left_scroll = crate::ui::scroll::clamp_offset(new, content, self.left_viewport);
    }

    /// Page the left panel up/down by (viewport - 1).
    pub fn page_left(&mut self, down: bool) {
        let step = crate::ui::scroll::page_step(self.left_viewport) as i32;
        self.scroll_left(if down { step } else { -step });
    }

    /// Reconcile the scroll offset with the current viewport: clamp it, then
    /// ensure the focused field/module is visible. Called from render each
    /// frame with the measured viewport height so resize is handled.
    pub fn reconcile_left_scroll(&mut self, viewport: u16) {
        self.left_viewport = viewport;
        let content = crate::ui::scroll::content_height(&crate::ui::panel::module_heights(self));
        self.left_scroll = crate::ui::scroll::clamp_offset(self.left_scroll, content, viewport);
        let target = crate::ui::panel::focused_span(self);
        // Only auto-scroll to a real target (Global has an empty span).
        if target.end > target.start {
            self.left_scroll =
                crate::ui::scroll::ensure_visible(target, self.left_scroll, viewport);
            self.left_scroll = crate::ui::scroll::clamp_offset(self.left_scroll, content, viewport);
        }
    }

    /// The committed source center frequency in Hz.
    // Consumed by the Piece B2 device-tuning wiring.
    #[allow(dead_code)]
    pub fn center_hz(&self) -> u64 {
        self.center_freq.committed_hz()
    }

    // ── Global Controls bar field navigation ─────────────────────────────

    /// ENTER on the focused global control: toggle it, or enter/leave edit for
    /// the center-frequency field (committing + reprojecting VFOs on leave).
    pub fn global_enter(&mut self) {
        use GlobalField as G;
        match self.global_field {
            G::StartStop => self.toggle_run(),
            G::Spectrum => self.toggle_spectrum(),
            G::Waterfall => self.toggle_waterfall(),
            G::CenterFree => self.toggle_tune_mode(),
            G::Theme => self.cycle_theme(),
            G::Mute => self.toggle_mute(),
            G::Help => self.toggle_help(),
            G::Layout => self.toggle_work_layout(),
            // Steppers: adjusted with Left/Right (or the wheel) while selected.
            G::Volume | G::Panel => {}
            G::Frequency => {
                if self.global_editing {
                    self.global_editing = false;
                    self.commit_center();
                } else {
                    self.global_editing = true;
                }
            }
        }
    }

    // ── Work Area (spec 13.5) ────────────────────────────────────────────

    /// Take a VFO view frame from the engine: crop the channel spectrum to
    /// the zoom (about +/-1.5 x bandwidth), keep 0..4 kHz of the audio, and
    /// feed both waterfalls (spec 13.5).
    pub fn observe_view(&mut self, f: crate::engine::VfoViewFrame) {
        use crate::ui::vfo_view::{AUDIO_VIEW_HZ, audio_bins, crop_centered, zoom_half_span};
        let half = zoom_half_span(self.radio.active().bandwidth(), f.zoom_rate);
        if (2.0 * half - self.zoom_span_hz).abs() > 1e-6 {
            // A new zoom: old rows have another scale.
            self.zoom_history.clear();
            self.zoom_span_hz = 2.0 * half;
        }
        if !f.zoom_db.is_empty() {
            let z = crop_centered(&f.zoom_db, f.zoom_rate, half);
            self.zoom_range.observe(&z);
            self.zoom_history.push(z.clone());
            self.zoom_db = z;
        }
        if !f.audio_db.is_empty() {
            let a = audio_bins(&f.audio_db, crate::engine::AUDIO_RATE as f64, AUDIO_VIEW_HZ);
            self.audio_range.observe(&a);
            self.audio_history.push(a);
        }
        self.view = Some(f);
    }

    /// Forget the VFO view's data (the engine stopped).
    pub fn clear_view(&mut self) {
        self.view = None;
        self.zoom_db.clear();
        self.zoom_history.clear();
        self.audio_history.clear();
    }

    /// A click in the zoomed channel: move only the VFO, snapped to 10 Hz
    /// (fine tuning for CW/SSB; the source center stays).
    pub fn zoom_tune(&mut self, hz: f64) {
        let hz = crate::vfo::model::snap(hz.round().max(0.0) as u64, 10);
        let (lo, hi) = self.freq_band();
        self.radio.active_mut_freq(hz.clamp(lo, hi));
    }

    /// Whether the engine should compute the spectrum: a display is on and
    /// the spectrum stack is on screen (low-fi: zero FFT cost otherwise).
    pub fn fft_wanted(&self) -> bool {
        (self.spectrum_on || self.waterfall_on) && (self.spectrum_visible || self.remote_shows.0)
    }

    /// `l` / `[Tabs]`/`[Split]`: switch the Work Area layout.
    pub fn toggle_work_layout(&mut self) {
        use crate::config::WorkAreaLayout as L;
        self.work_area.layout = match self.work_area.layout {
            L::Tabs => L::Split,
            L::Split => L::Tabs,
        };
    }

    /// `w`: show the other tab (in Split both are shown; the tab is still
    /// switched for when Tabs comes back).
    pub fn switch_work_tab(&mut self) {
        use crate::config::WorkAreaTab as T;
        self.select_work_tab(match self.work_area.tab {
            T::Spectrum => T::Vfo,
            T::Vfo => T::Spectrum,
        });
    }

    /// Show tab `tab` (a click on the tab row).
    pub fn select_work_tab(&mut self, tab: crate::config::WorkAreaTab) {
        self.work_area.tab = tab;
    }

    /// The Work Area's regions on the last drawn screen size.
    pub fn work_regions(&self, area: ratatui::layout::Rect) -> crate::ui::layout::Regions {
        crate::ui::layout::compute_layout_with(
            area,
            self.bookmarks.strip_rows,
            self.left_panel_width,
            self.work_area,
        )
    }

    /// Esc: cancel a center-frequency edit.
    pub fn global_cancel(&mut self) {
        if self.global_editing && self.global_field == GlobalField::Frequency {
            self.center_freq.cancel();
        }
        self.global_editing = false;
    }

    /// Left/Right on the focused global control.
    pub fn global_left(&mut self) {
        self.global_horizontal(false);
    }

    pub fn global_right(&mut self) {
        self.global_horizontal(true);
    }

    fn global_horizontal(&mut self, right: bool) {
        use GlobalField as G;
        match self.global_field {
            G::Frequency if self.global_editing => {
                if right {
                    self.center_freq.cursor_right();
                } else {
                    self.center_freq.cursor_left();
                }
            }
            G::CenterFree => self.toggle_tune_mode(),
            G::Theme => self.cycle_theme(),
            G::Mute => self.toggle_mute(),
            G::Layout => self.toggle_work_layout(),
            G::Panel => self.resize_left_panel(right),
            G::Volume => {
                let step = 0.05;
                self.volume = (self.volume + if right { step } else { -step }).clamp(0.0, 1.0);
            }
            _ => {}
        }
    }

    /// Up/Down on the focused global control: change the center-frequency
    /// digit while editing, else move the field cursor.
    pub fn global_up(&mut self) {
        if self.global_field == GlobalField::Frequency && self.global_editing {
            self.center_freq.increment();
        } else if !self.global_editing {
            self.global_field = self.global_field.prev();
        }
    }

    pub fn global_down(&mut self) {
        if self.global_field == GlobalField::Frequency && self.global_editing {
            self.center_freq.decrement();
        } else if !self.global_editing {
            self.global_field = self.global_field.next();
        }
    }

    /// Type a digit into the center-frequency field while editing it.
    pub fn global_type_digit(&mut self, d: u8) {
        if self.global_field == GlobalField::Frequency && self.global_editing {
            self.center_freq.type_digit(d);
        }
    }

    /// Zero digits to the right while editing the center frequency.
    pub fn global_zero_right(&mut self) {
        if self.global_field == GlobalField::Frequency && self.global_editing {
            self.center_freq.zero_right();
        }
    }

    /// Commit the pending center-frequency edit, clamp to the device band, and
    /// reproject all VFOs so their offset from center is preserved.
    fn commit_center(&mut self) {
        let (lo, hi) = self.center_band();
        let old = self.center_freq.committed_hz();
        let res = self.center_freq.commit(lo, hi);
        self.radio.reproject_to_center(old, res.hz);
    }

    // ── Bookmarks ────────────────────────────────────────────────────────

    /// Load the shared bookmarks file into the manager (startup). Returns a
    /// warning when the file was unreadable/malformed (defaults are used).
    pub fn load_bookmarks(&mut self, mut file: StoreFile) -> Option<String> {
        let (store, warn) = file.load();
        self.bookmarks.replace_store(store);
        if file.read_only {
            self.note_file_warning(warn.clone());
        }
        self.bookmark_file = Some(file);
        warn
    }

    /// Remember a read-only-file warning for the status bar (the first one
    /// stays; the others went to stderr at startup).
    pub fn note_file_warning(&mut self, warning: Option<String>) {
        if self.file_warning.is_none() {
            self.file_warning = warning;
        }
    }

    /// Save the bookmarks when they changed; with `check_disk`, also reload
    /// them when another instance saved the shared file. Returns a warning
    /// when saving failed.
    pub fn sync_bookmarks(&mut self, check_disk: bool) -> Option<String> {
        let file = self.bookmark_file.as_mut()?;
        if self.bookmarks.take_dirty() {
            return file
                .save(&self.bookmarks.store)
                .err()
                .map(|e| format!("cannot save bookmarks {}: {e}", file.path.display()));
        }
        if check_disk && file.changed_on_disk() {
            let (store, warn) = file.load();
            if warn.is_none() {
                self.bookmarks.replace_store(store);
            }
            return warn;
        }
        None
    }

    /// Whether keystrokes are text for a text field (prompt, editor text
    /// field, bookmark filter) rather than app accelerators.
    pub fn text_capture(&self) -> bool {
        if self.popup.is_some() || self.help.is_some() {
            return false;
        }
        // The prompt types text; the file dialog uses letters for type-ahead.
        if self.prompt.is_some() || self.file_browser.is_some() {
            return true;
        }
        // The editor form is modal and text-heavy: every key is text, so a
        // letter typed on a field edits it instead of firing an accelerator.
        if self.bm_editor.is_some() || self.sched_editor.is_some() {
            return true;
        }
        self.bookmarks.is_filter_editing()
    }

    /// The span shown by the spectrum and the strip: the effective (decimated)
    /// sample rate, else the last seen span.
    pub fn display_span_hz(&self) -> f64 {
        self.source
            .effective_sample_rate()
            .unwrap_or(self.spectrum_span_hz)
    }

    /// Carry out a request from the Bookmarks module.
    pub fn bookmark_request(&mut self, req: Request) {
        match req {
            Request::None => {}
            Request::OpenPage => self.open_page(crate::ui::page::WorkOwner::Bookmarks),
            Request::PickList => {
                let names = self.bookmarks.list_names();
                if names.is_empty() {
                    return;
                }
                let cur = self.bookmarks.list_index().unwrap_or(0);
                self.popup = Some(ListPopup::new("Bookmark list", names, cur));
                self.popup_target = Some(PopupTarget::BookmarkList);
            }
            Request::NewList => {
                let name = self.bookmarks.store.unique_list_name("New List");
                self.prompt = Some(TextPrompt::new("New list", name));
                self.prompt_target = Some(PromptTarget::NewList);
            }
            Request::RenameList => {
                if let Some(l) = self.bookmarks.current_list() {
                    self.prompt = Some(TextPrompt::new("Rename list", l.name.clone()));
                    self.prompt_target = Some(PromptTarget::RenameList);
                }
            }
            Request::DeleteList => {
                if let Some(l) = self.bookmarks.current_list() {
                    let title = format!(
                        "Delete list \"{}\" ({} bookmarks)?",
                        l.name,
                        l.bookmarks.len()
                    );
                    self.popup = Some(ListPopup::new(title, confirm_items("Delete"), 0));
                    self.popup_target = Some(PopupTarget::ConfirmDeleteList);
                }
            }
            Request::Add => self.open_bookmark_add(),
            Request::Edit(i) => self.open_bookmark_edit(i),
            Request::Remove(i) => {
                if let Some(b) = self
                    .bookmarks
                    .current_list()
                    .and_then(|l| l.bookmarks.get(i))
                {
                    let title = format!("Remove \"{}\"?", b.name);
                    self.popup = Some(ListPopup::new(title, confirm_items("Remove"), 0));
                    self.popup_target = Some(PopupTarget::ConfirmRemoveBookmark(i));
                }
            }
            Request::Tune(l, i) => self.tune_bookmark(l, i),
            Request::QuickAdd => self.quick_add_bookmark(),
            Request::Copy(i) | Request::Move(i) => {
                let Some(b) = self
                    .bookmarks
                    .current_list()
                    .and_then(|l| l.bookmarks.get(i))
                else {
                    return;
                };
                let (verb, target) = match req {
                    Request::Copy(_) => ("Copy", PopupTarget::CopyBookmark(i)),
                    _ => ("Move", PopupTarget::MoveBookmark(i)),
                };
                let title = format!("{verb} \"{}\" to list", b.name);
                let cur = self.bookmarks.list_index().unwrap_or(0);
                self.popup = Some(ListPopup::new(title, self.bookmarks.list_names(), cur));
                self.popup_target = Some(target);
            }
        }
    }

    /// `[Quick Add]`: the active VFO into the selected list as "New
    /// Bookmark" (or "New Bookmark (n)"), without the editor (spec 19.4).
    fn quick_add_bookmark(&mut self) {
        if self.bookmarks.list_index().is_none() {
            let _ = self
                .bookmarks
                .create_list(crate::bookmarks::model::DEFAULT_LIST);
        }
        let Some(list) = self.bookmarks.list_index() else {
            return;
        };
        let name = self.bookmarks.unique_bookmark_name(list, "New Bookmark");
        let bm = Bookmark::from_vfo(name, self.radio.active());
        // The new row is selected; only a failure needs a word.
        if let Err(e) = self.bookmarks.save_bookmark(None, list, bm) {
            self.show_notice(e);
        }
    }

    /// Open the editor on a new bookmark captured from the active VFO, into
    /// the selected list (a "General" list is created when none exists).
    fn open_bookmark_add(&mut self) {
        if self.bookmarks.list_index().is_none() {
            let name = crate::bookmarks::model::DEFAULT_LIST;
            let _ = self.bookmarks.create_list(name);
        }
        let Some(list) = self.bookmarks.list_index() else {
            return;
        };
        let name = self.bookmarks.unique_bookmark_name(list, "New Bookmark");
        let bm = Bookmark::from_vfo(name, self.radio.active());
        self.bm_editor = Some(BookmarkEditor::new(
            &bm,
            None,
            list,
            self.bookmarks.list_names(),
        ));
    }

    /// Open the editor on bookmark `idx` of the selected list.
    fn open_bookmark_edit(&mut self, idx: usize) {
        let Some(list) = self.bookmarks.list_index() else {
            return;
        };
        if let Some(bm) = self.bookmarks.store.lists[list].bookmarks.get(idx).cloned() {
            self.bm_editor = Some(BookmarkEditor::new(
                &bm,
                Some((list, idx)),
                list,
                self.bookmarks.list_names(),
            ));
        }
    }

    /// Act on an editor outcome: Apply validates and saves (the form stays
    /// open showing the error when invalid), Cancel closes it.
    pub fn editor_outcome(&mut self, outcome: EditorOutcome) {
        match outcome {
            EditorOutcome::Open => {}
            EditorOutcome::Cancel => self.bm_editor = None,
            EditorOutcome::Apply => {
                let Some(ed) = &mut self.bm_editor else {
                    return;
                };
                let result = ed
                    .build()
                    .and_then(|(target, bm)| self.bookmarks.save_bookmark(ed.original, target, bm));
                match result {
                    Ok(()) => self.bm_editor = None,
                    Err(e) => ed.error = Some(e),
                }
            }
        }
    }

    /// ENTER / [OK] on the text prompt: apply the name, closing the prompt,
    /// or keep it open with the validation error.
    pub fn prompt_confirm(&mut self) {
        let (Some(p), Some(target)) = (&self.prompt, self.prompt_target) else {
            return;
        };
        let name = p.input.text().to_string();
        let result = match target {
            PromptTarget::NewList => self.bookmarks.create_list(&name),
            PromptTarget::RenameList => self.bookmarks.rename_list(&name),
            PromptTarget::NewSchedList => self.schedule.create_list(&name),
            PromptTarget::ReportDir => {
                let r = self.report_dir_entered(&name);
                if r.is_ok() {
                    // The Move / Start fresh question replaces the prompt.
                    self.prompt = None;
                    self.prompt_target = None;
                    return;
                }
                r
            }
            PromptTarget::ReportHook => {
                self.report_config.hook = name.trim().to_string();
                self.report_apply_hook();
                Ok(())
            }
            PromptTarget::ReportKeep(i) => match name.trim().parse::<u32>() {
                Ok(n) => {
                    let mut keep = [
                        self.report_config.keep_days,
                        self.report_config.keep_log_days,
                        self.report_config.keep_months,
                    ];
                    keep[i as usize] = n;
                    self.report_set_keep(keep);
                    Ok(())
                }
                Err(_) => Err("A whole number, 0 = forever".to_string()),
            },
            PromptTarget::RenameSchedList => self.schedule.rename_list(&name),
            PromptTarget::RecTemplate => {
                self.radio.set_rec_template(&name);
                Ok(())
            }
            PromptTarget::FilePath => {
                let full = expand_home(name.trim());
                if std::path::Path::new(&full).is_dir() {
                    self.prompt_cancel();
                    self.file_browser = Some(crate::ui::file_browser::FileBrowser::open(
                        std::path::Path::new(&full),
                        None,
                    ));
                    return;
                }
                self.load_file(&name)
            }
            PromptTarget::RecDir => {
                let d = name.trim();
                if d.is_empty() {
                    Err("Folder must not be empty".to_string())
                } else {
                    let t = self.radio.active().vfo_type.clone();
                    self.rec_dirs.insert(t, d.to_string());
                    Ok(())
                }
            }
            PromptTarget::VfoSetting(i) => self.radio.set_setting_text(i as usize, name.trim()),
            PromptTarget::NetServer => {
                self.set_network_server(name.trim());
                Ok(())
            }
            PromptTarget::ShareListen => {
                let v = name.trim();
                if v.is_empty() {
                    Err("An address, e.g. 0.0.0.0:7373".to_string())
                } else {
                    self.source.share.listen = v.to_string();
                    Ok(())
                }
            }
            PromptTarget::ShareToken => {
                self.source.share.token = name.trim().to_string();
                Ok(())
            }
            PromptTarget::NetToken => {
                self.source.network.token = name.trim().to_string();
                self.network_query();
                Ok(())
            }
            t @ (PromptTarget::DscSearch
            | PromptTarget::DscLocation
            | PromptTarget::DscApiKey
            | PromptTarget::DscEndpoint
            | PromptTarget::DscSave) => self.dsc_prompt_confirm(t, &name),
            t @ (PromptTarget::HfdlSearch
            | PromptTarget::HfdlLocation
            | PromptTarget::HfdlSave
            | PromptTarget::HfdlWatch
            | PromptTarget::HfdlKeywordsEmergency
            | PromptTarget::HfdlKeywordsCaution
            | PromptTarget::HfdlAircraftFile
            | PromptTarget::HfdlAirlinesFile
            | PromptTarget::HfdlAirportsFile) => self.hfdl_prompt_confirm(t, &name),
        };
        match result {
            Ok(()) => self.prompt_cancel(),
            Err(e) => {
                if let Some(p) = &mut self.prompt {
                    p.error = Some(e);
                }
            }
        }
    }

    pub fn prompt_cancel(&mut self) {
        self.prompt = None;
        self.prompt_target = None;
    }

    /// '/' accelerator: focus the Bookmarks module and type into its filter.
    pub fn find_bookmark(&mut self) {
        use crate::ui::nav::Focus;
        self.end_edits();
        self.nav.focus = Focus::Bookmarks;
        self.nav.collapsed.bookmarks = false;
        self.bookmarks.begin_find();
    }

    /// End in-progress edits outside the bookmarks UI the way ENTER does.
    fn end_edits(&mut self) {
        if self.global_editing {
            self.global_field = GlobalField::Frequency;
            self.global_enter();
        }
        if self.radio.is_editing() {
            let (lo, hi) = self.freq_band();
            self.radio.toggle_edit(lo, hi);
        }
    }

    /// Tune the active VFO to bookmark (`list`, `idx`): apply its mode and
    /// settings, then its frequency (selecting it in the module).
    pub fn tune_bookmark(&mut self, list: usize, idx: usize) {
        let Some(bm) = self
            .bookmarks
            .store
            .lists
            .get(list)
            .and_then(|l| l.bookmarks.get(idx))
            .cloned()
        else {
            return;
        };
        if self.global_editing {
            self.global_editing = false;
            self.center_freq.cancel();
        }
        self.bookmarks.select_bookmark(list, idx);
        // The type first, then its settings, then the frequency (spec 13.7).
        // A bookmark of a type this build lacks (or a VFO recording as
        // another type) is not tuned: say why.
        if !self.radio.apply_type_settings(&bm.vfo_type, &bm.settings) {
            let why = if bm.type_def().is_none() {
                format!(
                    "\"{}\" is a {} bookmark: not in this build",
                    bm.name, bm.vfo_type
                )
            } else {
                format!("Stop recording to tune a {} bookmark", bm.vfo_type)
            };
            self.show_notice(why);
            return;
        }
        self.tune_active_to(bm.frequency_hz);
        let list = self
            .bookmarks
            .store
            .lists
            .get(list)
            .map_or("", |l| l.name.as_str())
            .to_string();
        self.report_event(
            crate::report::model::Level::Info,
            crate::report::model::Category::User,
            format!("Tuned bookmark {} ({list})", bm.name),
            None,
        );
    }

    /// Click-to-tune in the spectrum / waterfall: `hz` is the clicked
    /// column's absolute frequency. It is snapped to the active VFO's snap
    /// interval (as SDR++ does), then tuned per the Center/Free mode
    /// (`tune_active_to`).
    pub fn click_tune(&mut self, hz: f64) {
        if self.global_editing {
            self.global_editing = false;
            self.center_freq.cancel();
        }
        let hz = crate::vfo::model::snap(hz.round().max(0.0) as u64, self.radio.active().snap_hz);
        self.tune_active_to(hz);
    }

    /// Move the active VFO to `hz`. Center mode re-centers the source on it;
    /// Free mode moves only the VFO while its channel fits the span, else
    /// retunes the source SDR++-style (`normal_tuning_center`). Other VFOs
    /// keep their offsets from the center.
    pub fn tune_active_to(&mut self, hz: u64) {
        let center = self.center_hz();
        let target = match self.tune_mode {
            TuneMode::Center => hz,
            TuneMode::Free => crate::vfo::model::normal_tuning_center(
                center as f64,
                self.display_span_hz(),
                self.radio.active().bandwidth_hz,
                hz as f64,
            )
            .round()
            .max(0.0) as u64,
        };
        let (lo, hi) = self.center_band();
        let new_center = target.clamp(lo, hi);
        if new_center != center {
            self.center_freq = FreqEntry::new(new_center);
            self.radio.reproject_to_center(center, new_center);
        }
        self.radio.active_mut_freq(hz);
    }

    /// The bookmarks to draw on the strip, with their on-air and selected
    /// state.
    pub fn strip_items(&self) -> Vec<crate::ui::bookmark_strip::StripItem> {
        let sel = (self.bookmarks.list_index(), self.bookmarks.selected());
        self.bookmarks
            .strip_entries()
            .into_iter()
            .map(|(l, i, b)| crate::ui::bookmark_strip::StripItem {
                key: (l, i),
                hz: b.frequency_hz as f64,
                // A type this build lacks: `?Name`, dim (spec 13.7).
                label: if b.type_def().is_some() {
                    b.name.clone()
                } else {
                    format!("?{}", b.name)
                },
                online: b.type_def().is_some() && b.is_online(self.utc),
                selected: sel == (Some(l), Some(i)),
            })
            .collect()
    }

    /// Snapshot persistable UI state into a `Config`, preserving the theme name
    /// (passed in, since the registry owns the current name).
    pub fn to_config(&self) -> crate::config::Config {
        // Start from the loaded per-device map so other devices' saved
        // settings are preserved, then update the current device's entry.
        let mut source_devices = self.source_devices.clone();
        source_devices.extend(self.source.all_device_settings());
        crate::config::Config {
            min_version: Some(crate::version::FILE_MIN_VERSION.to_string()),
            theme: Some(self.current_theme_name()),
            center_hz: self.center_hz(),
            tune_mode: self.tune_mode,
            spectrum_on: self.spectrum_on,
            waterfall_on: self.waterfall_on,
            volume: self.volume,
            muted: self.muted,
            left_panel_width: self.left_panel_width,
            device_args: self.source.local_device_args(),
            source_kind: self.source.kind,
            file_path: self.source.file.path.clone(),
            source_devices,
            decimation: self.source.decimation(),
            recordings_dir: Some(self.rec_dir.clone()),
            recordings_dirs: self.rec_dirs.clone(),
            vfo: self
                .radio
                .vfos()
                .iter()
                .map(|v| (v.name.clone(), v.to_entry()))
                .collect(),
            vfos: Vec::new(),
            work_area: self.work_area,
            bookmarks: self.bookmarks.view_config(),
            report: self.report_config.clone(),
            network: crate::config::NetworkConfig {
                addr: self.source.network.addr.clone(),
                token: self.source.network.token.clone(),
                bitrate: self.source.network.bitrate,
                device_args: self.source.network_device_args(),
            },
            location: self
                .location
                .map(|(lat, lon)| crate::config::Location { lat, lon }),
            share: crate::config::ShareConfig {
                on: self.source.share.on,
                listen: self.source.share.listen.clone(),
                token: self.source.share.token.clone(),
                mute_while_viewed: self.source.share.mute_while_viewed,
            },
        }
    }

    /// Restore persistable UI state from a `Config` (called at startup after
    /// the theme registry is set).
    pub fn apply_config(&mut self, cfg: &crate::config::Config) {
        if let Some(name) = cfg.theme.as_deref() {
            self.select_theme(name);
        }
        self.location = cfg
            .location
            .filter(|l| l.lat.abs() <= 90.0 && l.lon.abs() <= 180.0)
            .map(|l| (l.lat, l.lon));
        self.center_freq = FreqEntry::new(cfg.center_hz);
        self.tune_mode = cfg.tune_mode;
        self.spectrum_on = cfg.spectrum_on;
        self.waterfall_on = cfg.waterfall_on;
        self.volume = cfg.volume.clamp(0.0, 1.0);
        self.muted = cfg.muted;
        self.left_panel_width = cfg.left_panel_width.clamp(
            crate::ui::layout::LEFT_PANEL_MIN_WIDTH,
            crate::ui::layout::LEFT_PANEL_MAX_WIDTH,
        );
        // Keep the per-device settings map so a later save preserves entries
        // for devices not selected this session.
        self.source_devices = cfg.source_devices.clone();
        self.source.set_decimation(cfg.decimation);
        if let Some(d) = cfg
            .recordings_dir
            .as_deref()
            .filter(|d| !d.trim().is_empty())
        {
            self.rec_dir = d.to_string();
        }
        self.rec_dirs = cfg
            .recordings_dirs
            .iter()
            .filter(|(_, d)| !d.trim().is_empty())
            .map(|(t, d)| (t.clone(), d.clone()))
            .collect();
        // Restore the last-selected source device by its args string,
        // re-probing it so the capability view matches (SDR++-style). If the
        // device is gone, the current (first-device) selection is kept.
        if let Some(args) = cfg.device_args.as_deref() {
            self.source
                .select_device_by_args(args, Self::probe_device_caps);
        }
        // Re-apply the selected device's saved settings (sample rate, bandwidth,
        // gains, antenna, AGC) onto the freshly probed view.
        if let Some(args) = self.source.selected_device_args()
            && let Some(settings) = self.source_devices.get(&args).cloned()
        {
            self.source.apply_device_settings(&settings);
        }
        // The File source: reload the last file (its center wins over the
        // saved center, as on load). A file that is gone shows its error.
        let n = &mut self.source.network;
        n.addr = cfg.network.addr.clone();
        n.token = cfg.network.token.clone();
        if crate::source::model::NET_BITRATES.contains(&cfg.network.bitrate) {
            n.bitrate = cfg.network.bitrate;
        }
        let sh = &mut self.source.share;
        sh.on = cfg.share.on;
        if !cfg.share.listen.trim().is_empty() {
            sh.listen = cfg.share.listen.clone();
        }
        sh.token = cfg.share.token.clone();
        sh.mute_while_viewed = cfg.share.mute_while_viewed;
        self.source.set_kind(cfg.source_kind);
        if cfg.source_kind == crate::source::model::SourceKind::Network {
            self.network_query_for(cfg.network.device_args.clone());
        }
        if let Some(p) = cfg.file_path.as_deref() {
            let _ = self.load_file(p);
            if self.source.file.path.is_none() {
                self.source.file.path = Some(p.to_string());
            }
        }
        self.bookmarks.apply_view_config(&cfg.bookmarks);
        self.work_area = cfg.work_area;
        self.report_config = cfg.report.clone();
        // Restore the Master VFO (0.2.0 has one VFO; Step 6 restores the list).
        if let Some(master) = cfg.vfo.get("Master") {
            self.radio.set_active_from_entry("Master", master);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cpal-device audio option for tests.
    fn audio_opt(name: &str) -> crate::engine::audio::OutputOption {
        crate::engine::audio::OutputOption {
            label: name.to_string(),
            target: crate::engine::audio_target::AudioTarget::CpalDevice(name.to_string()),
        }
    }

    /// A state with one "VOLMET" list: Rostov (USB 8939 kHz, 2300 Hz, on air
    /// 0000-1200 UTC) and a CW beacon (7030 kHz, 300 Hz, tone 650).
    fn with_bookmarks() -> AppState {
        use crate::bookmarks::model::{BookmarkList, BookmarkStore};
        use crate::vfo::model::Mode;
        let mut list = BookmarkList::new("VOLMET");
        list.bookmarks = vec![
            Bookmark {
                name: "Rostov".into(),
                frequency_hz: 8_939_000,
                start_utc: 0,
                end_utc: 1200,
                settings: crate::bookmarks::model::voice_part(Mode::Usb, Some(2_300.0), None),
                ..Bookmark::default()
            },
            Bookmark {
                name: "Beacon".into(),
                frequency_hz: 7_030_000,
                settings: crate::bookmarks::model::voice_part(Mode::Cw, Some(300.0), Some(650.0)),
                ..Bookmark::default()
            },
        ];
        let mut s = AppState::with_mock_source();
        s.bookmarks = BookmarkManager::new(BookmarkStore {
            lists: vec![list],
            ..BookmarkStore::default()
        });
        s
    }

    #[test]
    fn tuning_a_bookmark_in_center_mode_recenters_and_applies_settings() {
        use crate::vfo::model::Mode;
        let mut s = with_bookmarks();
        assert_eq!(s.tune_mode, TuneMode::Center);
        s.tune_bookmark(0, 1); // Beacon
        let v = s.radio.active();
        assert_eq!(v.frequency_hz, 7_030_000);
        assert_eq!(v.mode, Mode::Cw);
        assert_eq!(v.bandwidth_hz, 300.0);
        assert_eq!(v.cw_tone_hz, 650.0);
        assert_eq!(s.center_hz(), 7_030_000, "Center mode centers the source");
        assert_eq!(s.bookmarks.selected(), Some(1), "selected in the module");
    }

    #[test]
    fn tuning_a_bookmark_in_free_mode_follows_sdrpp_normal_tuning() {
        let mut s = with_bookmarks();
        s.tune_mode = TuneMode::Free;
        s.center_freq = FreqEntry::new(9_000_000);
        let span = s.display_span_hz();
        assert!(span > 200_000.0, "mock span {span}");
        s.tune_bookmark(0, 0); // Rostov, 61 kHz below center: fits
        assert_eq!(s.center_hz(), 9_000_000, "only the VFO moves");
        assert_eq!(s.radio.active().frequency_hz, 8_939_000);
        // Beacon is far below: the source retunes with the VFO near the top
        // edge (offset span/2 - bw/2 - span/10).
        s.tune_bookmark(0, 1);
        let want = 7_030_000.0 - (span / 2.0 - 300.0 / 2.0 - span / 10.0);
        assert_eq!(s.center_hz(), want.round() as u64);
        assert_eq!(s.radio.active().frequency_hz, 7_030_000);
    }

    #[test]
    fn click_tune_in_center_mode_recenters_on_the_snapped_click() {
        let mut s = AppState::with_mock_source();
        s.center_freq = FreqEntry::new(100_000_000);
        s.radio.active_mut_freq(100_000_000);
        let snap = s.radio.active().snap_hz;
        assert!(snap > 1, "NFM has a snap interval");
        s.click_tune(100_250_000.0 + snap as f64 / 3.0);
        assert_eq!(s.center_hz(), 100_250_000, "click becomes the center");
        assert_eq!(s.radio.active().frequency_hz, 100_250_000, "VFO follows");
    }

    #[test]
    fn click_tune_in_free_mode_moves_only_the_vfo() {
        let mut s = AppState::with_mock_source();
        s.tune_mode = TuneMode::Free;
        s.center_freq = FreqEntry::new(100_000_000);
        s.radio.active_mut_freq(100_000_000);
        let snap = s.radio.active().snap_hz;
        s.click_tune(99_700_000.0 - snap as f64 / 3.0);
        assert_eq!(s.center_hz(), 100_000_000, "the source stays put");
        assert_eq!(s.radio.active().frequency_hz, 99_700_000, "snapped");
    }

    #[test]
    fn click_tune_in_free_mode_at_the_edge_retunes_like_sdrpp() {
        // A channel that would straddle the span edge retunes the source
        // (SDR++ normal tuning), keeping the VFO on the clicked frequency.
        let mut s = AppState::with_mock_source();
        s.tune_mode = TuneMode::Free;
        s.center_freq = FreqEntry::new(100_000_000);
        let span = s.display_span_hz();
        s.click_tune(100_000_000.0 + span / 2.0);
        assert_ne!(s.center_hz(), 100_000_000, "source retuned");
        assert_eq!(
            s.radio.active().frequency_hz,
            100_000_000 + (span / 2.0) as u64
        );
    }

    #[test]
    fn click_tune_cancels_a_pending_center_edit() {
        let mut s = AppState::with_mock_source();
        s.global_editing = true;
        s.click_tune(100_100_000.0);
        assert!(!s.global_editing);
    }

    #[test]
    fn add_captures_the_vfo_and_apply_saves_into_the_list() {
        use crate::vfo::model::Mode;
        let mut s = with_bookmarks();
        s.radio.set_mode(Mode::Am);
        s.radio.active_mut_freq(6_000_000);
        s.bookmark_request(Request::Add);
        let ed = s.bm_editor.as_ref().expect("editor opened");
        assert_eq!(ed.name.text(), "New Bookmark");
        assert_eq!(ed.freq.display_hz(), 6_000_000);
        assert_eq!(ed.settings["mode"], "Am");
        s.editor_outcome(EditorOutcome::Apply);
        assert!(s.bm_editor.is_none(), "closed after saving");
        let b = s.bookmarks.selected_bookmark().unwrap();
        assert_eq!(
            (b.name.as_str(), b.frequency_hz),
            ("New Bookmark", 6_000_000)
        );
        assert!(s.bookmarks.is_dirty());
    }

    #[test]
    fn apply_with_a_duplicate_name_keeps_the_editor_open_with_an_error() {
        let mut s = with_bookmarks();
        s.bookmark_request(Request::Add);
        s.bm_editor.as_mut().unwrap().name.set("Rostov");
        s.editor_outcome(EditorOutcome::Apply);
        let ed = s.bm_editor.as_ref().expect("still open");
        assert!(ed.error.as_deref().unwrap().contains("already exists"));
    }

    #[test]
    fn add_with_no_lists_creates_general() {
        let mut s = AppState::with_mock_source();
        s.bookmarks.delete_list();
        s.bookmark_request(Request::Add);
        assert_eq!(s.bookmarks.list_names(), ["General"]);
        assert!(s.bm_editor.is_some());
    }

    #[test]
    fn delete_list_needs_confirmation_and_defaults_to_cancel() {
        let mut s = with_bookmarks();
        s.bookmark_request(Request::DeleteList);
        let p = s.popup.as_ref().expect("confirmation");
        assert!(p.title.contains("VOLMET") && p.title.contains("2 bookmarks"));
        assert_eq!(p.cursor, 0, "Cancel is the default");
        s.popup_confirm();
        assert_eq!(s.bookmarks.list_names(), ["VOLMET"], "cancelled");
        s.bookmark_request(Request::DeleteList);
        s.popup_down();
        s.popup_confirm();
        assert!(s.bookmarks.list_names().is_empty());
    }

    #[test]
    fn remove_bookmark_needs_confirmation() {
        let mut s = with_bookmarks();
        s.bookmark_request(Request::Remove(0));
        assert!(s.popup.as_ref().unwrap().title.contains("Rostov"));
        s.popup_down();
        s.popup_confirm();
        assert_eq!(s.bookmarks.store.lists[0].bookmarks.len(), 1);
    }

    #[test]
    fn new_list_prompt_validates_and_creates() {
        let mut s = with_bookmarks();
        s.bookmark_request(Request::NewList);
        assert!(s.text_capture(), "the prompt captures the keyboard");
        s.prompt.as_mut().unwrap().input.set("VOLMET");
        s.prompt_confirm();
        assert!(
            s.prompt.as_ref().unwrap().error.is_some(),
            "duplicate rejected"
        );
        s.prompt.as_mut().unwrap().input.set("Laivyba");
        s.prompt_confirm();
        assert!(s.prompt.is_none());
        assert_eq!(s.bookmarks.current_list().unwrap().name, "Laivyba");
    }

    #[test]
    fn rename_list_prompt_starts_with_the_current_name() {
        let mut s = with_bookmarks();
        s.bookmark_request(Request::RenameList);
        assert_eq!(s.prompt.as_ref().unwrap().input.text(), "VOLMET");
        s.prompt.as_mut().unwrap().input.set("Aviation");
        s.prompt_confirm();
        assert_eq!(s.bookmarks.list_names(), ["Aviation"]);
    }

    #[test]
    fn list_picker_selects_a_list() {
        let mut s = with_bookmarks();
        s.bookmarks.create_list("Ships").unwrap();
        s.bookmarks.select_list(0);
        s.bookmark_request(Request::PickList);
        assert_eq!(s.popup.as_ref().unwrap().items, ["VOLMET", "Ships"]);
        s.popup_down();
        s.popup_confirm();
        assert_eq!(s.bookmarks.current_list().unwrap().name, "Ships");
    }

    #[test]
    fn strip_items_carry_on_air_state_from_utc() {
        let mut s = with_bookmarks();
        s.utc = UtcNow {
            hhmm: 1300,
            weekday: 1,
        };
        let items = s.strip_items();
        let rostov = items.iter().find(|i| i.label == "Rostov").unwrap();
        assert!(!rostov.online, "0000-1200 is off air at 13:00");
        assert!(items.iter().find(|i| i.label == "Beacon").unwrap().online);
        s.utc.hhmm = 1000;
        assert!(
            s.strip_items()
                .iter()
                .find(|i| i.label == "Rostov")
                .unwrap()
                .online
        );
    }

    #[test]
    fn bookmark_view_settings_round_trip_through_config() {
        let mut s = with_bookmarks();
        s.bookmarks.create_list("Ships").unwrap();
        s.bookmarks.strip_rows = 6;
        let cfg = s.to_config();
        let mut s2 = with_bookmarks();
        s2.bookmarks.create_list("Ships").unwrap();
        s2.bookmarks.select_list(0);
        s2.apply_config(&cfg);
        assert_eq!(s2.bookmarks.current_list().unwrap().name, "Ships");
        assert_eq!(s2.bookmarks.strip_rows, 6);
    }

    #[test]
    fn changes_are_saved_to_the_shared_file() {
        let dir = std::env::temp_dir().join(format!("shellsdr-app-bm-{}", std::process::id()));
        let path = dir.join("bookmarks.json");
        let mut s = with_bookmarks();
        let store = s.bookmarks.store.clone();
        assert!(s.load_bookmarks(StoreFile::new(&path)).is_none());
        // A missing file loads the default store; put ours back and save.
        s.bookmarks.replace_store(store.clone());
        s.bookmarks.create_list("Ships").unwrap();
        assert!(s.sync_bookmarks(false).is_none());
        let (on_disk, _) = crate::bookmarks::model::BookmarkStore::load_or_default(&path);
        assert_eq!(on_disk.lists.len(), 2);
        assert!(!s.bookmarks.is_dirty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_are_running_center_both_on() {
        let s = AppState::default();
        assert!(s.spectrum_on && s.waterfall_on);
        assert_eq!(s.tune_mode, TuneMode::Center);
        assert_eq!(s.run, RunState::Stopped);
    }

    #[test]
    fn audio_field_opens_popup_and_confirm_sets_vfo_device() {
        use crate::ui::nav::Focus;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.radio
            .set_audio_options(vec![audio_opt("Speakers"), audio_opt("USB Out")]);
        s.nav.focus = Focus::Radio;
        s.radio.select_field(RadioField::Audio);
        assert_eq!(s.radio.field(), RadioField::Audio);
        s.open_popup_for_focus();
        let p = s.popup.as_ref().expect("audio popup opened");
        assert_eq!(p.items, vec!["Default", "Speakers", "USB Out"]);
        // Select "USB Out" (index 2) and confirm.
        s.popup_down();
        s.popup_down();
        s.popup_confirm();
        assert!(s.popup.is_none());
        assert_eq!(s.radio.active().audio_device.as_deref(), Some("USB Out"));
    }

    #[test]
    fn source_device_settings_persist_and_restore() {
        use crate::source::DeviceInfo;
        let mut s = AppState::with_mock_source();
        s.source.set_devices(vec![
            DeviceInfo {
                label: "A".into(),
                args: "driver=mock,mode=ST".into(),
            },
            DeviceInfo {
                label: "B".into(),
                args: "driver=mock,mode=MA".into(),
            },
        ]);
        // Select device B and probe it so a view exists.
        s.source.select_device(1, AppState::probe_device_caps);
        // Change its sample rate + AGC.
        if let Some(v) = &mut s.source.soapy {
            let i = v.sample_rates.iter().position(|r| *r == 250_000.0).unwrap();
            v.set_sample_rate_idx(i);
            v.agc_on = true;
        }

        let cfg = s.to_config();
        assert_eq!(cfg.device_args.as_deref(), Some("driver=mock,mode=MA"));
        assert!(
            cfg.source_devices.contains_key("driver=mock,mode=MA"),
            "current device's settings saved under its args"
        );

        // Fresh state restores device B selected with its saved settings.
        let mut s2 = AppState::with_mock_source();
        s2.source.set_devices(vec![
            DeviceInfo {
                label: "A".into(),
                args: "driver=mock,mode=ST".into(),
            },
            DeviceInfo {
                label: "B".into(),
                args: "driver=mock,mode=MA".into(),
            },
        ]);
        s2.apply_config(&cfg);
        assert_eq!(s2.source.device_idx, 1, "device B reselected");
        assert_eq!(
            s2.source.selected_sample_rate(),
            Some(250_000.0),
            "saved sample rate restored"
        );
        assert!(s2.source.agc_on(), "saved AGC restored");
    }

    #[test]
    fn to_config_preserves_other_devices_settings() {
        use crate::config::{Config, DeviceSettings};
        // A config already holding settings for a device we did NOT select
        // this session must survive a save.
        let mut s = AppState::with_mock_source();
        s.source.set_devices(vec![crate::source::DeviceInfo {
            label: "A".into(),
            args: "driver=mock,mode=ST".into(),
        }]);
        s.source.select_device(0, AppState::probe_device_caps);
        let mut source_devices = std::collections::BTreeMap::new();
        source_devices.insert(
            "driver=other".to_string(),
            DeviceSettings {
                sample_rate_hz: Some(1_000_000.0),
                ..Default::default()
            },
        );
        let cfg = Config {
            source_devices,
            ..Default::default()
        };
        s.apply_config(&cfg);
        let out = s.to_config();
        assert!(
            out.source_devices.contains_key("driver=other"),
            "other device's settings preserved across save"
        );
        assert!(
            out.source_devices.contains_key("driver=mock,mode=ST"),
            "current device also saved"
        );
    }

    #[test]
    fn source_antenna_popup_lists_ports_and_left_right_cycles() {
        use crate::source::model::SourceField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Source;
        while s.source.field() != SourceField::Antenna {
            s.source.field_next();
        }
        s.open_popup_for_focus();
        let p = s.popup.as_ref().expect("antenna popup opened");
        assert_eq!(
            p.items,
            vec!["Tuner 1 50 ohm", "Tuner 1 Hi-Z", "Tuner 2 50 ohm"]
        );
        s.popup_down();
        s.popup_down();
        s.popup_confirm();
        assert_eq!(
            s.source.selected_antenna().as_deref(),
            Some("Tuner 2 50 ohm")
        );
        // Quick-cycle wraps back to the first port.
        s.source_adjust(1);
        assert_eq!(
            s.source.selected_antenna().as_deref(),
            Some("Tuner 1 50 ohm")
        );
    }

    #[test]
    fn source_device_field_opens_popup_of_device_labels() {
        use crate::source::model::SourceField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.source.set_devices(vec![
            crate::source::DeviceInfo {
                label: "RSPduo ST".into(),
                args: "driver=sdrplay,mode=ST".into(),
            },
            crate::source::DeviceInfo {
                label: "RSPduo Master".into(),
                args: "driver=sdrplay,mode=MA".into(),
            },
        ]);
        s.nav.focus = Focus::Source;
        s.source.field_next(); // Kind -> Device
        assert_eq!(s.source.field(), SourceField::Device);
        s.open_popup_for_focus();
        let p = s.popup.as_ref().expect("device popup opened");
        assert_eq!(p.items, vec!["RSPduo ST", "RSPduo Master"]);
    }

    #[test]
    fn confirming_device_popup_selects_and_reprobes() {
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.source.set_devices(vec![
            crate::source::DeviceInfo {
                label: "A".into(),
                args: "driver=mock,mode=ST".into(),
            },
            crate::source::DeviceInfo {
                label: "B".into(),
                args: "driver=mock,mode=MA".into(),
            },
        ]);
        s.nav.focus = Focus::Source;
        s.source.field_next(); // Device
        s.open_popup_for_focus();
        s.popup_down(); // select index 1 (B)
        s.popup_confirm();
        assert!(s.popup.is_none());
        assert_eq!(s.source.device_idx, 1, "device selection applied");
    }

    #[test]
    fn source_samplerate_popup_lists_rates_and_applies() {
        use crate::source::model::SourceField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Source;
        // Kind -> Device -> SampleRate.
        s.source.field_next();
        s.source.field_next();
        assert_eq!(s.source.field(), SourceField::SampleRate);
        s.open_popup_for_focus();
        {
            let p = s.popup.as_ref().expect("sample-rate popup opened");
            assert_eq!(
                p.items.len(),
                s.source.soapy.as_ref().unwrap().sample_rates.len()
            );
        }
        // Move the cursor and confirm; the chosen index becomes the selection.
        s.popup_down();
        let want = s.popup.as_ref().unwrap().cursor;
        s.popup_confirm();
        assert_eq!(s.source.soapy.as_ref().unwrap().sample_rate_idx, want);
    }

    #[test]
    fn source_decimation_popup_applies_and_narrows_the_display_span() {
        use crate::source::model::SourceField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Source;
        s.source.select_field(SourceField::Decimation);
        assert_eq!(s.display_span_hz(), 2_000_000.0);
        s.open_popup_for_focus();
        {
            let p = s.popup.as_ref().expect("decimation popup opened");
            assert_eq!(p.items[0], "None");
            assert_eq!(p.items[3], "8x");
        }
        for _ in 0..3 {
            s.popup_down();
        }
        s.popup_confirm();
        assert_eq!(s.source.decimation(), 8);
        assert_eq!(s.display_span_hz(), 250_000.0, "span is the decimated rate");
        // Persisted globally (SDR++ `conf["decimation"]`) and restored.
        let cfg = s.to_config();
        assert_eq!(cfg.decimation, 8);
        let mut s2 = AppState::with_mock_source();
        s2.apply_config(&cfg);
        assert_eq!(s2.source.decimation(), 8);
    }

    fn info<'a>(p: &'a crate::record::RecPlan, k: &str) -> Option<&'a str> {
        p.info.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn baseband_plan_uses_center_effective_rate_and_iq() {
        use crate::record::RecKind;
        use crate::record::naming::UtcDateTime;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.center_freq = FreqEntry::new(13_264_000);
        s.source.set_decimation(8);
        s.radio.toggle_rec(RadioField::RecNative);
        s.radio.toggle_rec(RadioField::RecBaseband);
        let plans = s
            .recording_plans(UtcDateTime::from_unix(1_793_886_029))
            .unwrap();
        assert_eq!(plans.len(), 1);
        let p = &plans[0];
        assert_eq!(p.kind, RecKind::Baseband);
        assert_eq!(p.minimize, None);
        assert_eq!(p.name, "baseband_13264000Hz_13-40-29_05-11-2026");
        assert_eq!(
            (p.channels, p.sample_rate),
            (2, 250_000),
            "I/Q at 2 MHz / 8"
        );
        assert_eq!(info(p, "center_freq"), Some("13264000"));
        assert_eq!(info(p, "rec_type"), Some("baseband"));
        assert_eq!(info(p, "minimized"), Some("0"));
        assert_eq!(info(p, "vfo_type"), Some("Voice"));
        assert_eq!(info(p, "decimation"), Some("8"));
        assert_eq!(info(p, "source_sample_rate"), Some("2000000"));
        assert_eq!(info(p, "start_time"), Some("2026-11-05T13:40:29Z"));
        assert_eq!(info(p, "timezone"), Some("UTC"));
        assert_eq!(info(p, "format_version"), Some("2"));
    }

    #[test]
    fn audio_plan_uses_the_vfo_frequency_and_48k_mono() {
        use crate::record::naming::UtcDateTime;
        let s = AppState::with_mock_source();
        let plans = s
            .recording_plans(UtcDateTime::from_unix(1_793_886_029))
            .unwrap();
        assert_eq!(plans.len(), 1, "Native only by default");
        let p = &plans[0];
        assert_eq!(p.name, "audio_100000000Hz_13-40-29_05-11-2026");
        assert_eq!((p.channels, p.sample_rate), (1, 48_000));
        assert_eq!(info(p, "mode"), Some("NFM"));
        assert_eq!(info(p, "vfo_name"), Some("Master"));
        assert_eq!(info(p, "rec_type"), Some("audio"));
        let settings: serde_json::Value =
            serde_json::from_str(info(p, "vfo_settings").unwrap()).unwrap();
        assert_eq!(settings["mode"], "Nfm");
    }

    #[test]
    fn both_outputs_share_the_timestamp_and_get_suffixes_without_t() {
        use crate::record::RecKind;
        use crate::record::naming::UtcDateTime;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.radio.set_rec_template("$f_$h$m$s");
        s.radio.toggle_rec(RadioField::RecBaseband);
        let plans = s
            .recording_plans(UtcDateTime::from_unix(1_793_886_029))
            .unwrap();
        let names: Vec<(RecKind, &str)> = plans.iter().map(|p| (p.kind, p.name.as_str())).collect();
        assert_eq!(
            names,
            [
                (RecKind::Native, "100000000Hz_134029_n"),
                (RecKind::Baseband, "100000000Hz_134029_b")
            ]
        );
    }

    #[test]
    fn a_minimized_baseband_is_centered_on_the_vfo_at_a_power_of_two_rate() {
        use crate::record::naming::UtcDateTime;
        use crate::vfo::model::Mode;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.center_freq = FreqEntry::new(7_000_000);
        s.radio.active_mut_freq(7_100_000);
        s.radio.set_mode(Mode::Am); // 10 kHz -> needs 12.5 kHz
        s.radio.toggle_rec(RadioField::RecNative);
        s.radio.toggle_rec(RadioField::RecBaseband);
        s.radio.toggle_rec(RadioField::RecMinimize);
        let p = &s.recording_plans(UtcDateTime::from_unix(0)).unwrap()[0];
        let m = p.minimize.expect("minimized");
        assert_eq!(m.freq_hz, 7_100_000.0);
        assert_eq!(m.min_rate, 12_500.0);
        assert_eq!(p.sample_rate, 15_625, "2 MHz / 128");
        assert_eq!(info(p, "center_freq"), Some("7100000"));
        assert_eq!(info(p, "minimized"), Some("1"));
        assert!(p.name.starts_with("baseband_7100000Hz"), "{}", p.name);
    }

    #[test]
    fn a_minimized_recording_opens_with_its_vfo_tuning() {
        use crate::record::naming::UtcDateTime;
        use crate::record::recorder::{RecorderSpec, start};
        use crate::vfo::model::Mode;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.center_freq = FreqEntry::new(7_000_000);
        s.radio.active_mut_freq(7_074_000);
        s.radio.set_mode(Mode::Usb);
        s.radio.toggle_rec(RadioField::RecNative);
        s.radio.toggle_rec(RadioField::RecBaseband);
        s.radio.toggle_rec(RadioField::RecMinimize);
        let p = s
            .recording_plans(UtcDateTime::from_unix(0))
            .unwrap()
            .remove(0);
        let dir = std::env::temp_dir().join(format!("shellsdr-minrec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.wav", p.name));
        let (sink, handle) = start(RecorderSpec::new(
            path.clone(),
            p.channels,
            p.sample_rate,
            p.info,
        ))
        .unwrap();
        sink.push(vec![0.0; 2 * 1000]);
        drop(sink);
        assert!(handle.finish().is_none());
        // Another state, tuned elsewhere in LSB, opens the file.
        let mut t = AppState::with_mock_source();
        t.radio.set_mode(Mode::Lsb);
        t.toggle_source_kind();
        t.load_file(path.to_str().unwrap()).unwrap();
        assert_eq!(t.center_hz(), 7_074_000, "the file's center is the VFO");
        let v = t.radio.active();
        assert_eq!(v.frequency_hz, 7_074_000);
        assert_eq!(v.mode, Mode::Usb, "the recorded mode");
        assert_eq!(v.bandwidth_hz, 2_800.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tuning_an_unknown_type_bookmark_says_why_and_changes_nothing() {
        use crate::bookmarks::model::{Bookmark, BookmarkList, BookmarkStore};
        let mut s = AppState::default();
        let mut l = BookmarkList::new("Ships");
        l.bookmarks.push(Bookmark {
            name: "NAVTEX 518".into(),
            frequency_hz: 156_525_000,
            vfo_type: "NAVTEX".into(),
            ..Bookmark::default()
        });
        s.bookmarks.replace_store(BookmarkStore {
            lists: vec![l],
            ..BookmarkStore::default()
        });
        let before = s.radio.active().clone();
        s.tune_bookmark(0, 0);
        assert_eq!(s.radio.active(), &before, "nothing tuned");
        assert_eq!(s.popup_target, Some(PopupTarget::Notice));
        assert!(
            s.popup
                .as_ref()
                .unwrap()
                .title
                .contains("not in this build")
        );
        let items = s.strip_items();
        assert_eq!(items[0].label, "?NAVTEX 518");
        assert!(!items[0].online, "drawn dim");
    }

    #[test]
    fn dsc_records_only_the_baseband_its_messages_are_always_on() {
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.radio.set_type(crate::vfo::types::DSC);
        s.radio.toggle_rec(RadioField::RecNative);
        assert!(s.radio.active().rec.native, "Native cannot be switched off");
        let err = s
            .recording_plans(crate::record::naming::UtcDateTime::from_unix(0))
            .unwrap_err();
        assert!(err.contains("Check Baseband"), "{err}");
        s.radio.toggle_rec(RadioField::RecBaseband);
        let plans = s
            .recording_plans(crate::record::naming::UtcDateTime::from_unix(0))
            .unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].kind, crate::record::RecKind::Baseband);
    }

    #[test]
    fn nothing_checked_cannot_record() {
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.radio.toggle_rec(RadioField::RecNative);
        let err = s
            .recording_plans(crate::record::naming::UtcDateTime::from_unix(0))
            .unwrap_err();
        assert!(err.contains("Native or Baseband"), "{err}");
    }

    #[test]
    fn stopping_the_source_switches_recording_off() {
        let mut s = AppState::with_mock_source();
        s.toggle_run();
        s.toggle_recording();
        assert!(s.radio.active().recording);
        s.toggle_run();
        assert!(!s.radio.active().recording, "Stop ends the recording");
        s.toggle_run();
        assert!(!s.radio.active().recording, "and Start does not resume it");
    }

    #[test]
    fn each_vfo_type_has_its_own_recordings_folder() {
        use crate::vfo::types::{DSC, VOICE};
        let mut s = AppState::with_mock_source();
        s.rec_dir = "/data/all".to_string();
        assert_eq!(s.radio.active().vfo_type, VOICE);
        s.open_rec_prompt(PromptTarget::RecDir);
        s.prompt.as_mut().unwrap().input.set("/data/voice");
        s.prompt_confirm();
        assert_eq!(s.active_rec_dir(), "/data/voice");
        assert_eq!(
            s.rec_dir_of(DSC),
            "/data/all",
            "other types keep the common one"
        );
        s.rec_dirs.insert(DSC.to_string(), "/data/dsc".to_string());
        let cfg = s.to_config();
        let mut s2 = AppState::with_mock_source();
        s2.apply_config(&cfg);
        assert_eq!(s2.rec_dir_of(VOICE), "/data/voice");
        assert_eq!(s2.rec_dir_of(DSC), "/data/dsc");
        assert_eq!(s2.rec_dir_of(crate::vfo::types::FAX), "/data/all");
    }

    #[test]
    fn recorder_prompts_set_template_and_folder_and_persist() {
        let mut s = AppState::with_mock_source();
        s.open_rec_prompt(PromptTarget::RecDir);
        assert!(s.text_capture());
        s.prompt.as_mut().unwrap().input.set("  ");
        s.prompt_confirm();
        assert!(
            s.prompt.as_ref().unwrap().error.is_some(),
            "empty folder refused"
        );
        s.prompt.as_mut().unwrap().input.set("/data/rec");
        s.prompt_confirm();
        assert!(s.prompt.is_none());
        s.open_rec_prompt(PromptTarget::RecTemplate);
        s.prompt.as_mut().unwrap().input.set("$n_$t");
        s.prompt_confirm();
        s.radio
            .toggle_rec(crate::vfo::radio::RadioField::RecBaseband);
        s.radio
            .toggle_rec(crate::vfo::radio::RadioField::RecMinimize);
        let cfg = s.to_config();
        let mut s2 = AppState::with_mock_source();
        s2.apply_config(&cfg);
        assert_eq!(s2.active_rec_dir(), "/data/rec");
        assert_eq!(s2.radio.active().rec_template, "$n_$t");
        let rec = s2.radio.active().rec;
        assert!(
            rec.native && rec.baseband && rec.minimize,
            "choices persist"
        );
        assert!(!s2.radio.active().recording, "recording is never restored");
    }

    #[test]
    fn mode_field_opens_popup_listing_all_modes() {
        use crate::ui::nav::Focus;
        use crate::vfo::model::Mode;
        use crate::vfo::radio::RadioField;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        s.radio.field_next();
        assert_eq!(s.radio.field(), RadioField::Type);
        s.radio.field_next();
        assert_eq!(s.radio.field(), RadioField::MODE);
        s.open_popup_for_focus();
        let p = s.popup.as_ref().expect("mode popup opened");
        assert_eq!(p.items.len(), Mode::ALL.len(), "lists every mode");
        assert_eq!(p.items[0], "NFM");
        assert_eq!(p.cursor, 0, "opens on current mode (NFM)");
    }

    #[test]
    fn popup_confirm_applies_selected_mode() {
        use crate::ui::nav::Focus;
        use crate::vfo::model::Mode;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        s.radio.field_next(); // Type
        s.radio.field_next(); // Mode
        s.open_popup_for_focus();
        s.popup_down(); // WFM
        s.popup_down(); // AM
        s.popup_confirm();
        assert!(s.popup.is_none(), "popup closes on confirm");
        assert_eq!(s.radio.active().mode, Mode::Am, "selected mode applied");
    }

    #[test]
    fn popup_cancel_leaves_mode_unchanged() {
        use crate::ui::nav::Focus;
        use crate::vfo::model::Mode;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Radio;
        s.radio.field_next(); // Type
        s.radio.field_next(); // Mode
        let before = s.radio.active().mode;
        assert_eq!(before, Mode::Nfm);
        s.open_popup_for_focus();
        s.popup_down();
        s.popup_cancel();
        assert!(s.popup.is_none(), "popup closes on cancel");
        assert_eq!(s.radio.active().mode, before, "mode unchanged on cancel");
    }

    #[test]
    fn toggles_flip_their_fields() {
        let mut s = AppState::default();
        s.toggle_spectrum();
        assert!(!s.spectrum_on);
        s.toggle_waterfall();
        assert!(!s.waterfall_on);
        s.toggle_tune_mode();
        assert_eq!(s.tune_mode, TuneMode::Free);
        s.toggle_run();
        assert_eq!(s.run, RunState::Running);
    }

    #[test]
    fn fft_branch_gating_via_toggles() {
        let mut s = AppState::default();
        assert!(s.spectrum_on || s.waterfall_on);
        s.spectrum_on = false;
        assert!(s.waterfall_on, "waterfall still on");
        s.waterfall_on = false;
        assert!(
            !(s.spectrum_on || s.waterfall_on),
            "both off: FFT can be disabled"
        );
    }

    #[test]
    fn select_and_report_theme_name_for_persistence() {
        let mut s = AppState::default();
        // Default on startup.
        assert_eq!(s.current_theme_name(), "Default");
        // Restoring a known theme by name succeeds and is reflected.
        assert!(s.select_theme("Retro terminal"));
        assert_eq!(s.current_theme_name(), "Retro terminal");
        // An unknown name does not change the selection.
        assert!(!s.select_theme("Nonexistent"));
        assert_eq!(s.current_theme_name(), "Retro terminal");
    }

    #[test]
    fn theme_persistence_round_trip_through_config() {
        // Simulate: user cycles to retro, we save its name, reload applies it.
        let mut s = AppState::default();
        s.cycle_theme();
        let saved = s.current_theme_name();
        let cfg = crate::config::Config {
            theme: Some(saved.clone()),
            ..Default::default()
        };

        // Fresh state + the saved config restores the same theme.
        let mut s2 = AppState::default();
        if let Some(name) = cfg.theme.as_deref() {
            s2.select_theme(name);
        }
        assert_eq!(s2.current_theme_name(), saved);
    }

    #[test]
    fn center_frequency_edits_are_pending_until_enter() {
        use crate::ui::global::GlobalField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Global;
        // Navigate the global cursor to the Frequency field.
        while s.global_field != GlobalField::Frequency {
            s.global_down();
        }
        let start = s.center_hz();
        s.global_enter(); // enter edit
        assert!(s.global_editing);
        s.global_up(); // change a digit (pending)
        assert_eq!(s.center_hz(), start, "center not applied until ENTER");
        s.global_enter(); // commit
        assert!(!s.global_editing);
        assert_ne!(s.center_hz(), start, "center applied on ENTER");
    }

    #[test]
    fn changing_center_preserves_vfo_offset() {
        use crate::ui::global::GlobalField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();

        // Put the Master VFO 1 MHz above the 100 MHz center via the Radio
        // editing flow: focus Radio, edit Frequency, type it up, commit.
        s.nav.focus = Focus::Radio;
        s.radio.toggle_edit(0, 2_000_000_000); // edit Frequency
        // Move the digit cursor to the MHz place and increment once (+1 MHz).
        s.radio.freq_mut().cursor_left(); // 1e4
        s.radio.freq_mut().cursor_left(); // 1e5
        s.radio.freq_mut().cursor_left(); // 1e6
        s.radio.freq_mut().increment(); // +1 MHz -> 101 MHz
        s.radio.toggle_edit(0, 2_000_000_000); // commit
        assert_eq!(s.radio.active().frequency_hz, 101_000_000);

        // Now move the center up 1 MHz via the global flow; the VFO keeps its
        // +1 MHz offset -> 102 MHz.
        s.nav.focus = Focus::Global;
        while s.global_field != GlobalField::Frequency {
            s.global_down();
        }
        s.global_enter();
        s.center_freq.cursor_left(); // 1e4
        s.center_freq.cursor_left(); // 1e5
        s.center_freq.cursor_left(); // 1e6
        s.global_up(); // +1 MHz -> center 101 MHz
        s.global_enter(); // commit

        assert_eq!(s.center_hz(), 101_000_000);
        assert_eq!(
            s.radio.active().frequency_hz,
            102_000_000,
            "VFO kept its +1 MHz offset"
        );
    }

    #[test]
    fn center_edit_cancel_restores() {
        use crate::ui::global::GlobalField;
        use crate::ui::nav::Focus;
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Global;
        while s.global_field != GlobalField::Frequency {
            s.global_down();
        }
        let start = s.center_hz();
        s.global_enter();
        s.global_up();
        s.global_cancel();
        assert!(!s.global_editing);
        assert_eq!(s.center_hz(), start, "cancel restores center");
    }

    #[test]
    fn volume_field_adjusts_with_left_right() {
        use crate::ui::global::GlobalField;
        use crate::ui::nav::Focus;
        let mut s = AppState::default();
        s.nav.focus = Focus::Global;
        // Navigate to the Volume field.
        while s.global_field != GlobalField::Volume {
            s.global_down();
        }
        let start = s.volume; // default 1.0
        s.global_left();
        assert!(
            s.volume < start,
            "left lowers volume ({} -> {})",
            start,
            s.volume
        );
        let lowered = s.volume;
        s.global_right();
        assert!(s.volume > lowered, "right raises volume");
        // Clamped to [0, 1].
        for _ in 0..40 {
            s.global_left();
        }
        assert!(s.volume >= 0.0);
        for _ in 0..40 {
            s.global_right();
        }
        assert!(s.volume <= 1.0);
    }

    #[test]
    fn config_round_trip_restores_center_vfo_and_toggles() {
        use crate::vfo::model::Mode;
        let mut s = AppState::with_mock_source();
        s.center_freq = crate::freq::FreqEntry::new(145_500_000);
        s.spectrum_on = false;
        s.waterfall_on = false;
        s.volume = 0.5;
        s.tune_mode = TuneMode::Free;
        s.radio.set_mode(Mode::Am);
        s.radio.active_mut_freq(7_200_000);
        s.radio.set_active_audio(Some("USB Out".to_string()));

        let cfg = s.to_config();
        let mut s2 = AppState::with_mock_source();
        s2.apply_config(&cfg);
        assert_eq!(s2.center_hz(), 145_500_000);
        assert!(!s2.spectrum_on && !s2.waterfall_on);
        assert_eq!(s2.tune_mode, TuneMode::Free);
        assert!((s2.volume - 0.5).abs() < 1e-6);
        assert_eq!(s2.radio.active().mode, Mode::Am);
        assert_eq!(s2.radio.active().frequency_hz, 7_200_000);
        assert_eq!(
            s2.radio.active().audio_device.as_deref(),
            Some("USB Out"),
            "per-VFO audio device persists"
        );
    }

    #[test]
    fn mute_zeroes_the_engine_volume_and_keeps_the_level() {
        let mut s = AppState {
            volume: 0.6,
            ..Default::default()
        };
        s.toggle_mute();
        assert!(s.muted);
        assert_eq!(s.effective_volume(), 0.0);
        assert!((s.volume - 0.6).abs() < 1e-6, "level kept while muted");
        s.toggle_mute();
        assert!((s.effective_volume() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn a_muted_vfo_zeroes_the_engine_volume_and_enter_toggles_it() {
        use crate::vfo::radio::RadioField;
        let mut s = AppState {
            volume: 0.6,
            ..AppState::default()
        };
        assert!(s.activate_recorder_field(RadioField::Mute));
        assert!(s.radio.active().muted);
        assert_eq!(s.effective_volume(), 0.0);
        assert!(!s.muted, "the global mute is separate");
        s.activate_recorder_field(RadioField::Mute);
        assert!((s.effective_volume() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn mute_and_help_are_global_bar_controls() {
        let mut s = AppState {
            global_field: GlobalField::Volume,
            ..Default::default()
        };
        s.global_down();
        assert_eq!(s.global_field, GlobalField::Mute, "Mute follows Volume");
        s.global_enter();
        assert!(s.muted, "ENTER toggles mute");
        s.global_right();
        assert!(!s.muted, "Left/Right toggle it too");
        s.global_field = GlobalField::Help;
        s.global_enter();
        assert!(s.help.is_some(), "ENTER on [?] Help opens the popup");
    }

    #[test]
    fn panel_stepper_resizes_within_the_range_and_the_screen() {
        use crate::ui::layout::{LEFT_PANEL_MAX_WIDTH, LEFT_PANEL_MIN_WIDTH, LEFT_PANEL_STEP};
        let mut s = AppState {
            global_field: GlobalField::Theme,
            ..Default::default()
        };
        assert_eq!(s.left_panel_width, LEFT_PANEL_MIN_WIDTH, "default");
        s.global_down();
        assert_eq!(s.global_field, GlobalField::Panel, "Panel follows Theme");
        s.global_right();
        assert_eq!(s.left_panel_width, LEFT_PANEL_MIN_WIDTH + LEFT_PANEL_STEP);
        s.global_left();
        s.global_left();
        assert_eq!(s.left_panel_width, LEFT_PANEL_MIN_WIDTH, "stops at min");
        for _ in 0..100 {
            s.resize_left_panel(true);
        }
        assert_eq!(s.left_panel_width, LEFT_PANEL_MAX_WIDTH, "stops at max");
        // On a known narrow screen it grows only as far as fits, and the
        // first narrowing step is visible at once.
        s.left_panel_width = LEFT_PANEL_MIN_WIDTH;
        s.screen_width = 100;
        for _ in 0..100 {
            s.resize_left_panel(true);
        }
        assert_eq!(s.left_panel_width, 60, "100 - 40 for the spectrum side");
        s.left_panel_width = LEFT_PANEL_MAX_WIDTH;
        s.resize_left_panel(false);
        assert_eq!(s.left_panel_width, 60 - LEFT_PANEL_STEP);
    }

    #[test]
    fn panel_width_is_persisted_and_clamped_on_load() {
        let s = AppState {
            left_panel_width: 48,
            ..Default::default()
        };
        let cfg = s.to_config();
        assert_eq!(cfg.left_panel_width, 48);
        let mut s2 = AppState::default();
        s2.apply_config(&cfg);
        assert_eq!(s2.left_panel_width, 48);
        let old: crate::config::Config = toml::from_str("volume = 0.5").unwrap();
        assert_eq!(
            old.left_panel_width,
            crate::ui::layout::LEFT_PANEL_MIN_WIDTH
        );
        let bad: crate::config::Config = toml::from_str("left_panel_width = 5").unwrap();
        s2.apply_config(&bad);
        assert_eq!(s2.left_panel_width, crate::ui::layout::LEFT_PANEL_MIN_WIDTH);
    }

    #[test]
    fn mute_is_persisted() {
        let s = AppState {
            muted: true,
            ..Default::default()
        };
        let cfg = s.to_config();
        assert!(cfg.muted);
        let mut s2 = AppState::default();
        s2.apply_config(&cfg);
        assert!(s2.muted);
        // An old config without the key loads unmuted.
        let old: crate::config::Config = toml::from_str("volume = 0.5").unwrap();
        assert!(!old.muted);
    }

    /// Write a 1 s, 8 kHz I/Q WAV named `name` into a fresh temp folder.
    fn temp_iq_wav(tag: &str, name: &str, comment: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("shellsdr-file-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let f = std::fs::File::create(&path).unwrap();
        let mut w = crate::record::wav::WavWriter::new(f, 2, 8_000).unwrap();
        w.write_f32(&vec![0.1; 16_000]).unwrap();
        w.finish(comment).unwrap();
        path
    }

    #[test]
    fn loading_a_file_tunes_the_center_and_keeps_vfo_offsets() {
        let p = temp_iq_wav("load", "baseband_7100000Hz.wav", "");
        let mut s = AppState::with_mock_source();
        s.center_freq = FreqEntry::new(100_000_000);
        s.radio.active_mut_freq(100_010_000);
        s.toggle_source_kind();
        s.load_file(p.to_str().unwrap()).unwrap();
        assert_eq!(s.center_hz(), 7_100_000, "center from the file name");
        assert_eq!(s.radio.active().frequency_hz, 7_110_000, "offset kept");
        assert_eq!(s.source.selected_sample_rate(), Some(8_000.0));
        assert_eq!(s.display_span_hz(), 8_000.0, "span is the file rate");
        let m = s.source.file.meta.as_ref().unwrap();
        assert_eq!(m.frames, 8_000);
    }

    #[test]
    fn a_bad_file_shows_an_error_and_keeps_the_prompt_open() {
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.open_file_prompt("");
        s.prompt.as_mut().unwrap().input = crate::ui::text_input::TextInput::new("/no/such.wav");
        s.prompt_confirm();
        assert!(s.prompt.as_ref().unwrap().error.is_some());
        assert!(s.source.file.error.is_some());
        assert!(s.source.file.meta.is_none());
    }

    #[test]
    fn the_center_is_locked_to_the_file_while_it_is_the_source() {
        let p = temp_iq_wav("lock", "x.wav", "center_freq=13264000");
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.load_file(p.to_str().unwrap()).unwrap();
        assert_eq!(s.center_hz(), 13_264_000, "ICMT center_freq");
        // Center mode click: the VFO moves, the center does not.
        s.tune_mode = TuneMode::Center;
        s.click_tune(13_265_000.0);
        assert_eq!(s.center_hz(), 13_264_000);
        assert_eq!(s.radio.active().frequency_hz, 13_265_000);
        // Back on SoapySDR the center is free again.
        s.step_source_kind(-1);
        s.click_tune(14_000_000.0);
        assert_eq!(s.center_hz(), 14_000_000);
        // Returning to the file tunes back to it.
        s.toggle_source_kind();
        assert_eq!(s.center_hz(), 13_264_000);
    }

    #[test]
    fn transport_buttons_seek_and_start() {
        use crate::source::model::FileButton;
        let p = temp_iq_wav("transport", "a_1000Hz.wav", "");
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.load_file(p.to_str().unwrap()).unwrap();
        s.source.file.position = 4_000;
        s.press_file_button(FileButton::Back(0));
        assert_eq!(s.source.file.position, 0, "5 s back clamps at 0");
        s.press_file_button(FileButton::Fwd(0));
        assert_eq!(
            s.source.file.position, 8_000,
            "5 s forward clamps at the end"
        );
        s.press_file_button(FileButton::Rewind);
        assert_eq!(s.source.file.position, 0);
        assert_eq!(s.source.file.pending_seek, Some(0));
        s.run = RunState::Stopped;
        s.press_file_button(FileButton::StartStop);
        assert_eq!(s.run, RunState::Running);
        assert!(!s.source.file.fast, "Start plays in real time");
    }

    #[test]
    fn max_starts_fast_toggles_back_and_stop_clears_it() {
        use crate::source::model::FileButton;
        let p = temp_iq_wav("max", "a_1000Hz.wav", "");
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.load_file(p.to_str().unwrap()).unwrap();
        s.run = RunState::Stopped;
        s.press_file_button(FileButton::Max);
        assert_eq!(s.run, RunState::Running, "Max starts a stopped playback");
        assert!(s.source.file.fast);
        s.press_file_button(FileButton::Max);
        assert!(!s.source.file.fast, "again: back to real time");
        assert_eq!(s.run, RunState::Running);
        s.press_file_button(FileButton::Max);
        assert!(s.source.file.fast);
        s.toggle_run();
        assert!(!s.source.file.fast, "Stop ends Max");
        s.source.file.fast = true;
        s.run = RunState::Running;
        s.file_finished();
        assert!(!s.source.file.fast, "the end of the file too");
    }

    #[test]
    fn the_end_of_the_file_stops_and_rewinds() {
        let mut s = AppState::with_mock_source();
        s.run = RunState::Running;
        s.radio.toggle_recording();
        s.source.file.position = 1234;
        s.file_finished();
        assert_eq!(s.run, RunState::Stopped);
        assert_eq!(s.source.file.position, 0);
        assert!(!s.radio.active().recording, "the recording ends too");
    }

    #[test]
    fn keyboard_reaches_every_file_control() {
        use crate::source::model::SourceField;
        use crate::ui::nav::Focus;
        let p = temp_iq_wav("keys", "k_1000Hz.wav", "");
        let mut s = AppState::with_mock_source();
        s.nav.focus = Focus::Source;
        s.toggle_source_kind();
        s.source.field_next();
        assert_eq!(s.source.field(), SourceField::FilePath);
        assert!(s.activate_file_field(), "ENTER on File opens the dialog");
        let b = s.file_browser.as_mut().expect("dialog open");
        b.enter(p.parent().unwrap()).unwrap();
        b.select_name("k_1000Hz.wav");
        let out = b.activate();
        s.file_browser_outcome(out);
        assert!(s.file_browser.is_none(), "a good file closes the dialog");
        assert!(s.source.file.meta.is_some());
        s.source.field_next();
        s.source.field_next();
        assert_eq!(s.source.field(), SourceField::FileBack);
        s.source_adjust(1); // -> [10s]
        s.source.file.position = 8_000;
        assert!(s.activate_file_field());
        assert_eq!(s.source.file.position, 0, "10 s back");
    }

    #[test]
    fn file_source_persists_and_reloads() {
        let p = temp_iq_wav("persist", "p_7000000Hz.wav", "");
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.load_file(p.to_str().unwrap()).unwrap();
        let cfg = s.to_config();
        let text = toml::to_string(&cfg).unwrap();
        let back: crate::config::Config = toml::from_str(&text).unwrap();
        assert_eq!(back.source_kind, crate::source::model::SourceKind::File);
        let mut t = AppState::with_mock_source();
        t.apply_config(&back);
        assert_eq!(t.source.kind, crate::source::model::SourceKind::File);
        assert_eq!(t.source.file.meta, s.source.file.meta);
        assert_eq!(t.center_hz(), 7_000_000);
        // A file that disappeared keeps its path and shows the error.
        std::fs::remove_file(&p).unwrap();
        let mut u = AppState::with_mock_source();
        u.apply_config(&back);
        assert!(u.source.file.error.is_some());
        assert_eq!(u.source.file.path.as_deref(), p.to_str());
    }

    #[test]
    fn home_is_expanded_in_paths() {
        assert_eq!(expand_home("/abs/x.wav"), "/abs/x.wav");
        assert_eq!(expand_home("~user/x.wav"), "~user/x.wav");
        if let Some(u) = directories::UserDirs::new() {
            let home = u.home_dir().to_string_lossy().into_owned();
            assert_eq!(expand_home("~/x.wav"), format!("{home}/x.wav"));
        }
    }

    #[test]
    fn an_unplayable_file_keeps_the_dialog_open_with_the_error() {
        let dir = std::env::temp_dir().join(format!("shellsdr-fb-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad.wav"), b"not a wav").unwrap();
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.rec_dir = dir.to_string_lossy().into_owned();
        s.open_file_browser();
        let b = s.file_browser.as_mut().unwrap();
        b.select_name("bad.wav");
        let out = b.activate();
        s.file_browser_outcome(out);
        assert!(s.file_browser.as_ref().unwrap().error.is_some());
        assert!(s.text_capture(), "letters are type-ahead, not accelerators");
    }

    #[test]
    fn path_button_prompts_and_a_typed_folder_reopens_the_dialog() {
        use crate::ui::file_browser::BrowserButton;
        let p = temp_iq_wav("pathbtn", "z_1000Hz.wav", "");
        let dir = p.parent().unwrap().canonicalize().unwrap();
        let mut s = AppState::with_mock_source();
        s.toggle_source_kind();
        s.load_file(p.to_str().unwrap()).unwrap();
        s.open_file_browser();
        assert_eq!(
            s.file_browser.as_ref().unwrap().selected().unwrap().name,
            "z_1000Hz.wav"
        );
        let out = s.file_browser.as_mut().unwrap().press(BrowserButton::Path);
        s.file_browser_outcome(out);
        assert!(s.file_browser.is_none());
        assert_eq!(s.prompt_target, Some(PromptTarget::FilePath));
        s.prompt.as_mut().unwrap().input =
            crate::ui::text_input::TextInput::new(dir.to_str().unwrap());
        s.prompt_confirm();
        assert!(s.prompt.is_none());
        assert_eq!(s.file_browser.as_ref().unwrap().dir, dir);
    }
}
