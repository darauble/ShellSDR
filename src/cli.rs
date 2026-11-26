//! Command-line interface.
//!
//! `--config <name>`, `--config-path <path>` and `--test` (mock mode), parsed with clap.

use std::path::PathBuf;

use clap::Parser;

/// ShellSDR: a terminal SDR receiver and recorder.
#[derive(Debug, Clone, Parser)]
#[command(name = "shellsdr", version, about)]
pub struct Cli {
    /// Named config: uses ~/.config/ShellSDR/<NAME>.toml and
    /// ~/.config/ShellSDR/<NAME>.schedule.json. Defaults to "default".
    #[arg(long, value_name = "NAME", conflicts_with = "config_path")]
    pub config: Option<String>,

    /// Explicit config file path (relative or absolute). The schedule file is
    /// placed next to it as <stem>.schedule.json.
    #[arg(long, value_name = "PATH")]
    pub config_path: Option<PathBuf>,

    /// Run without hardware using a deterministic mock source.
    #[arg(long)]
    pub test: bool,

    /// Run in a graphical window (egui) instead of the terminal: the same
    /// ShellSDR, with the spectrum and the waterfall at pixel resolution.
    #[arg(long)]
    pub gui: bool,

    /// Run headless as a network server for another ShellSDR's Network
    /// source, listening on ADDR (default 127.0.0.1:7373; 0.0.0.0 for every
    /// interface).
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "")]
    pub serve: Option<String>,

    /// The secret a client must give to use the server (--serve).
    #[arg(long, value_name = "TOKEN", requires = "serve")]
    pub token: Option<String>,

    /// Also offer this baseband WAV as an input, played in a loop (--serve;
    /// may be repeated).
    #[arg(long, value_name = "PATH", requires = "serve")]
    pub serve_file: Vec<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_no_args() {
        let cli = Cli::try_parse_from(["shellsdr"]).unwrap();
        assert_eq!(cli.config, None);
        assert_eq!(cli.config_path, None);
        assert!(!cli.test);
    }

    #[test]
    fn parses_config_name() {
        let cli = Cli::try_parse_from(["shellsdr", "--config", "tuner2"]).unwrap();
        assert_eq!(cli.config.as_deref(), Some("tuner2"));
        assert_eq!(cli.config_path, None);
    }

    #[test]
    fn parses_config_path() {
        let cli = Cli::try_parse_from(["shellsdr", "--config-path", "/tmp/a.toml"]).unwrap();
        assert_eq!(
            cli.config_path.as_deref(),
            Some(std::path::Path::new("/tmp/a.toml"))
        );
        assert_eq!(cli.config, None);
    }

    #[test]
    fn config_and_config_path_conflict() {
        let r = Cli::try_parse_from(["shellsdr", "--config", "a", "--config-path", "/tmp/b.toml"]);
        assert!(r.is_err());
    }

    #[test]
    fn parses_serve() {
        let cli = Cli::try_parse_from(["shellsdr", "--serve"]).unwrap();
        assert_eq!(cli.serve.as_deref(), Some(""));
        let cli = Cli::try_parse_from([
            "shellsdr",
            "--serve",
            "0.0.0.0:9000",
            "--token",
            "s",
            "--serve-file",
            "a.wav",
        ])
        .unwrap();
        assert_eq!(cli.serve.as_deref(), Some("0.0.0.0:9000"));
        assert_eq!(cli.token.as_deref(), Some("s"));
        assert_eq!(cli.serve_file.len(), 1);
        assert!(Cli::try_parse_from(["shellsdr", "--token", "s"]).is_err());
    }

    #[test]
    fn parses_test_flag() {
        let cli = Cli::try_parse_from(["shellsdr", "--test"]).unwrap();
        assert!(Cli::try_parse_from(["shellsdr", "--gui"]).unwrap().gui);
        assert!(cli.test);
    }
}
