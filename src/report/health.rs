//! Health of an unattended receiver (spec 20.3, 20.6): readers for the
//! machine's state (temperature, the Raspberry Pi's throttling, free disk,
//! clock synchronisation, memory) and a monitor that turns readings into
//! warnings when a threshold is crossed and a note when it is back to
//! normal (one event per change, not one per reading).

use std::path::Path;

use super::model::Level;

/// The CPU temperature in °C (`/sys/class/thermal/thermal_zone0`).
pub fn cpu_temp_c() -> Option<f32> {
    let s = std::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp").ok()?;
    s.trim().parse::<f32>().ok().map(|m| m / 1000.0)
}

/// The Raspberry Pi firmware's throttling flags (`get_throttled`), where
/// the board has them.
pub fn throttled() -> Option<u32> {
    let s = std::fs::read_to_string("/sys/devices/platform/soc/soc:firmware/get_throttled").ok()?;
    let t = s.trim().trim_start_matches("0x");
    u32::from_str_radix(t, 16).ok()
}

/// What the current throttling flags mean (bits 0..3: now).
pub fn throttle_text(bits: u32) -> Option<String> {
    let mut why = Vec::new();
    if bits & 0x1 != 0 {
        why.push("under-voltage");
    }
    if bits & 0x2 != 0 {
        why.push("frequency capped");
    }
    if bits & 0x4 != 0 {
        why.push("throttled");
    }
    if bits & 0x8 != 0 {
        why.push("soft temperature limit");
    }
    (!why.is_empty()).then(|| why.join(", "))
}

/// Free bytes on the file system holding `path` (its nearest existing
/// ancestor, so a folder not made yet still answers).
pub fn disk_free(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    // SAFETY: `c` is a valid C string; `st` is written by statvfs.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        Some(st.f_bavail as u64 * st.f_frsize as u64)
    }
}

/// Whether the kernel's clock is synchronised (NTP or another source):
/// `adjtimex` answers `TIME_ERROR` while it is not.
pub fn clock_synced() -> Option<bool> {
    // SAFETY: modes = 0 only reads the kernel's state into `tx`.
    unsafe {
        let mut tx: libc::timex = std::mem::zeroed();
        let state = libc::adjtimex(&mut tx);
        if state < 0 {
            return None;
        }
        Some(state != libc::TIME_ERROR)
    }
}

/// This process's resident memory in kB (`/proc/self/statm`).
pub fn rss_kb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: sysconf has no preconditions.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    Some(pages * page.max(4096) as u64 / 1024)
}

/// Days the free space lasts at `bytes_per_day` (None when nothing is
/// being recorded).
pub fn days_left(free: u64, bytes_per_day: u64) -> Option<f64> {
    (bytes_per_day > 0).then(|| free as f64 / bytes_per_day as f64)
}

/// One minute's readings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    /// DSP load, 0..1+ (the minute's highest), while the source runs.
    pub dsp: Option<f32>,
    /// IQ blocks the driver dropped during the minute.
    pub drops: u64,
    pub temp_c: Option<f32>,
    pub throttled: Option<u32>,
    pub disk_free: Option<u64>,
    pub days_left: Option<f64>,
    pub clock_synced: Option<bool>,
}

/// Thresholds (spec 20.4).
pub const DISK_LOW_BYTES: u64 = 2_000_000_000;
pub const DISK_LOW_DAYS: f64 = 1.0;
pub const TEMP_HIGH_C: f32 = 80.0;
pub const TEMP_OK_C: f32 = 75.0;
pub const DSP_HIGH: f32 = 0.9;

/// What is wrong now, to log each change once.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HealthMonitor {
    disk_low: bool,
    hot: bool,
    throttle: Option<String>,
    dsp_high: bool,
    unsynced: bool,
    /// The clock was looked at (its first answer is logged when bad).
    clock_known: bool,
}

