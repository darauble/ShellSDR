//! Recording file names: SDR++'s recorder template plus a UTC clock.
//!
//! Tokens (SDR++ `RecorderModule::genFileName`): `$t` type (the VFO type's
//! native token, `audio` for Voice, or `baseband`), `$f` frequency as `%.0fHz` (the VFO frequency for audio, the
//! source center for baseband), `$h` `$m` `$s` hour/minute/second, `$d` `$M`
//! `$y` day/month/year, `$r` mode (`IQ` for baseband). ShellSDR adds `$n`, the
//! VFO name (spec 8). Unknown `$x` sequences are kept literally. Times are UTC
//! (the bookmarks clock is UTC too); the ICMT says `timezone=UTC`.

use std::path::{Path, PathBuf};

use super::RecKind;

/// SDR++'s default recorder name template.
pub const DEFAULT_TEMPLATE: &str = "$t_$f_$h-$m-$s_$d-$M-$y";

/// A UTC calendar time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcDateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl UtcDateTime {
    /// From seconds since the Unix epoch (civil-from-days, H. Hinnant).
    pub fn from_unix(secs: u64) -> Self {
        let days = (secs / 86_400) as i64;
        let rem = secs % 86_400;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Self {
            year,
            month,
            day,
            hour: (rem / 3600) as u32,
            minute: (rem / 60 % 60) as u32,
            second: (rem % 60) as u32,
        }
    }

    /// The current UTC time.
    pub fn now() -> Self {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self::from_unix(secs)
    }

    /// ISO 8601, e.g. `2026-11-05T13:40:29Z` (for the ICMT times).
    pub fn iso(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// The values a template can reference.
#[derive(Debug, Clone)]
pub struct NameFields<'a> {
    pub kind: RecKind,
    /// `$t` of a native recording (the type's token, e.g. `audio`).
    pub native_token: &'a str,
    pub freq_hz: f64,
    pub time: UtcDateTime,
    /// Demod mode label (ignored for baseband, which is `IQ`).
    pub mode: &'a str,
    pub vfo_name: &'a str,
}

/// Expand `template` with `f`. Path separators coming from values (a VFO name)
/// are replaced so a value cannot change the directory. A template without
/// `$t` gets `_n` (native) or `_b` (baseband) at the end, so the two outputs
/// of one recording never collide (spec 13.6).
pub fn expand(template: &str, f: &NameFields) -> String {
    let mut out = expand_tokens(template, f);
    if !template.contains("$t") {
        out.push_str(f.kind.suffix());
    }
    out
}

fn expand_tokens(template: &str, f: &NameFields) -> String {
    let clean = |s: &str| s.replace(['/', '\\', '\0'], "_");
    let mut out = String::with_capacity(template.len() + 32);
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let t = &f.time;
        let rep = match chars.peek() {
            Some('t') => Some(match f.kind {
                RecKind::Native => clean(f.native_token),
                RecKind::Baseband => "baseband".to_string(),
            }),
            Some('f') => Some(format!("{:.0}Hz", f.freq_hz)),
            Some('h') => Some(format!("{:02}", t.hour)),
            Some('m') => Some(format!("{:02}", t.minute)),
            Some('s') => Some(format!("{:02}", t.second)),
            Some('d') => Some(format!("{:02}", t.day)),
            Some('M') => Some(format!("{:02}", t.month)),
            Some('y') => Some(format!("{:02}", t.year)),
            Some('r') => Some(match f.kind {
                RecKind::Baseband => "IQ".to_string(),
                RecKind::Native => clean(f.mode),
            }),
            Some('n') => Some(clean(f.vfo_name)),
            _ => None,
        };
        match rep {
            Some(r) => {
                chars.next();
                out.push_str(&r);
            }
            None => out.push('$'),
        }
    }
    out
}

/// `dir/name.wav`, or `dir/name_2.wav`, `_3`, ... when `exists` says the
/// name is taken (two recordings started within the same second).
pub fn unique_path(dir: &Path, name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(format!("{name}.wav"));
    if !exists(&first) {
        return first;
    }
    (2..)
        .map(|i| dir.join(format!("{name}_{i}.wav")))
        .find(|p| !exists(p))
        .expect("an unused name exists")
}

