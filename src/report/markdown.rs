//! The midnight report (spec 20.6): a finished day as Markdown,
//! `<profile>_report_YYYY-MM-DD.md` in the report folder, with links to the
//! day's pictures and recordings.

use super::model::{Category, Clock, Counters, Event, Level, Period, keys};

fn hms(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn size(n: u64) -> String {
    let f = n as f64;
    if n < 1_000_000 {
        format!("{:.0} kB", f / 1e3)
    } else if n < 1_000_000_000 {
        format!("{:.0} MB", f / 1e6)
    } else {
        format!("{:.1} GB", f / 1e9)
    }
}

fn noise(c: &Counters) -> Option<f64> {
    let n = *c.get(keys::STATION_NOISE_N)?;
    let s = *c.get(keys::STATION_NOISE_SUM)?;
    (n > 0).then(|| s as f64 / n as f64 / 10.0 - 200.0)
}

/// A link target: spaces would end a Markdown link, so the path goes in
/// angle brackets.
fn link(path: &str) -> String {
    format!("<{path}>")
}

/// The Markdown of `day` (its counters `p` and its log `events`).
pub fn day_report(profile: &str, day: &str, p: &Period, events: &[Event], clock: &Clock) -> String {
    let g = |k: &str| p.get(k);
    let gf = |k: &str| p.get(&format!("{}{k}", keys::FILE));
    let zone = if clock.local { "local time" } else { "UTC" };
    let mut md = format!("# ShellSDR report {day}\n\nProfile {profile}, times in {zone}.\n\n");

    md += "## Running\n\n";
    md += &format!(
        "- ShellSDR {}, source {}, recording {}\n- Starts {}, unclean exits {}, source failures {}\n\n",
        hms(g(keys::TIME_APP)),
        hms(g(keys::TIME_SOURCE)),
        hms(g(keys::TIME_REC)),
        g(keys::APP_STARTS),
        g(keys::APP_UNCLEAN),
        g(keys::SOURCE_FAILURES)
    );

    md += "## Work done\n\n| | Live | File |\n|---|---:|---:|\n";
    md += &format!(
        "| DSC messages (distress {}, urgency {}) | {} | {} |\n",
        g("dsc.distress"),
        g("dsc.urgency"),
        g(keys::DSC_MESSAGES),
        gf(keys::DSC_MESSAGES)
    );
    md += &format!(
        "| HFDL messages (emergency {}, caution {}) | {} | {} |\n",
        g("hfdl.emergency"),
        g("hfdl.caution"),
        g(keys::HFDL_MESSAGES),
        gf(keys::HFDL_MESSAGES)
    );
    if let Some(n) = p.max_of(keys::HFDL_AIRCRAFT) {
        md += &format!("| HFDL planes seen | {n} | |\n");
    }
    if let Some(n) = p.max_of(keys::HFDL_STATIONS) {
        md += &format!("| HFDL ground stations received | {n} | |\n");
    }
    md += &format!(
        "| FAX pictures | {} | {} |\n",
        g(keys::FAX_PICTURES),
        gf(keys::FAX_PICTURES)
    );
    md += &format!(
        "| Digital text | {} | {} |\n| Digital pictures | {} | {} |\n",
        size(g(keys::DIGITAL_BYTES)),
        size(gf(keys::DIGITAL_BYTES)),
        g(keys::DIGITAL_PICTURES),
        gf(keys::DIGITAL_PICTURES)
    );
    md += &format!(
        "| Recordings | {} files, {} | |\n| Scheduled jobs | {} run, {} skipped | |\n\n",
        g(keys::REC_FILES),
        size(g(keys::REC_BYTES)),
        g(keys::SCHED_RUNS),
        g(keys::SCHED_SKIPPED)
    );

    if !p.stations.is_empty() {
        md += "## Stations\n\n| Station | Tuned | Output | Noise floor |\n|---|---:|---|---:|\n";
        let mut st: Vec<_> = p.stations.iter().collect();
        st.sort_by_key(|(_, c)| std::cmp::Reverse(c.get(keys::STATION_TIME).copied().unwrap_or(0)));
        for (name, c) in st {
            let get = |k: &str| c.get(k).copied().unwrap_or(0);
            let mut out = Vec::new();
            for (k, unit) in [
                (keys::DSC_MESSAGES, " messages"),
                (keys::HFDL_MESSAGES, " HFDL messages"),
                (keys::FAX_PICTURES, " fax pictures"),
                (keys::DIGITAL_PICTURES, " pictures"),
            ] {
                if get(k) > 0 {
                    out.push(format!("{}{unit}", get(k)));
                }
            }
            if get(keys::DIGITAL_BYTES) > 0 {
                out.push(size(get(keys::DIGITAL_BYTES)));
            }
            md += &format!(
                "| {} | {} | {} | {} |\n",
                name.replace('|', "/"),
                hms(get(keys::STATION_TIME)),
                out.join(", "),
                noise(c).map_or(String::new(), |n| format!("{n:.0} dB"))
            );
        }
        md += "\n";
    }

    md += "## Health\n\n";
    let mut h = Vec::new();
    if let Some(d) = p.max_of(keys::HEALTH_DSP) {
        h.push(format!("DSP max {}%", d / 10));
    }
    h.push(format!("{} IQ blocks dropped", g(keys::HEALTH_DROPS)));
    if let Some(t) = p.max_of(keys::HEALTH_TEMP) {
        h.push(format!("CPU max {:.0} °C", t as f64 / 10.0));
    }
    if let Some(m) = p.max_of(keys::HEALTH_MEM) {
        h.push(format!("memory max {} MB", m / 1024));
    }
    if g(keys::WATCHDOG) > 0 {
        h.push(format!("{} watchdog restarts", g(keys::WATCHDOG)));
    }
    md += &format!("{}\n\n", h.join(", "));

    let pictures: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e.category, Category::Fax | Category::Digital) && e.file.is_some())
        .collect();
    if !pictures.is_empty() {
        md += "## Pictures\n\n";
        for e in pictures {
            let f = e.file.as_deref().expect("filtered");
            md += &format!(
                "{} {}\n\n![{}]({})\n\n",
                clock.time_of_day(e.time),
                e.text,
                e.category.label(),
                link(f)
            );
        }
    }
    let recordings: Vec<&Event> = events
        .iter()
        .filter(|e| e.category == Category::Rec && e.file.is_some())
        .collect();
    if !recordings.is_empty() {
        md += "## Recordings\n\n";
        for e in recordings {
            md += &format!("- {} {}\n", clock.time_of_day(e.time), e.text);
        }
        md += "\n";
    }
    let problems: Vec<&Event> = events
        .iter()
        .filter(|e| e.level >= Level::Warning)
        .collect();
    md += &format!("## Warnings and errors ({})\n\n", problems.len());
    if problems.is_empty() {
        md += "None.\n";
    }
    for e in problems {
        md += &format!(
            "- {} {} {} {}\n",
            clock.time_of_day(e.time),
            e.level.mark(),
            e.category.label(),
            e.text
        );
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::model::ReportDoc;

    #[test]
    fn a_day_reads_as_markdown_with_its_pictures_and_problems() {
        let clock = Clock {
            local: false,
            offset_min: 0,
        };
        let t = 1_795_543_452; // 2026-11-24 18:04:12 UTC
        let at = clock.moment(t);
        let mut d = ReportDoc::default();
        d.count(&at, keys::DSC_MESSAGES, 412);
        d.count(&at, "dsc.distress", 2);
        d.count(&at, keys::HFDL_MESSAGES, 120);
        d.count(&at, "hfdl.emergency", 1);
        d.maximum_apart(&at, keys::HFDL_AIRCRAFT, 41, 41);
        d.count(&at, keys::TIME_APP, 3600);
        d.count_station(&at, "DSC 2", keys::STATION_TIME, 600);
        d.count_station(&at, "DSC 2", keys::DSC_MESSAGES, 380);
        let events = vec![
            Event {
                time: t,
                level: Level::Info,
                category: Category::Fax,
                text: "Picture saved".into(),
                file: Some("/r/fax 1.png".into()),
            },
            Event {
                time: t + 60,
                level: Level::Warning,
                category: Category::Dsc,
                text: "DISTRESS from 314001102".into(),
                file: None,
            },
        ];
        let md = day_report(
            "default",
            "2026-11-24",
            d.day("2026-11-24").unwrap(),
            &events,
            &clock,
        );
        assert!(md.starts_with("# ShellSDR report 2026-11-24\n"));
        assert!(md.contains("| DSC messages (distress 2, urgency 0) | 412 | 0 |"));
        assert!(md.contains("| HFDL messages (emergency 1, caution 0) | 120 | 0 |"));
        assert!(md.contains("| HFDL planes seen | 41 | |"));
        assert!(!md.contains("ground stations received"), "none counted");
        assert!(md.contains("| DSC 2 | 0:10:00 | 380 messages |  |"));
        assert!(md.contains("![FAX](</r/fax 1.png>)"), "{md}");
        assert!(
            md.contains("## Warnings and errors (1)\n\n- 18:05:12 ! DSC DISTRESS from 314001102")
        );
        assert!(md.contains("- ShellSDR 1:00:00"));
    }
}
