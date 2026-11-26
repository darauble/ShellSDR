//! External libraries of VFO types (spec 13.10).
//!
//! A type whose core is an external `.so` (0.2.1: DSC) is compiled in and
//! opens its library at runtime with `dlopen`, so a missing library only
//! makes that type unavailable: ShellSDR itself always starts. Libraries are
//! searched in `$SHELLSDR_LIB_DIR`, `<exe dir>/../lib/shellsdr`, the
//! checkout's `assets/lib/<arch>` when the executable is a development build
//! in its `target/` (`cargo run`), `/usr/local/lib/shellsdr`,
//! `/usr/lib/shellsdr`, in that order.

use std::path::{Path, PathBuf};

/// The environment variable naming an extra library folder (searched first).
pub const LIB_DIR_ENV: &str = "SHELLSDR_LIB_DIR";

/// The library folders to search, in order, for an executable at `exe` and
/// an optional `$SHELLSDR_LIB_DIR` value.
pub fn search_dirs(env_dir: Option<&Path>, exe: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = env_dir.filter(|d| !d.as_os_str().is_empty()) {
        dirs.push(d.to_path_buf());
    }
    if let Some(bin_dir) = exe.and_then(Path::parent) {
        dirs.push(bin_dir.join("..").join("lib").join("shellsdr"));
    }
    if let Some(d) = exe.and_then(|e| checkout_lib_dir(e, Path::new(env!("CARGO_MANIFEST_DIR")))) {
        dirs.push(d);
    }
    dirs.push(PathBuf::from("/usr/local/lib/shellsdr"));
    dirs.push(PathBuf::from("/usr/lib/shellsdr"));
    dirs
}

/// The libraries of the checkout at `manifest_dir` (`assets/lib/x64` or
/// `arm64`, where the packages take them from), for an executable built in
/// its `target/` folder; None for an installed one.
pub fn checkout_lib_dir(exe: &Path, manifest_dir: &Path) -> Option<PathBuf> {
    if !exe.starts_with(manifest_dir.join("target")) {
        return None;
    }
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    Some(manifest_dir.join("assets").join("lib").join(arch))
}

/// The search folders of this process.
pub fn default_search_dirs() -> Vec<PathBuf> {
    let env = std::env::var_os(LIB_DIR_ENV).map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    search_dirs(env.as_deref(), exe.as_deref())
}

/// The first existing `file_name` in `dirs`.
pub fn find_in(dirs: &[PathBuf], file_name: &str) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(file_name)).find(|p| p.is_file())
}

/// Open the library `file_name` (e.g. `libdsc.so`) from the search folders.
/// The error is a short reason for the UI ("libdsc.so not found in ...").
pub fn open(file_name: &str) -> Result<libloading::Library, String> {
    open_in(&default_search_dirs(), file_name)
}

/// As [`open`], searching `dirs`.
pub fn open_in(dirs: &[PathBuf], file_name: &str) -> Result<libloading::Library, String> {
    let Some(path) = find_in(dirs, file_name) else {
        let list: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        return Err(format!("{file_name} not found in {}", list.join(", ")));
    };
    // SAFETY: loading a library runs its initializers; the libraries are
    // ShellSDR's own type libraries from the folders above.
    unsafe { libloading::Library::new(&path) }
        .map_err(|e| format!("cannot load {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_order_is_env_exe_relative_then_system() {
        let dirs = search_dirs(
            Some(Path::new("/opt/x")),
            Some(Path::new("/usr/local/bin/shellsdr")),
        );
        assert_eq!(
            dirs,
            [
                PathBuf::from("/opt/x"),
                PathBuf::from("/usr/local/bin/../lib/shellsdr"),
                PathBuf::from("/usr/local/lib/shellsdr"),
                PathBuf::from("/usr/lib/shellsdr"),
            ]
        );
        assert_eq!(search_dirs(Some(Path::new("")), None).len(), 2);
    }

    #[test]
    fn a_build_in_the_checkout_also_finds_its_assets_libraries() {
        let repo = Path::new("/src/ShellSDR");
        let d = checkout_lib_dir(Path::new("/src/ShellSDR/target/release/shellsdr"), repo).unwrap();
        assert!(d.starts_with("/src/ShellSDR/assets/lib"), "{}", d.display());
        assert_eq!(
            checkout_lib_dir(Path::new("/usr/local/bin/shellsdr"), repo),
            None
        );
        assert_eq!(
            checkout_lib_dir(Path::new("/src/ShellSDR/dist/bin/shellsdr"), repo),
            None,
            "only target/"
        );
    }

    #[test]
    fn a_missing_library_is_a_reason_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("shellsdr-libs-{}", std::process::id()));
        let err = open_in(std::slice::from_ref(&dir), "libnothing.so").unwrap_err();
        assert!(err.contains("libnothing.so not found"), "{err}");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("libbad.so"), b"not an elf").unwrap();
        let err = open_in(std::slice::from_ref(&dir), "libbad.so").unwrap_err();
        assert!(err.contains("cannot load"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
