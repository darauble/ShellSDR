//! Configuration model and path resolution.
//!
//! Holds the persisted settings. Grows as features land; `#[serde(default)]`
//! keeps older/newer files loadable, and every field has a default so partial
//! files work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::TuneMode;
use crate::vfo::model::{AmAgcMode, Mode};

/// Persisted per-device Source settings, keyed by the device args string in
/// `Config::source_devices`. Values are stored by name/value (not list index)
/// so they survive capability-list reordering across driver versions, matching
/// SDR++'s per-device config.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceSettings {
    /// Selected capture sample rate (Hz).
    pub sample_rate_hz: Option<f64>,
    /// Selected analog filter bandwidth (Hz).
    pub bandwidth_hz: Option<f64>,
    /// Selected antenna port name.
    pub antenna: Option<String>,
    /// Named gain elements and their dB values (e.g. IFGR/RFGR).
    pub gains: Vec<(String, f64)>,
    /// Hardware AGC on/off.
    pub agc: bool,
}

/// Application name used for the default config directory (`~/.config/ShellSDR`).
pub const APP_DIR_NAME: &str = "ShellSDR";
/// The shared bookmarks file name within the config directory (0.2.0).
pub const BOOKMARKS_FILE: &str = "bookmarks.v2.json";
/// The 0.1.x bookmarks file: read once to migrate, never written by 0.2.0
/// (so a 0.1.x instance keeps working on it; spec 13.9).
pub const BOOKMARKS_LEGACY_FILE: &str = "bookmarks.json";

/// Path of the bookmarks file shared by all instances:
/// `<config_dir>/ShellSDR/bookmarks.v2.json` (independent of `--config`/`--config-path`).
pub fn bookmarks_path(base_config_dir: &Path) -> PathBuf {
    base_config_dir.join(APP_DIR_NAME).join(BOOKMARKS_FILE)
}

/// Path of the 0.1.x bookmarks file (migration source).
pub fn bookmarks_legacy_path(base_config_dir: &Path) -> PathBuf {
    base_config_dir
        .join(APP_DIR_NAME)
        .join(BOOKMARKS_LEGACY_FILE)
}

/// Default config file name within the config directory.
pub const DEFAULT_CONFIG_FILE: &str = "default.toml";

/// One VFO in the profile (0.2.0, spec 13.4): the settings common to all
/// its types, plus one settings table per type it has used
/// (`[vfo.Master.types.Voice]`), recording choices included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VfoEntry {
    pub frequency_hz: u64,
    /// The active type.
    pub vfo_type: String,
    pub enabled: bool,
    pub collapsed: bool,
    pub audio_device: Option<String>,
    pub muted: bool,
    pub rec_template: String,
    /// Per type: its settings and its `rec_native` / `rec_baseband` /
    /// `rec_minimize` choices. Types this build lacks are kept as they are.
    pub types: BTreeMap<String, crate::vfo::types::Settings>,
}

impl Default for VfoEntry {
    fn default() -> Self {
        crate::vfo::model::Vfo::master(100_000_000).to_entry()
    }
}

/// Settings for one VFO as 0.1.x stored them (`[[vfos]]`): read only to
/// migrate a legacy profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VfoConfig {
    pub name: String,
    pub frequency_hz: u64,
    pub mode: Mode,
    pub bandwidth_hz: f64,
    pub snap_hz: u64,
    pub cw_tone_hz: f64,
    pub am_agc: AmAgcMode,
    #[serde(default)]
    pub audio_device: Option<String>,
    pub enabled: bool,
    pub collapsed: bool,
    /// Recorder type (Audio / Baseband) and file-name template.
    pub rec_type: crate::record::RecType,
    pub rec_template: String,
    /// This VFO's audio is muted (it still records).
    #[serde(default)]
    pub muted: bool,
}

