//! ShellSDR entry point.
//!
//! Parses the CLI, loads the config (restoring persisted settings), launches
//! the TUI, and persists the config on exit.

use clap::Parser;

use shellsdr::app::AppState;
use shellsdr::cli::Cli;
use shellsdr::{config, ui};

fn main() {
    let args = Cli::parse();

    // Route SoapySDR/driver native logging (e.g. sdrplay's `[INFO] devIdx: ...`
    // lines) into the Rust `log` crate so they never hit stdout/stderr and
    // corrupt the ratatui display. We install no `log` logger, so these
    // messages are discarded. Belt-and-suspenders: also cap the native log
    // level via the environment in case a driver bypasses the handler.
    // SAFETY: set before any threads are spawned or SoapySDR is touched.
    unsafe {
        std::env::set_var("SOAPY_SDR_LOG_LEVEL", "0");
    }
    soapysdr::configure_logging();

    #[cfg(not(feature = "gui"))]
    if args.gui {
        eprintln!("error: this ShellSDR was built without the window (cargo feature `gui`)");
        std::process::exit(2);
    }

    // Headless network server (spec 21.2): no TUI, no profile.
    if let Some(addr) = args.serve.clone() {
        let opts = shellsdr::remote::server::ServeOptions {
            addr,
            token: args.token.clone(),
            files: args.serve_file.clone(),
        };
        if let Err(e) = shellsdr::remote::server::run_until_signal(opts) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }

    let base_dir = default_base_config_dir();
    let config_path = match config::resolve_config_path(
        args.config.as_deref(),
        args.config_path.as_deref(),
        &base_dir,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let loaded = config::load(&config_path);
    for w in &loaded.warnings {
        eprintln!("warning: {w}");
    }
    let cfg = loaded.config.clone();

    // Discover themes from the search path (env override, user config dir,
    // dev assets dir, system dirs) plus the embedded built-ins.
    let resource_env = std::env::var_os("SHELLSDR_RESOURCE_DIR").map(std::path::PathBuf::from);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let search_dirs =
        ui::theme_loader::theme_search_dirs(resource_env.as_deref(), Some(&base_dir), &cwd);
    let (registry, theme_errors) = ui::theme_loader::build_registry(&search_dirs);
    for e in &theme_errors {
        eprintln!("warning: theme load: {e}");
    }

    // Launch the TUI, restoring persisted settings (theme, center freq, VFO,
    // toggles, volume). `--test` uses the mock source; else real SoapySDR.
    let mut state = if args.test {
        AppState::with_mock_source()
    } else {
        AppState::with_real_source()
    };
    state.set_theme_registry(registry);
    // Bookmarks are shared by all instances (one file in the config dir),
    // loaded before the per-instance view settings select a list.
    let bookmarks = shellsdr::bookmarks::model::StoreFile::with_legacy(
        config::bookmarks_path(&base_dir),
        config::bookmarks_legacy_path(&base_dir),
    );
    if let Some(w) = state.load_bookmarks(bookmarks) {
        eprintln!("warning: {w}");
    }
    state.apply_config(&cfg);
    if loaded.read_only {
        state.note_file_warning(loaded.warnings.first().cloned());
    }
    // The recording schedule belongs to this instance (next to its config).
    let schedule = shellsdr::schedule::store::schedule_path(&config_path);
    let v2_schedule = shellsdr::schedule::store::v2_schedule_path(&config_path);
    let legacy_schedule = shellsdr::schedule::store::legacy_schedule_path(&config_path);
    if let Some(w) =
        state.load_schedule_with_legacy(schedule, Some(v2_schedule), Some(legacy_schedule))
    {
        eprintln!("warning: {w}");
    }
    // The report (statistics and the event log) of this profile.
    state.open_report(&config_path);

    // Populate the per-VFO audio output list from enumeration (cpal/ALSA PCMs
    // plus PipeWire sinks) so the VFO Audio picker lists real outputs.
    state.set_audio_options(shellsdr::engine::audio::output_options());

    #[cfg(feature = "gui")]
    let run_result = if args.gui {
        shellsdr::gui::run_app(&mut state, base_dir.join("ShellSDR"))
    } else {
        ui::run::run(&mut state)
    };
    #[cfg(not(feature = "gui"))]
    let run_result = ui::run::run(&mut state);

    // Persist current settings on exit (best effort). A profile written by a
    // newer ShellSDR is never overwritten (spec 13.9).
    let out_cfg = state.to_config();
    if loaded.read_only {
        eprintln!(
            "note: {} needs a newer ShellSDR; settings were not saved",
            config_path.display()
        );
    } else if let Err(e) = config::save(&out_cfg, &config_path) {
        eprintln!(
            "warning: could not save config to {}: {e}",
            config_path.display()
        );
    }

    if let Err(e) = run_result {
        eprintln!("ShellSDR exited with error: {e}");
        std::process::exit(1);
    }
}

/// OS config directory (e.g. `~/.config` on Linux), falling back to the
/// current directory if it cannot be determined.
fn default_base_config_dir() -> std::path::PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.config_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}
