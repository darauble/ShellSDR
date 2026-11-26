//! Versions of the program and of its persisted files (0.2.0, spec 13.9).
//!
//! Every persisted file (profile TOML, `bookmarks.v2.json`,
//! `<profile>.schedule.v3.json`) carries `min_version`: the lowest ShellSDR
//! version that can read it. A file without it predates 0.2.0 and is
//! migrated; a file that needs a newer build is loaded best effort and never
//! written back (so the newer build's data survives).

/// This build's version (`Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `min_version` this build writes: the version that introduced the
/// current shape of the files.
pub const FILE_MIN_VERSION: &str = "0.2.0";

/// The `min_version` of a profile that selects the Network source (an older
/// build cannot read that kind).
pub const NETWORK_MIN_VERSION: &str = "0.4.0";

/// The version assumed for a file without `min_version`.
pub const LEGACY_VERSION: &str = "0.1.0";

/// A `major.minor.patch` version. Missing parts are 0; anything after a `-`
/// or `+` (pre-release, build) is ignored.
pub fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// How a file's `min_version` relates to this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileVersion {
    /// No `min_version`: written by 0.1.x, needs migration.
    Legacy,
    /// Readable and writable by this build.
    Current,
    /// Needs a newer build (the version it names, or the raw text when it
    /// cannot be parsed): load what is understood, never save.
    Newer(String),
}

/// Classify a file's `min_version` against the running build `build`.
pub fn classify_against(min_version: Option<&str>, build: &str) -> FileVersion {
    let Some(v) = min_version else {
        return FileVersion::Legacy;
    };
    match (parse(v), parse(build)) {
        (Some(file), Some(me)) if file <= me => FileVersion::Current,
        _ => FileVersion::Newer(v.to_string()),
    }
}

/// Classify a file's `min_version` against this build.
pub fn classify(min_version: Option<&str>) -> FileVersion {
    classify_against(min_version, VERSION)
}

/// The warning shown for a file that needs a newer build.
pub fn newer_warning(what: &str, path: &std::path::Path, needs: &str) -> String {
    format!(
        "{what} {} needs ShellSDR {needs} (this is {VERSION}): read-only, changes are not saved",
        path.display()
    )
}

/// Copy a pre-0.2.0 file to `<file>.0.1.0.bak` before it is migrated, unless
/// that backup already exists (the first backup is the original). Errors are
/// returned as a warning text; migration goes on regardless.
pub fn backup_legacy(path: &std::path::Path) -> Option<String> {
    let mut bak = path.as_os_str().to_owned();
    bak.push(format!(".{LEGACY_VERSION}.bak"));
    let bak = std::path::PathBuf::from(bak);
    if bak.exists() {
        return None;
    }
    std::fs::copy(path, &bak).err().map(|e| {
        format!(
            "cannot back up {} to {}: {e}",
            path.display(),
            bak.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_semver_with_missing_parts_and_suffixes() {
        assert_eq!(parse("0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse("1.10"), Some((1, 10, 0)));
        assert_eq!(parse("3"), Some((3, 0, 0)));
        assert_eq!(parse(" 0.2.1-rc1 "), Some((0, 2, 1)));
        assert_eq!(parse("0.2.1+build5"), Some((0, 2, 1)));
        assert_eq!(parse("x.1"), None);
        assert_eq!(parse("1.2.3.4"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn classifies_missing_equal_older_and_newer() {
        assert_eq!(classify_against(None, "0.2.0"), FileVersion::Legacy);
        assert_eq!(
            classify_against(Some("0.2.0"), "0.2.0"),
            FileVersion::Current
        );
        assert_eq!(
            classify_against(Some("0.1.5"), "0.2.0"),
            FileVersion::Current
        );
        assert_eq!(
            classify_against(Some("0.3.0"), "0.2.1"),
            FileVersion::Newer("0.3.0".into())
        );
        assert_eq!(
            classify_against(Some("garbage"), "0.2.0"),
            FileVersion::Newer("garbage".into()),
            "an unreadable version is treated as newer (never overwrite)"
        );
    }

    #[test]
    fn this_build_reads_the_files_it_writes() {
        assert_eq!(classify(Some(FILE_MIN_VERSION)), FileVersion::Current);
    }

    #[test]
    fn backup_is_made_once_and_keeps_the_original() {
        let dir = std::env::temp_dir().join(format!("shellsdr-bak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("default.toml");
        std::fs::write(&p, "original").unwrap();
        assert!(backup_legacy(&p).is_none());
        std::fs::write(&p, "changed").unwrap();
        assert!(backup_legacy(&p).is_none());
        let bak = dir.join("default.toml.0.1.0.bak");
        assert_eq!(std::fs::read_to_string(bak).unwrap(), "original");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
