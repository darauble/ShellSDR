//! Day files: one NDJSON file per local date (seed spec/05-data, STO-02..07).

use super::ndjson;
use crate::dsc::record::Record;
use std::path::{Path, PathBuf};

/// The day file of a record in a message directory: its local date.
pub fn path_for(dir: &Path, r: &Record) -> PathBuf {
    dir.join(crate::dsc::time::day_file_name(r.utc, r.offset))
}

/// Appends a record as one line with a single write (STO-03).
/// Concurrent writers (decoders, other program copies) never interleave
/// inside a line: the file is opened for appending and locked for the write.
pub fn append(dir: &Path, r: &Record) -> Result<PathBuf, String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = path_for(dir, r);
    let mut line = ndjson::to_line(r);
    line.push('\n');
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(err)?;
    f.lock().map_err(err)?;
    let written = f.write_all(line.as_bytes()).and_then(|_| f.flush());
    let _ = f.unlock();
    written.map_err(err)?;
    Ok(path)
}

/// The valid records of a day file, in file order (STO-04). A missing file
/// has none.
pub fn read(path: &Path) -> Vec<Record> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    // A torn or foreign line may not be UTF-8; it costs only that line.
    bytes
        .split(|&b| b == b'\n')
        .filter_map(|l| std::str::from_utf8(l).ok())
        .filter(|l| !l.trim().is_empty())
        .filter_map(ndjson::from_line)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsc::record::TimeSource;

    const SAFETY: [u8; 23] = [
        120, 120, 0, 64, 52, 70, 0, 108, 47, 72, 31, 70, 0, 118, 126, 126, 126, 126, 126, 126, 126,
        117, 101,
    ];

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dsqrust-dayfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn at(local: &str) -> Record {
        // "2026-10-20T23:59:58" in +03:00
        let utc = crate::dsc::time::parse_iso_utc(&format!("{local}Z")).unwrap() - 10800;
        Record::new(SAFETY.to_vec(), utc, 10800, TimeSource::Wallclock)
    }

    #[test]
    fn sc_sto_07_out_of_order_dates_go_to_their_own_files() {
        let d = temp_dir("sto07");
        for t in [
            "2026-10-20T23:59:58",
            "2026-10-21T00:00:01",
            "2026-10-20T23:59:59",
        ] {
            append(&d, &at(t)).unwrap();
        }
        let times = |f: &str| {
            read(&d.join(f))
                .iter()
                .map(|r| crate::dsc::time::iso_local(r.utc, r.offset))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            times("2026-10-20.ndjson"),
            ["2026-10-20T23:59:58+03:00", "2026-10-20T23:59:59+03:00"]
        );
        assert_eq!(times("2026-10-21.ndjson"), ["2026-10-21T00:00:01+03:00"]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn invalid_lines_are_skipped_and_the_rest_loads() {
        let d = temp_dir("skip");
        let p = append(&d, &at("2026-10-20T10:00:00")).unwrap();
        let mut text = std::fs::read_to_string(&p).unwrap();
        text.push_str("{\"sym\":[1,2\n not json\n\n");
        text.push_str(&ndjson::to_line(&at("2026-10-20T11:00:00")));
        text.push('\n');
        std::fs::write(&p, text).unwrap();
        assert_eq!(read(&p).len(), 2);
        assert!(read(&d.join("missing.ndjson")).is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn sc_sto_03_concurrent_appends_never_interleave() {
        let d = temp_dir("sto03");
        let writers: Vec<_> = (0..2)
            .map(|w| {
                let d = d.clone();
                std::thread::spawn(move || {
                    for i in 0..1000 {
                        let mut r = at("2026-10-20T12:00:00");
                        r.source_label = Some(format!("{w}-{i}-{}", "x".repeat(2000)));
                        append(&d, &r).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let text = std::fs::read_to_string(d.join("2026-10-20.ndjson")).unwrap();
        assert_eq!(text.lines().count(), 2000);
        assert!(text.lines().all(|l| ndjson::from_line(l).is_some()));
        std::fs::remove_dir_all(d).unwrap();
    }
}