/// The path of part `n` (2, 3, ...) of a recording split at the RIFF size
/// limit or a sample-rate change: `name.wav` -> `name_part2.wav`.
pub fn part_path(first: &Path, n: u32) -> PathBuf {
    let stem = first
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    first.with_file_name(format!("{stem}_part{n}.wav"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_from_unix_matches_known_dates() {
        // Literals from Python's datetime.utcfromtimestamp.
        let t = UtcDateTime::from_unix(1_700_000_000);
        assert_eq!(t.iso(), "2023-11-14T22:13:20Z");
        assert_eq!(UtcDateTime::from_unix(0).iso(), "1970-01-01T00:00:00Z");
        assert_eq!(
            UtcDateTime::from_unix(951_782_400).iso(),
            "2000-02-29T00:00:00Z",
            "leap day"
        );
        assert_eq!(
            UtcDateTime::from_unix(1_793_886_029).iso(),
            "2026-11-05T13:40:29Z"
        );
    }

    fn fields(kind: RecKind) -> NameFields<'static> {
        NameFields {
            kind,
            native_token: "audio",
            freq_hz: 13_264_000.0,
            time: UtcDateTime::from_unix(1_793_886_029),
            mode: "USB",
            vfo_name: "Master",
        }
    }

    #[test]
    fn sdrpp_default_template_expands_like_sdrpp() {
        assert_eq!(
            expand(DEFAULT_TEMPLATE, &fields(RecKind::Baseband)),
            "baseband_13264000Hz_13-40-29_05-11-2026"
        );
        assert_eq!(
            expand(DEFAULT_TEMPLATE, &fields(RecKind::Native)),
            "audio_13264000Hz_13-40-29_05-11-2026"
        );
    }

    #[test]
    fn mode_and_vfo_name_tokens() {
        assert_eq!(
            expand("$r_$n_$t", &fields(RecKind::Native)),
            "USB_Master_audio"
        );
        assert_eq!(expand("$r$t", &fields(RecKind::Baseband)), "IQbaseband");
        let mut f = fields(RecKind::Native);
        f.vfo_name = "a/b";
        assert_eq!(
            expand("$n$t", &f),
            "a_baudio",
            "no directory change via a value"
        );
    }

    #[test]
    fn unknown_tokens_and_trailing_dollar_stay_literal() {
        assert_eq!(expand("x$q$$t", &fields(RecKind::Native)), "x$q$audio");
    }

    #[test]
    fn a_template_without_t_gets_the_kind_suffix() {
        assert_eq!(
            expand("$f_$h-$m-$s", &fields(RecKind::Native)),
            "13264000Hz_13-40-29_n"
        );
        assert_eq!(
            expand("$f_$h-$m-$s", &fields(RecKind::Baseband)),
            "13264000Hz_13-40-29_b"
        );
        assert_eq!(expand("$r_$n", &fields(RecKind::Native)), "USB_Master_n");
        let mut f = fields(RecKind::Native);
        f.native_token = "dsc";
        assert_eq!(expand("$t_$n", &f), "dsc_Master", "$t present: no suffix");
    }

    #[test]
    fn unique_path_appends_a_counter_when_taken() {
        let dir = Path::new("/rec");
        let taken = [PathBuf::from("/rec/a.wav"), PathBuf::from("/rec/a_2.wav")];
        assert_eq!(
            unique_path(dir, "b", |p| taken.iter().any(|t| t == p)),
            dir.join("b.wav")
        );
        assert_eq!(
            unique_path(dir, "a", |p| taken.iter().any(|t| t == p)),
            dir.join("a_3.wav")
        );
    }

    #[test]
    fn part_path_numbers_the_continuation_files() {
        assert_eq!(
            part_path(Path::new("/rec/x_1.wav"), 2),
            PathBuf::from("/rec/x_1_part2.wav")
        );
    }
}