impl Default for VfoConfig {
    fn default() -> Self {
        Self {
            name: "Master".to_string(),
            frequency_hz: 100_000_000,
            mode: Mode::Nfm,
            bandwidth_hz: Mode::Nfm.default_bandwidth(),
            snap_hz: Mode::Nfm.default_snap_hz(),
            cw_tone_hz: 800.0,
            am_agc: AmAgcMode::default(),
            audio_device: None,
            enabled: true,
            collapsed: false,
            rec_type: crate::record::RecType::Audio,
            rec_template: crate::record::naming::DEFAULT_TEMPLATE.to_string(),
            muted: false,
        }
    }
}

/// Top-level persisted configuration. `#[serde(default)]` keeps old/new files
/// loadable; every field has a default so partial files work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The lowest ShellSDR version that can read this file (spec 13.9).
    /// Missing in files written before 0.2.0, which are then migrated.
    #[serde(default)] // missing = a 0.1.x file (not the struct default)
    pub min_version: Option<String>,
    /// Name of the last-used theme, restored on startup. `None` = default.
    pub theme: Option<String>,
    /// Last source center frequency (Hz).
    pub center_hz: u64,
    /// Center/Free tuning mode.
    pub tune_mode: TuneMode,
    /// Spectrum/waterfall display toggles.
    pub spectrum_on: bool,
    pub waterfall_on: bool,
    /// Master output volume (0.0..=1.0).
    pub volume: f32,
    /// Global mute (the volume level is kept).
    pub muted: bool,
    /// Requested left panel width in columns (clamped on load and per
    /// screen by `ui::layout::left_panel_width`).
    pub left_panel_width: u16,
    /// Args string of the last-selected source device (e.g.
    /// `driver=sdrplay,mode=MA`). Re-resolved against enumeration on startup.
    pub device_args: Option<String>,
    /// Selected source kind (SoapySDR or a baseband File).
    pub source_kind: crate::source::model::SourceKind,
    /// The baseband file of the File source (spec 7.2).
    pub file_path: Option<String>,
    /// Per-device Source settings, keyed by the device args string. The
    /// selected device's entry is restored (and re-applied to the device) on
    /// startup. `BTreeMap` for a stable, diffable TOML order.
    #[serde(default)]
    pub source_devices: BTreeMap<String, DeviceSettings>,
    /// Source decimation ratio (1 = none .. 64), applied to the wideband IQ
    /// right after capture. Global like SDR++'s `conf["decimation"]`.
    pub decimation: u32,
    /// Folder recordings are written to (None = `~/ShellSDR/recordings`):
    /// the folder of every VFO type without its own below.
    pub recordings_dir: Option<String>,
    /// Recordings folders of single VFO types, keyed by the type id (`DSC`,
    /// `FAX`...): each type's recordings can go to their own folder.
    pub recordings_dirs: BTreeMap<String, String>,
    /// VFOs by name (at least the Master). Empty means "use defaults".
    pub vfo: BTreeMap<String, VfoEntry>,
    /// The 0.1.x VFO list, read only to migrate (never written).
    #[serde(skip_serializing)]
    pub vfos: Vec<VfoConfig>,
    /// The Work Area layout and tab (spec 13.5).
    pub work_area: WorkAreaConfig,
    /// Bookmarks module view (selected list, strip rows, sort). The bookmarks
    /// themselves live in the shared `bookmarks.json`.
    pub bookmarks: crate::bookmarks::manager::BookmarkView,
    /// The Report's folder, clock and retention (0.3.0, spec 20.5).
    pub report: crate::report::ReportConfig,
    /// The Network source's server (0.4.0, spec 21.2).
    pub network: NetworkConfig,
    /// Sharing this ShellSDR's screen (0.4.1, spec 22.2).
    pub share: ShareConfig,
    /// The user's location, shared by the decoders (0.4.5; DSC, HFDL).
    pub location: Option<Location>,
}

/// The user's location in decimal degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
}

/// The persisted sharing settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShareConfig {
    pub on: bool,
    pub listen: String,
    pub token: String,
    pub mute_while_viewed: bool,
}

