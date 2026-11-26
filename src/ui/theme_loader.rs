//! Theme discovery and loading from the filesystem, with embedded fallback.
//!
//! Step 3.6a. Themes are loaded from a search path of directories; the first
//! occurrence of a given theme name wins. The embedded built-ins are always
//! included last so the app always has Default and Retro. New `*.json` files
//! dropped into any search directory are picked up at the next start (or when
//! the registry is rebuilt).
//!
//! Search directories, highest priority first:
//!   1. `$SHELLSDR_RESOURCE_DIR/themes` (explicit override, dev/testing)
//!   2. `<os_config_dir>/ShellSDR/themes` (user themes, writable)
//!   3. `assets/themes` relative to the current dir (cargo-run / dev)
//!   4. `/usr/local/share/shellsdr/themes`, `/usr/share/shellsdr/themes`
//!      (system install)
//!   5. embedded built-ins (always)

use std::path::{Path, PathBuf};

use super::theme::{Theme, ThemeRegistry};
use super::theme_spec::ThemeSpec;

/// Compute the ordered theme search directories.
pub fn theme_search_dirs(
    resource_env: Option<&Path>,
    os_config_dir: Option<&Path>,
    cwd: &Path,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(r) = resource_env {
        dirs.push(r.join("themes"));
    }
    if let Some(c) = os_config_dir {
        dirs.push(c.join("ShellSDR").join("themes"));
    }
    dirs.push(cwd.join("assets").join("themes"));
    dirs.push(PathBuf::from("/usr/local/share/shellsdr/themes"));
    dirs.push(PathBuf::from("/usr/share/shellsdr/themes"));
    dirs
}

/// Load every `*.json` theme from `dir` (non-recursive). Invalid files are
/// skipped (with the error returned alongside for optional reporting). Returns
/// (themes, errors).
pub fn load_dir(dir: &Path) -> (Vec<Theme>, Vec<String>) {
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return (themes, errors), // missing dir is not an error
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort(); // deterministic order
    for path in paths {
        match std::fs::read_to_string(&path) {
            Ok(s) => match ThemeSpec::from_json(&s).and_then(|spec| spec.to_theme()) {
                Ok(t) => themes.push(t),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            },
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    (themes, errors)
}

/// Build the theme registry: disk themes from the search dirs first (so a user
/// theme named e.g. "Retro terminal" overrides the built-in of the same name),
/// then the embedded built-ins as the guaranteed fallback. De-duplication by
/// name keeps the first occurrence, so earlier (higher-priority) dirs win.
pub fn build_registry(search_dirs: &[PathBuf]) -> (ThemeRegistry, Vec<String>) {
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    for dir in search_dirs {
        let (mut ts, mut errs) = load_dir(dir);
        themes.append(&mut ts);
        errors.append(&mut errs);
    }
    // Embedded built-ins last: only used if not already provided by a file of
    // the same name.
    themes.push(Theme::builtin_default());
    themes.push(Theme::builtin_retro());
    (ThemeRegistry::from_themes(themes), errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_dir_order_prioritizes_env_then_user() {
        let dirs = theme_search_dirs(
            Some(Path::new("/opt/res")),
            Some(Path::new("/home/u/.config")),
            Path::new("/proj"),
        );
        assert_eq!(dirs[0], PathBuf::from("/opt/res/themes"));
        assert_eq!(dirs[1], PathBuf::from("/home/u/.config/ShellSDR/themes"));
        assert_eq!(dirs[2], PathBuf::from("/proj/assets/themes"));
        assert!(dirs.contains(&PathBuf::from("/usr/share/shellsdr/themes")));
    }

    #[test]
    fn missing_dir_yields_no_themes_no_error() {
        let (themes, errors) = load_dir(Path::new("/definitely/not/here"));
        assert!(themes.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn loads_and_overrides_from_a_temp_dir() {
        // Write a theme file that overrides the built-in "Retro terminal".
        let dir = std::env::temp_dir().join(format!("shellsdr-themes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let custom = r##"{
            "name": "Retro terminal",
            "text": "#ABCDEF", "dim": "#111111", "accent": "#222222",
            "border": "#333333", "border_focused": "#444444",
            "field_idle": "#555555", "field_selected": "#666666",
            "editing_fg": "#000000", "editing_bg": "#777777",
            "off_note": "#888888", "spectrum": "#999999",
            "waterfall": ["#000000", "#FFFFFF"]
        }"##;
        std::fs::write(dir.join("retro.json"), custom).unwrap();

        let (reg, errors) = build_registry(std::slice::from_ref(&dir));
        assert!(errors.is_empty(), "no parse errors: {errors:?}");
        // The user file (loaded before the embedded built-in) wins for the
        // shared name.
        let names = reg.names();
        assert!(names.contains(&"Retro terminal".to_string()));
        assert!(
            names.contains(&"Default".to_string()),
            "embedded Default still present"
        );
        // Find the overridden theme and confirm it's the custom one.
        let mut r = reg;
        assert!(r.select_by_name("Retro terminal"));
        assert_eq!(
            r.current().text,
            ratatui::style::Color::Rgb(0xAB, 0xCD, 0xEF)
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn registry_always_has_builtins_with_empty_search() {
        let (reg, _) = build_registry(&[]);
        let names = reg.names();
        assert!(names.contains(&"Default".to_string()));
        assert!(names.contains(&"Retro terminal".to_string()));
    }

    #[test]
    fn invalid_theme_file_is_reported_not_fatal() {
        let dir = std::env::temp_dir().join(format!("shellsdr-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.json"), "{ not valid json").unwrap();
        let (reg, errors) = build_registry(std::slice::from_ref(&dir));
        assert!(!errors.is_empty(), "the broken file is reported");
        // Built-ins still load.
        assert!(reg.names().contains(&"Default".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
