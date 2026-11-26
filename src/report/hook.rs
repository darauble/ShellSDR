//! The event hook (spec 20.6): a user command run for chosen events, e.g.
//! a phone notification (Simplepush, ntfy). The command runs detached in
//! `sh -c` with the event as arguments (`$1` level, `$2` category, `$3`
//! text) and as environment variables (`SHELLSDR_LEVEL`,
//! `SHELLSDR_CATEGORY`, `SHELLSDR_TEXT`, `SHELLSDR_TIME`, `SHELLSDR_FILE`,
//! `SHELLSDR_PROFILE`). It is killed after 10 s; at most 10 run a minute;
//! a failure is reported back (and never sent through the hook again).

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use super::model::{Category, Event, Level};

/// How long a hook may run.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(10);
/// Hooks run at most this many a minute (the rest are counted).
pub const HOOK_PER_MINUTE: usize = 10;

/// Which events go to the hook (spec 20.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookFilter {
    pub errors: bool,
    pub warnings: bool,
    /// DSC distress and urgency calls.
    pub dsc: bool,
    /// HFDL emergency and caution alerts.
    pub hfdl: bool,
    /// Health warnings (disk, temperature, throttling, the clock, nothing
    /// decoded, the noise floor).
    pub health: bool,
}

impl HookFilter {
    pub fn wants(&self, e: &Event) -> bool {
        match (e.level, e.category) {
            (Level::Error, _) => self.errors,
            (Level::Warning, Category::Dsc) => self.dsc || self.warnings,
            (Level::Warning, Category::Hfdl) => self.hfdl || self.warnings,
            (Level::Warning, Category::Health) => self.health || self.warnings,
            (Level::Warning, _) => self.warnings,
            (Level::Info, _) => false,
        }
    }
}

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Info => "info",
        Level::Warning => "warning",
        Level::Error => "error",
    }
}

/// Runs the hook and reports failures back.
#[derive(Debug)]
pub struct HookRunner {
    starts: Vec<Instant>,
    /// Events not sent because of the rate limit, since the last report.
    held: u64,
    tx: Sender<String>,
    rx: Receiver<String>,
}

impl Default for HookRunner {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self {
            starts: Vec::new(),
            held: 0,
            tx,
            rx,
        }
    }
}

impl HookRunner {
    /// Run `command` for `e` (stamped `stamp`), within the rate limit.
    pub fn run(&mut self, command: &str, profile: &str, e: &Event, stamp: &str) {
        let now = Instant::now();
        self.starts
            .retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        if self.starts.len() >= HOOK_PER_MINUTE {
            self.held += 1;
            return;
        }
        self.starts.push(now);
        let level = level_name(e.level);
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .arg("shellsdr-hook")
            .arg(level)
            .arg(e.category.label())
            .arg(&e.text)
            .env("SHELLSDR_LEVEL", level)
            .env("SHELLSDR_CATEGORY", e.category.label())
            .env("SHELLSDR_TEXT", &e.text)
            .env("SHELLSDR_TIME", stamp)
            .env("SHELLSDR_FILE", e.file.as_deref().unwrap_or(""))
            .env("SHELLSDR_PROFILE", profile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let tx = self.tx.clone();
        let what = e.text.clone();
        match cmd.spawn() {
            Ok(mut child) => {
                std::thread::Builder::new()
                    .name("report-hook".into())
                    .spawn(move || {
                        let t0 = Instant::now();
                        loop {
                            match child.try_wait() {
                                Ok(Some(status)) if status.success() => return,
                                Ok(Some(status)) => {
                                    let mut err = String::new();
                                    if let Some(mut s) = child.stderr.take() {
                                        use std::io::Read;
                                        let _ = s.read_to_string(&mut err);
                                    }
                                    let err = err.lines().last().unwrap_or("").to_string();
                                    let _ = tx.send(format!(
                                        "Hook failed ({status}) for \"{what}\"{}",
                                        if err.is_empty() {
                                            String::new()
                                        } else {
                                            format!(": {err}")
                                        }
                                    ));
                                    return;
                                }
                                Ok(None) if t0.elapsed() >= HOOK_TIMEOUT => {
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    let _ = tx.send(format!(
                                        "Hook killed after {} s for \"{what}\"",
                                        HOOK_TIMEOUT.as_secs()
                                    ));
                                    return;
                                }
                                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                                Err(e) => {
                                    let _ = tx.send(format!("Hook: {e}"));
                                    return;
                                }
                            }
                        }
                    })
                    .ok();
            }
            Err(err) => {
                let _ = self.tx.send(format!("Hook could not start: {err}"));
            }
        }
    }

    /// Failures since the last call, and how many events the rate limit
    /// held back.
    pub fn take_problems(&mut self) -> (Vec<String>, u64) {
        (self.rx.try_iter().collect(), std::mem::take(&mut self.held))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(level: Level, category: Category, text: &str) -> Event {
        Event {
            time: 0,
            level,
            category,
            text: text.into(),
            file: None,
        }
    }

    #[test]
    fn the_filter_picks_levels_and_categories() {
        let f = HookFilter {
            errors: true,
            warnings: false,
            dsc: true,
            hfdl: true,
            health: false,
        };
        assert!(f.wants(&ev(Level::Error, Category::Rec, "x")));
        assert!(f.wants(&ev(Level::Warning, Category::Dsc, "DISTRESS")));
        assert!(!f.wants(&ev(Level::Warning, Category::Health, "disk")));
        assert!(!f.wants(&ev(Level::Warning, Category::Source, "x")));
        assert!(!f.wants(&ev(Level::Info, Category::Dsc, "x")));
        assert!(f.wants(&ev(Level::Warning, Category::Hfdl, "EMERGENCY")));
        assert!(!f.wants(&ev(Level::Info, Category::Hfdl, "WATCH")));
        let off = HookFilter { hfdl: false, ..f };
        assert!(!off.wants(&ev(Level::Warning, Category::Hfdl, "EMERGENCY")));
    }

    fn wait_for(r: &mut HookRunner) -> Vec<String> {
        for _ in 0..50 {
            let (p, _) = r.take_problems();
            if !p.is_empty() {
                return p;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Vec::new()
    }

    #[test]
    fn the_hook_gets_the_event_and_its_failures_come_back() {
        let d = std::env::temp_dir().join(format!("shellsdr-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let out = d.join("out.txt");
        let mut r = HookRunner::default();
        let e = Event {
            file: Some("/r/a.png".into()),
            ..ev(Level::Warning, Category::Dsc, "DISTRESS from 314001102")
        };
        let cmd = format!(
            "printf '%s|%s|%s|%s|%s' \"$1\" \"$2\" \"$3\" \"$SHELLSDR_FILE\" \"$SHELLSDR_PROFILE\" > {}",
            out.display()
        );
        r.run(&cmd, "tuner2", &e, "2026-11-24T18:04:12Z");
        for _ in 0..50 {
            if out.exists() && !std::fs::read_to_string(&out).unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "warning|DSC|DISTRESS from 314001102|/r/a.png|tuner2"
        );
        r.run("echo nope >&2; exit 3", "p", &e, "t");
        let p = wait_for(&mut r);
        assert!(
            p[0].contains("Hook failed") && p[0].contains("nope"),
            "{p:?}"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn more_than_ten_a_minute_are_held_back() {
        let mut r = HookRunner::default();
        let e = ev(Level::Error, Category::App, "x");
        for _ in 0..12 {
            r.run("true", "p", &e, "t");
        }
        let (_, held) = r.take_problems();
        assert_eq!(held, 2);
    }
}