impl Default for ShareConfig {
    fn default() -> Self {
        let v = crate::source::model::ShareView::default();
        Self {
            on: v.on,
            listen: v.listen,
            token: v.token,
            mute_while_viewed: v.mute_while_viewed,
        }
    }
}

/// The persisted Network source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// `host:port` of the server (empty: none yet).
    pub addr: String,
    /// The server's secret (empty: none).
    pub token: String,
    /// Opus bit rate; 0 = 16-bit PCM.
    pub bitrate: u32,
    /// The selected server input.
    pub device_args: Option<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            addr: String::new(),
            token: String::new(),
            bitrate: crate::source::model::NET_DEFAULT_BITRATE,
            device_args: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_version: Some(crate::version::FILE_MIN_VERSION.to_string()),
            theme: None,
            center_hz: 100_000_000,
            tune_mode: TuneMode::Center,
            spectrum_on: true,
            waterfall_on: true,
            volume: 1.0,
            muted: false,
            left_panel_width: crate::ui::layout::LEFT_PANEL_MIN_WIDTH,
            device_args: None,
            source_kind: Default::default(),
            file_path: None,
            source_devices: BTreeMap::new(),
            decimation: 1,
            recordings_dir: None,
            recordings_dirs: BTreeMap::new(),
            vfo: BTreeMap::new(),
            vfos: Vec::new(),
            work_area: WorkAreaConfig::default(),
            bookmarks: Default::default(),
            report: Default::default(),
            network: Default::default(),
            share: Default::default(),
            location: None,
        }
    }
}

/// Resolve the config file path from the CLI's `--config <name>` /
/// `--config-path <path>` and a base config directory (normally the OS config
/// dir).
///
/// Rules:
/// - `path` (`--config-path`) is used verbatim (relative or absolute).
/// - `name` (`--config`) maps to `<config_dir>/ShellSDR/<name>.toml`.
/// - Neither falls back to `<config_dir>/ShellSDR/default.toml`.
///
/// Errors when `name` is empty or contains a path separator.
pub fn resolve_config_path(
    name: Option<&str>,
    path: Option<&Path>,
    base_config_dir: &Path,
) -> Result<PathBuf, String> {
    if let Some(p) = path {
        return Ok(p.to_path_buf());
    }
    let app_dir = base_config_dir.join(APP_DIR_NAME);
    match name {
        None => Ok(app_dir.join(DEFAULT_CONFIG_FILE)),
        Some(n) => {
            let n = n.strip_suffix(".toml").unwrap_or(n);
            if n.is_empty() || n.contains(['/', '\\']) || n == "." || n == ".." {
                return Err(format!(
                    "invalid config name {n:?}: use a plain name, or --config-path for a file path"
                ));
            }
            Ok(app_dir.join(format!("{n}.toml")))
        }
    }
}

/// Serialize a config to a TOML string.
pub fn to_toml(config: &Config) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(config)
}

/// Parse a config from a TOML string. Unknown/missing fields use defaults.
pub fn from_toml(s: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(s)
}

/// A loaded profile: the config, warnings to show, and whether it must not
/// be written back (it needs a newer ShellSDR).
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
    pub read_only: bool,
}

/// Load the profile at `path` (spec 13.9): the default when absent; the
/// default plus a warning when malformed; a pre-0.2.0 file is backed up to
/// `<file>.0.1.0.bak` and migrated; a file needing a newer build loads best
/// effort, read-only.
pub fn load(path: &Path) -> Loaded {
    use crate::version::{FileVersion, backup_legacy, classify, newer_warning};
    let mut warnings = Vec::new();
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            return Loaded {
                config: Config::default(),
                warnings,
                read_only: false,
            };
        }
    };
    let mut config = match from_toml(&text) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!(
                "failed to parse {}: {e}; using defaults",
                path.display()
            ));
            return Loaded {
                config: Config::default(),
                warnings,
                read_only: false,
            };
        }
    };
    let mut read_only = false;
    match classify(config.min_version.as_deref()) {
        FileVersion::Legacy => {
            warnings.extend(backup_legacy(path));
            migrate_legacy(&mut config);
        }
        FileVersion::Current => {}
        FileVersion::Newer(v) => {
            warnings.push(newer_warning("config", path, &v));
            read_only = true;
        }
    }
    Loaded {
        config,
        warnings,
        read_only,
    }
}