impl HealthMonitor {
    /// The events for a minute's readings: warnings when a threshold is
    /// crossed, a note when it is back to normal, drops every minute.
    pub fn evaluate(&mut self, s: &Sample) -> Vec<(Level, String)> {
        let mut out = Vec::new();
        if let Some(free) = s.disk_free {
            let low = free < DISK_LOW_BYTES || s.days_left.is_some_and(|d| d < DISK_LOW_DAYS);
            if low != self.disk_low {
                self.disk_low = low;
                let gb = free as f64 / 1e9;
                let days = s
                    .days_left
                    .map_or(String::new(), |d| format!(" (~{d:.1} days)"));
                out.push(if low {
                    (Level::Warning, format!("Disk low: {gb:.1} GB free{days}"))
                } else {
                    (
                        Level::Info,
                        format!("Disk space back: {gb:.1} GB free{days}"),
                    )
                });
            }
        }
        if let Some(t) = s.temp_c {
            let hot = if self.hot {
                t > TEMP_OK_C
            } else {
                t > TEMP_HIGH_C
            };
            if hot != self.hot {
                self.hot = hot;
                out.push(if hot {
                    (Level::Warning, format!("CPU at {t:.0}°C"))
                } else {
                    (Level::Info, format!("CPU cooled to {t:.0}°C"))
                });
            }
        }
        if let Some(bits) = s.throttled {
            let now = throttle_text(bits);
            if now != self.throttle {
                out.push(match &now {
                    Some(why) => (Level::Warning, format!("Raspberry Pi: {why}")),
                    None => (Level::Info, "Raspberry Pi: no longer throttled".to_string()),
                });
                self.throttle = now;
            }
        }
        if let Some(d) = s.dsp {
            let high = d > DSP_HIGH;
            if high != self.dsp_high {
                self.dsp_high = high;
                out.push(if high {
                    (
                        Level::Warning,
                        format!(
                            "DSP at {:.0}% for a minute: the CPU cannot keep up",
                            d * 100.0
                        ),
                    )
                } else {
                    (Level::Info, format!("DSP load back to {:.0}%", d * 100.0))
                });
            }
        }
        if s.drops > 0 {
            out.push((
                Level::Warning,
                format!("{} IQ blocks dropped in the last minute", s.drops),
            ));
        }
        if let Some(ok) = s.clock_synced {
            let first = !self.clock_known;
            self.clock_known = true;
            if !ok && (first || !self.unsynced) {
                out.push((
                    Level::Warning,
                    "System clock not synchronised: schedules and DSC times may be wrong"
                        .to_string(),
                ));
            } else if ok && self.unsynced {
                out.push((Level::Info, "System clock synchronised".to_string()));
            }
            self.unsynced = !ok;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_threshold_is_logged_when_crossed_and_when_back() {
        let mut m = HealthMonitor::default();
        let mut s = Sample {
            temp_c: Some(60.0),
            disk_free: Some(50_000_000_000),
            ..Sample::default()
        };
        assert!(m.evaluate(&s).is_empty());
        s.temp_c = Some(82.0);
        let e = m.evaluate(&s);
        assert_eq!(e, [(Level::Warning, "CPU at 82°C".to_string())]);
        assert!(m.evaluate(&s).is_empty(), "once, not every minute");
        s.temp_c = Some(78.0);
        assert!(m.evaluate(&s).is_empty(), "hysteresis: still hot above 75");
        s.temp_c = Some(70.0);
        assert_eq!(m.evaluate(&s)[0].0, Level::Info);
        s.disk_free = Some(1_500_000_000);
        assert!(m.evaluate(&s)[0].1.starts_with("Disk low: 1.5 GB"));
        // Days left at today's rate count too.
        let mut m = HealthMonitor::default();
        let s = Sample {
            disk_free: Some(10_000_000_000),
            days_left: Some(0.5),
            ..Sample::default()
        };
        assert!(m.evaluate(&s)[0].1.contains("~0.5 days"));
    }

    #[test]
    fn throttling_drops_dsp_and_the_clock() {
        assert_eq!(throttle_text(0x5).unwrap(), "under-voltage, throttled");
        assert_eq!(throttle_text(0x50000), None, "only the past: not now");
        let mut m = HealthMonitor::default();
        let s = Sample {
            throttled: Some(0x1),
            dsp: Some(0.95),
            drops: 3,
            clock_synced: Some(false),
            ..Sample::default()
        };
        let e = m.evaluate(&s);
        assert_eq!(e.len(), 4, "{e:?}");
        let again = m.evaluate(&s);
        assert_eq!(again.len(), 1, "only the drops repeat: {again:?}");
        let s = Sample {
            throttled: Some(0),
            dsp: Some(0.3),
            clock_synced: Some(true),
            ..Sample::default()
        };
        let e = m.evaluate(&s);
        assert!(e.iter().all(|(l, _)| *l == Level::Info), "{e:?}");
        assert_eq!(e.len(), 3);
        assert_eq!(days_left(10, 0), None);
        assert_eq!(days_left(10, 5), Some(2.0));
    }

    #[test]
    fn readers_answer_on_this_machine() {
        // Where the machine has them; never panics.
        let _ = cpu_temp_c();
        let _ = throttled();
        assert!(disk_free(Path::new("/tmp/does/not/exist/yet")).is_some());
        assert!(clock_synced().is_some());
        assert!(rss_kb().is_some_and(|k| k > 0));
    }
}