/// Bring a pre-0.2.0 config to the current shape (in memory; it is written
/// on the next save): the `[[vfos]]` list becomes `[vfo.<name>]` tables of
/// type Voice, the recorder type becomes the Native / Baseband choices.
pub fn migrate_legacy(config: &mut Config) {
    config.min_version = Some(crate::version::FILE_MIN_VERSION.to_string());
    for old in std::mem::take(&mut config.vfos) {
        let v = crate::vfo::model::Vfo::from_config(&old);
        config
            .vfo
            .entry(v.name.clone())
            .or_insert_with(|| v.to_entry());
    }
}

/// How the Work Area shows the spectrum and the VFO's view (spec 13.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WorkAreaLayout {
    /// One of the two views, chosen by the tab row.
    #[default]
    Tabs,
    /// Both side by side: the VFO view left, the spectrum right.
    Split,
}

/// Which tab is shown in the Tabs layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WorkAreaTab {
    #[default]
    Spectrum,
    Vfo,
}

/// The persisted Work Area state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkAreaConfig {
    pub layout: WorkAreaLayout,
    pub tab: WorkAreaTab,
}

/// Load the config at `path`, or return the default if the file is absent or
/// malformed. A malformed file yields the default plus a warning string.
pub fn load_or_default(path: &Path) -> (Config, Option<String>) {
    let l = load(path);
    let warn = (!l.warnings.is_empty()).then(|| l.warnings.join("; "));
    (l.config, warn)
}

/// Write the config to `path` as TOML, creating parent directories as needed.
pub fn save(config: &Config, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut config = config.clone();
    config.min_version = Some(
        if config.source_kind == crate::source::model::SourceKind::Network {
            crate::version::NETWORK_MIN_VERSION
        } else {
            crate::version::FILE_MIN_VERSION
        }
        .to_string(),
    );
    let s = to_toml(&config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(path, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_uses_app_dir_and_default_file() {
        let base = Path::new("/home/user/.config");
        let got = resolve_config_path(None, None, base).unwrap();
        assert_eq!(got, Path::new("/home/user/.config/ShellSDR/default.toml"));
        assert_eq!(
            bookmarks_path(base),
            Path::new("/home/user/.config/ShellSDR/bookmarks.v2.json")
        );
        assert_eq!(
            bookmarks_legacy_path(base),
            Path::new("/home/user/.config/ShellSDR/bookmarks.json")
        );
    }

    #[test]
    fn config_name_maps_into_app_dir() {
        let base = Path::new("/home/user/.config");
        let got = resolve_config_path(Some("tuner2"), None, base).unwrap();
        assert_eq!(got, Path::new("/home/user/.config/ShellSDR/tuner2.toml"));
        assert_eq!(
            crate::schedule::store::schedule_path(&got),
            Path::new("/home/user/.config/ShellSDR/tuner2.schedule.v3.json")
        );
    }

    #[test]
    fn config_name_tolerates_toml_suffix() {
        let base = Path::new("/home/user/.config");
        let got = resolve_config_path(Some("tuner2.toml"), None, base).unwrap();
        assert_eq!(got, Path::new("/home/user/.config/ShellSDR/tuner2.toml"));
    }

    #[test]
    fn config_name_rejects_paths_and_empty() {
        let base = Path::new("/home/user/.config");
        for bad in ["", "a/b", "../x", "..", ".toml"] {
            assert!(
                resolve_config_path(Some(bad), None, base).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn config_path_is_used_verbatim() {
        let base = Path::new("/home/user/.config");
        let over = Path::new("/tmp/my-instance.toml");
        let got = resolve_config_path(None, Some(over), base).unwrap();
        assert_eq!(got, over);
    }

    #[test]
    fn config_path_accepts_relative_path() {
        let base = Path::new("/home/user/.config");
        let over = Path::new("slave.toml");
        let got = resolve_config_path(None, Some(over), base).unwrap();
        assert_eq!(got, Path::new("slave.toml"));
    }

    #[test]
    fn source_devices_map_round_trips() {
        let mut source_devices = std::collections::BTreeMap::new();
        source_devices.insert(
            "driver=sdrplay,mode=MA".to_string(),
            DeviceSettings {
                sample_rate_hz: Some(250_000.0),
                bandwidth_hz: Some(200_000.0),
                antenna: Some("Tuner 1 50 ohm".into()),
                gains: vec![("IFGR".into(), 40.0), ("RFGR".into(), 3.0)],
                agc: true,
            },
        );
        let cfg = Config {
            device_args: Some("driver=sdrplay,mode=MA".into()),
            source_devices,
            ..Default::default()
        };
        let back = from_toml(&to_toml(&cfg).unwrap()).unwrap();
        assert_eq!(back, cfg, "per-device settings survive TOML round-trip");
    }

    #[test]
    fn old_config_without_source_devices_still_loads() {
        // A file predating source_devices loads with an empty map (serde default).
        let cfg = from_toml("version = 1\ncenter_hz = 100000000").expect("parse");
        assert!(cfg.source_devices.is_empty());
        assert_eq!(cfg.decimation, 1, "no decimation when the key is absent");
    }

    #[test]
    fn config_toml_round_trips() {
        let cfg = Config::default();
        let s = to_toml(&cfg).expect("serialize");
        let back = from_toml(&s).expect("deserialize");
        assert_eq!(cfg, back);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // An empty TOML document should still load via serde(default); it
        // has no min_version (a 0.1.x file).
        let back = from_toml("").expect("deserialize empty");
        assert_eq!(
            back,
            Config {
                min_version: None,
                ..Config::default()
            }
        );
    }

    #[test]
    fn theme_field_round_trips() {
        let cfg = Config {
            theme: Some("Retro terminal".to_string()),
            ..Default::default()
        };
        let back = from_toml(&to_toml(&cfg).unwrap()).unwrap();
        assert_eq!(back.theme.as_deref(), Some("Retro terminal"));
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        // A file that only sets the theme still loads; it has no version.
        let cfg = from_toml("theme = \"Retro terminal\"").expect("parse partial");
        assert_eq!(cfg.theme.as_deref(), Some("Retro terminal"));
        assert_eq!(cfg.min_version, None, "no min_version: a 0.1.x file");
    }

    fn temp_profile(tag: &str, contents: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("shellsdr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.toml");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn a_legacy_profile_is_backed_up_and_migrated() {
        let (dir, path) = temp_profile("legacy", "version = 1\ncenter_hz = 7000000\n");
        let l = load(&path);
        assert!(!l.read_only);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(l.config.center_hz, 7_000_000);
        assert_eq!(
            l.config.min_version.as_deref(),
            Some(crate::version::FILE_MIN_VERSION)
        );
        let bak = dir.join("p.toml.0.1.0.bak");
        assert!(
            std::fs::read_to_string(bak)
                .unwrap()
                .contains("version = 1")
        );
        // Saving writes the new version marker.
        save(&l.config, &path).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("min_version = \"0.2.0\"")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_profile_from_a_newer_build_is_read_only() {
        let (dir, path) = temp_profile("newer", "min_version = \"99.0.0\"\ncenter_hz = 7000000\n");
        let l = load(&path);
        assert!(l.read_only);
        assert_eq!(l.config.center_hz, 7_000_000, "loaded best effort");
        assert!(
            l.warnings[0].contains("needs ShellSDR 99.0.0"),
            "{:?}",
            l.warnings
        );
        assert!(!dir.join("p.toml.0.1.0.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_current_profile_loads_without_warnings_or_backup() {
        let (dir, path) = temp_profile("current", "min_version = \"0.2.0\"\ncenter_hz = 7000000\n");
        let l = load(&path);
        assert!(!l.read_only && l.warnings.is_empty());
        assert!(!dir.join("p.toml.0.1.0.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_round_trips_via_disk() {
        let dir = std::env::temp_dir().join(format!("shellsdr-cfg-{}", std::process::id()));
        let path = dir.join("nested").join("default.toml");
        let cfg = Config {
            theme: Some("Default".to_string()),
            ..Default::default()
        };
        save(&cfg, &path).expect("save creates dirs and writes");
        let (loaded, warn) = load_or_default(&path);
        assert!(warn.is_none());
        assert_eq!(loaded, cfg);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_missing_file_is_default_no_warning() {
        let (cfg, warn) = load_or_default(Path::new("/definitely/not/here.toml"));
        assert_eq!(cfg, Config::default());
        assert!(warn.is_none());
    }

    #[test]
    fn load_malformed_file_warns_and_defaults() {
        let dir = std::env::temp_dir().join(format!("shellsdr-bad-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "this = = not toml").unwrap();
        let (cfg, warn) = load_or_default(&path);
        assert_eq!(cfg, Config::default());
        assert!(warn.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_legacy_vfo_list_becomes_a_voice_vfo_table() {
        let old = r#"
version = 1
[[vfos]]
name = "Master"
frequency_hz = 14200000
mode = "Usb"
bandwidth_hz = 2700.0
snap_hz = 100
cw_tone_hz = 800.0
am_agc = "Audio"
enabled = true
collapsed = false
rec_type = "Baseband"
rec_template = "$f"
muted = true
"#;
        let mut cfg = from_toml(old).unwrap();
        migrate_legacy(&mut cfg);
        assert!(cfg.vfos.is_empty());
        let m = &cfg.vfo["Master"];
        assert_eq!(m.frequency_hz, 14_200_000);
        assert_eq!(m.vfo_type, "Voice");
        assert!(m.muted);
        assert_eq!(m.rec_template, "$f");
        let voice = &m.types["Voice"];
        assert_eq!(voice["mode"], "Usb");
        assert_eq!(voice["bandwidth_hz"], 2700.0);
        assert_eq!(voice["rec_baseband"], true);
        assert_eq!(voice["rec_native"], false);
        // The written TOML has the spec 13.4 tables and no [[vfos]].
        let text = to_toml(&cfg).unwrap();
        assert!(text.contains("[vfo.Master]"), "{text}");
        assert!(text.contains("[vfo.Master.types.Voice]"), "{text}");
        assert!(text.contains("[work_area]"), "{text}");
        assert!(!text.contains("[[vfos]]"));
        assert_eq!(from_toml(&text).unwrap(), cfg, "round trip");
    }

    #[test]
    fn vfo_entries_keep_other_and_unknown_types() {
        use crate::vfo::model::Vfo;
        let mut v = Vfo::master(7_000_000);
        v.switch_type(crate::vfo::types::test_type::ID);
        v.stash.insert(
            "DSC".into(),
            serde_json::json!({"channel": 70})
                .as_object()
                .unwrap()
                .clone(),
        );
        let cfg = Config {
            vfo: [("Master".to_string(), v.to_entry())].into(),
            ..Default::default()
        };
        let back = from_toml(&to_toml(&cfg).unwrap()).unwrap();
        let e = &back.vfo["Master"];
        assert_eq!(e.vfo_type, "Test");
        assert_eq!(e.types["DSC"]["channel"], 70);
        let restored = Vfo::from_entry("Master", e);
        assert_eq!(restored.vfo_type, "Test");
        assert_eq!(restored.to_entry(), v.to_entry());
    }
}
