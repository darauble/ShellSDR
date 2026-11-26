//! The Digital VFO's receiver in the app (spec 16): the decoder on a worker
//! thread, the session (text lines, marks, pictures) for the view, the
//! Markdown recording and F2 Save.

use crossbeam_channel::{Receiver, Sender, TryRecvError, unbounded};
use num_complex::Complex32;
use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::mfsk::decoder::{Config, Decoder, Output, RsidNote, Status};
use crate::mfsk::frontend::Sideband;
use crate::mfsk::markdown::{self, PICTURES, Writer};
use crate::mfsk::rx::Picture;
use crate::record::naming::UtcDateTime;

/// Lines kept in memory (the oldest go first).
pub const MAX_LINES: usize = 50_000;
/// Pictures kept in memory.
pub const MAX_PICTURES: usize = 200;
/// Waterfall rows kept.
pub const SPECTRUM_ROWS: usize = 64;

/// One line of the session.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Text(String),
    /// A mark's text (without the Markdown around it).
    Mark(String),
    /// A picture (its id).
    Picture(u64),
}

/// A received picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Pic {
    pub id: u64,
    pub width: usize,
    pub height: usize,
    pub color: bool,
    pub rgb: Vec<u8>,
    /// When it was complete (Unix s).
    pub time: f64,
    /// Its file, once written.
    pub file: Option<PathBuf>,
}

/// Where and how a recording is named.
#[derive(Debug, Clone, PartialEq)]
pub struct RecNaming {
    pub dir: PathBuf,
    pub template: String,
    pub vfo_name: String,
    pub freq_hz: f64,
    pub mode: String,
}

impl RecNaming {
    /// The recording template expanded at `time` (`$t` = `digital`).
    fn name(&self, time: UtcDateTime) -> String {
        crate::record::naming::expand(
            &self.template,
            &crate::record::naming::NameFields {
                kind: crate::record::RecKind::Native,
                native_token: "digital",
                freq_hz: self.freq_hz,
                time,
                mode: &self.mode,
                vfo_name: &self.vfo_name,
            },
        )
    }

    /// A picture's name: `pic_` + the recording name at its time, so two
    /// recordings at once do not write over each other's pictures.
    fn picture_stem(&self, time: f64) -> String {
        format!(
            "pic_{}",
            self.name(UtcDateTime::from_unix(time.max(0.0) as u64))
        )
    }
}

/// The recording's progress for the panel.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecStatus {
    pub file: String,
    pub bytes: u64,
    pub started: f64,
    pub pictures: usize,
}

/// What marks need from the VFO.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    pub vfo_hz: u64,
    pub sideband: Sideband,
}

enum Msg {
    Iq {
        samples: Vec<Complex32>,
        rate: f64,
        time: f64,
    },
    Config(Config),
}

struct Snapshot {
    status: Status,
    config: Config,
    spectrum: Option<Vec<f32>>,
    picture: Option<Picture>,
}

enum Back {
    Outputs(Vec<Output>, f64),
    State(Box<Snapshot>),
}

fn worker(config: Config, rx: Receiver<Msg>, tx: Sender<Back>, busy: Arc<AtomicUsize>) {
    let mut config = config;
    let mut dec: Option<Decoder> = None;
    let mut spectra = 0;
    let mut pic_done = usize::MAX;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Config(c) => {
                config = c;
                if let Some(d) = dec.as_mut() {
                    d.set_config(c);
                }
            }
            Msg::Iq {
                samples,
                rate,
                time,
            } => {
                let d = match &mut dec {
                    Some(d) if d.rate() == rate => d,
                    _ => dec.insert(Decoder::new(rate, config)),
                };
                let out = d.process(&samples);
                if !out.is_empty() && tx.send(Back::Outputs(out, time)).is_err() {
                    return;
                }
                let (spec, n) = d.spectrum();
                let spectrum = (n != spectra).then(|| spec.to_vec());
                spectra = n;
                let status = d.status();
                let picture = match status.picture {
                    Some((done, _)) if done != pic_done => {
                        pic_done = done;
                        d.picture().cloned()
                    }
                    Some(_) => None,
                    None => {
                        pic_done = usize::MAX;
                        None
                    }
                };
                let snap = Snapshot {
                    status,
                    config: d.config(),
                    spectrum,
                    picture,
                };
                if tx.send(Back::State(Box::new(snap))).is_err() {
                    return;
                }
                busy.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }
}

struct Recording {
    naming: RecNaming,
    file: std::fs::File,
    path: PathBuf,
    writer: Writer,
    status: RecStatus,
}

pub struct DigitalService {
    to: Option<Sender<Msg>>,
    from: Receiver<Back>,
    /// IQ blocks sent to the worker and not fully answered yet.
    busy: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
    config: Config,
    status: Status,
    lines: VecDeque<Line>,
    /// The last line is still growing.
    open_line: bool,
    /// Lines dropped from the front so far (line numbers stay stable).
    dropped: u64,
    pictures: VecDeque<Pic>,
    next_pic: u64,
    /// The picture being received.
    live: Option<Picture>,
    spectrum: VecDeque<Vec<f32>>,
    rec: Option<Recording>,
    rec_error: Option<String>,
    /// The config the worker found (AFC, RSID) and the app has not taken.
    found: Option<Config>,
    /// Counts changes, so the view can cache.
    version: u64,
    /// What was decoded since the service started (for the Report).
    totals: Totals,
    /// Picture files written since the Report last took them.
    new_files: Vec<PathBuf>,
}

/// What a Digital VFO decoded since its service started (spec 20.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    pub text_bytes: u64,
    pub pictures: u64,
    pub rsid: u64,
    /// RSIDs that switched the mode.
    pub switches: u64,
}

impl DigitalService {
    pub fn new(config: Config) -> Self {
        let (to, wrx) = unbounded();
        let (wtx, from) = unbounded();
        let busy = Arc::new(AtomicUsize::new(0));
        let thread = std::thread::Builder::new()
            .name("digital".into())
            .spawn({
                let busy = Arc::clone(&busy);
                move || worker(config, wrx, wtx, busy)
            })
            .ok();
        Self {
            to: Some(to),
            from,
            busy,
            thread,
            config,
            status: Status {
                carrier: config.carrier,
                ..Status::default()
            },
            lines: VecDeque::new(),
            open_line: false,
            dropped: 0,
            pictures: VecDeque::new(),
            next_pic: 1,
            live: None,
            spectrum: VecDeque::new(),
            rec: None,
            rec_error: None,
            found: None,
            version: 0,
            totals: Totals::default(),
            new_files: Vec::new(),
        }
    }

    pub fn config(&self) -> Config {
        self.config
    }

    /// Picture files written since the last call (for the Report).
    pub fn take_new_files(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.new_files)
    }

    /// What was decoded since the service started.
    pub fn totals(&self) -> Totals {
        self.totals
    }

    /// New settings from the VFO.
    /// (A carrier less than 0.5 Hz off is the one the settings rounded.)
    pub fn set_config(&mut self, c: Config) {
        let same = Config {
            carrier: self.config.carrier,
            ..c
        } == self.config
            && (c.carrier - self.config.carrier).abs() < 0.5;
        if !same {
            self.config = c;
            if let Some(t) = &self.to {
                let _ = t.send(Msg::Config(c));
            }
        }
    }

    /// Channel IQ from the engine; `time` is the instant its end stands for.
    pub fn push(&mut self, samples: Vec<Complex32>, rate: f64, time: f64) {
        if let Some(t) = &self.to {
            self.busy.fetch_add(1, Ordering::SeqCst);
            if t.send(Msg::Iq {
                samples,
                rate,
                time,
            })
            .is_err()
            {
                self.busy.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    /// Blocks of IQ the worker has not decoded yet: 0 = all it found is
    /// waiting for [`Self::tick`].
    pub fn backlog(&self) -> usize {
        self.busy.load(Ordering::SeqCst)
    }

    /// Take what the worker found; returns whether anything changed.
    pub fn tick(&mut self, tuning: Tuning) -> bool {
        let mut changed = false;
        loop {
            match self.from.try_recv() {
                Ok(Back::Outputs(out, time)) => {
                    for o in out {
                        self.apply(o, time, tuning);
                    }
                    changed = true;
                }
                Ok(Back::State(s)) => {
                    self.status = s.status;
                    if s.config.mode != self.config.mode
                        || (s.config.carrier - self.config.carrier).abs() >= 1.0
                    {
                        self.config.mode = s.config.mode;
                        self.config.carrier = s.config.carrier;
                        self.found = Some(self.config);
                    }
                    if let Some(row) = s.spectrum {
                        self.spectrum.push_back(row);
                        while self.spectrum.len() > SPECTRUM_ROWS {
                            self.spectrum.pop_front();
                        }
                    }
                    if let Some(p) = s.picture {
                        self.live = Some(p);
                    } else if s.status.picture.is_none() {
                        self.live = None;
                    }
                    changed = true;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        if let Some(r) = &mut self.rec {
            let _ = r.file.flush();
        }
        if changed {
            self.version += 1;
        }
        changed
    }

    fn apply(&mut self, o: Output, time: f64, tuning: Tuning) {
        let t = UtcDateTime::from_unix(time.max(0.0) as u64);
        let q = |mode: &str, carrier: f64| {
            markdown::qrg(t, mode, tuning.vfo_hz, tuning.sideband, carrier)
        };
        match &o {
            Output::Text(s) => self.totals.text_bytes += s.len() as u64,
            Output::Rsid(RsidNote::Switch { .. }) => {
                self.totals.rsid += 1;
                self.totals.switches += 1;
            }
            Output::Rsid(_) => self.totals.rsid += 1,
            Output::PictureDone(_) => self.totals.pictures += 1,
            Output::PictureStart { .. } => {}
        }
        match o {
            Output::Text(s) => self.add_text(&s),
            Output::Rsid(RsidNote::Switch { before, after }) => {
                self.add_mark(format!("Before RSID: {}", q(before.0, before.1)));
                self.add_mark(format!("New RSID: {}", q(after.0, after.1)));
            }
            Output::Rsid(RsidNote::Other { name, code, freq }) => {
                let text = match name {
                    Some(n) => format!("RSID: {n}, not decoded here: {}", q(n, freq)),
                    None => format!(
                        "RSID: unknown code {code}: {}",
                        q(&format!("code {code}"), freq)
                    ),
                };
                self.add_mark(text);
            }
            Output::PictureStart { .. } => {}
            Output::PictureDone(p) => {
                self.live = None;
                self.add_picture(p, time);
            }
        }
    }

    fn push_line(&mut self, l: Line) {
        self.lines.push_back(l);
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
            self.dropped += 1;
        }
    }

    fn add_text(&mut self, s: &str) {
        if let Some(r) = &mut self.rec {
            let md = r.writer.text(s);
            Self::write(r, &md, &mut self.rec_error);
        }
        // each line end closes the open line; two in a row leave an empty one
        for (i, part) in s.split('\n').enumerate() {
            if i > 0 {
                if !self.open_line {
                    self.push_line(Line::Text(String::new()));
                }
                self.open_line = false;
            }
            if part.is_empty() {
                continue;
            }
            match self.lines.back_mut() {
                Some(Line::Text(l)) if self.open_line => l.push_str(part),
                _ => {
                    self.push_line(Line::Text(part.to_string()));
                    self.open_line = true;
                }
            }
        }
    }

    fn add_mark(&mut self, text: String) {
        if let Some(r) = &mut self.rec {
            let md = r.writer.block(&markdown::mark(&text));
            Self::write(r, &md, &mut self.rec_error);
        }
        self.push_line(Line::Mark(text));
        self.open_line = false;
    }

    /// A picture and a waterfall row as if decoded (other modules' tests).
    #[cfg(test)]
    pub(crate) fn add_for_test(&mut self, p: Picture, row: Vec<f32>) {
        self.add_picture(p, 0.0);
        self.spectrum.push_back(row);
    }

    fn add_picture(&mut self, p: Picture, time: f64) {
        let id = self.next_pic;
        self.next_pic += 1;
        let mut pic = Pic {
            id,
            width: p.width,
            height: p.height,
            color: p.color,
            rgb: p.rgb,
            time,
            file: None,
        };
        if let Some(r) = &mut self.rec {
            match write_picture(&r.naming.dir, &pic, &r.naming) {
                Ok(path) => {
                    let md = r.writer.block(&markdown::picture_link(
                        pic.width, pic.height, pic.color, &path,
                    ));
                    Self::write(r, &md, &mut self.rec_error);
                    r.status.pictures += 1;
                    self.new_files.push(path.clone());
                    pic.file = Some(path);
                }
                Err(e) => self.rec_error = Some(e),
            }
        }
        self.open_line = false;
        self.push_line(Line::Picture(id));
        self.pictures.push_back(pic);
        while self.pictures.len() > MAX_PICTURES {
            self.pictures.pop_front();
        }
    }

    fn write(r: &mut Recording, md: &str, err: &mut Option<String>) {
        match r.file.write_all(md.as_bytes()) {
            Ok(()) => r.status.bytes += md.len() as u64,
            Err(e) => *err = Some(format!("{}: {e}", r.path.display())),
        }
    }

    // ── the view's data ──────────────────────────────────────────────

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn lines(&self) -> &VecDeque<Line> {
        &self.lines
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn pictures(&self) -> &VecDeque<Pic> {
        &self.pictures
    }

    pub fn picture(&self, id: u64) -> Option<&Pic> {
        self.pictures.iter().find(|p| p.id == id)
    }

    /// The picture being received.
    pub fn live_picture(&self) -> Option<&Picture> {
        self.live.as_ref()
    }

    /// Waterfall rows, oldest first: power over 0..5512 Hz in 1024 bins.
    pub fn spectrum(&self) -> &VecDeque<Vec<f32>> {
        &self.spectrum
    }

    /// The mode or carrier the receiver moved to (RSID, AFC), once.
    pub fn take_found(&mut self) -> Option<Config> {
        self.found.take()
    }

    pub fn take_rec_error(&mut self) -> Option<String> {
        self.rec_error.take()
    }

    /// Forget the text and pictures (the recording goes on).
    pub fn clear(&mut self) {
        self.lines.clear();
        self.pictures.clear();
        self.open_line = false;
        self.version += 1;
    }

    // ── recording and saving ─────────────────────────────────────────

    pub fn recording(&self) -> bool {
        self.rec.is_some()
    }

    pub fn rec_status(&self) -> Option<&RecStatus> {
        self.rec.as_ref().map(|r| &r.status)
    }

    /// Start the Markdown recording (spec 16.7).
    pub fn start_recording(&mut self, naming: RecNaming, now: f64) {
        if self.rec.is_some() {
            return;
        }
        let stem = naming.name(UtcDateTime::from_unix(now.max(0.0) as u64));
        let opened = std::fs::create_dir_all(&naming.dir)
            .map_err(|e| format!("{}: {e}", naming.dir.display()))
            .and_then(|()| {
                let path = markdown::unique(&naming.dir, &stem, "md");
                std::fs::File::create(&path)
                    .map(|f| (f, path.clone()))
                    .map_err(|e| format!("{}: {e}", path.display()))
            });
        match opened {
            Ok((file, path)) => {
                let status = RecStatus {
                    file: path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                    bytes: 0,
                    started: now,
                    pictures: 0,
                };
                self.rec = Some(Recording {
                    naming,
                    file,
                    path,
                    writer: Writer::default(),
                    status,
                });
            }
            Err(e) => self.rec_error = Some(e),
        }
    }

    /// Stop the recording; its status for the list of recordings.
    pub fn stop_recording(&mut self) -> Option<RecStatus> {
        let mut r = self.rec.take()?;
        let _ = r.file.flush();
        Some(r.status)
    }

    /// F2: the session so far as Markdown in `dir`, with its pictures
    /// (named by `naming` if not in `dir` already).
    pub fn save(&mut self, dir: &Path, stem: &str, naming: &RecNaming) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut w = Writer::default();
        let mut md = String::new();
        let n = self.lines.len();
        for i in 0..n {
            match self.lines[i].clone() {
                Line::Text(t) => {
                    md += &w.text(&t);
                    if i + 1 < n || !self.open_line {
                        md += &w.text("\n");
                    }
                }
                Line::Mark(m) => md += &w.block(&markdown::mark(&m)),
                Line::Picture(id) => {
                    let Some(pic) = self.pictures.iter_mut().find(|p| p.id == id) else {
                        continue;
                    };
                    let in_dir = pic
                        .file
                        .as_ref()
                        .is_some_and(|f| f.parent() == Some(&dir.join(PICTURES)) && f.exists());
                    if !in_dir {
                        pic.file = Some(write_picture(dir, pic, naming)?);
                    }
                    let f = pic.file.as_ref().expect("written above");
                    md += &w.block(&markdown::picture_link(pic.width, pic.height, pic.color, f));
                }
            }
        }
        let path = markdown::unique(dir, stem, "md");
        std::fs::write(&path, md).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}

/// A picture's PNG in `<dir>/pictures/`, named by the recording template
/// at its time.
fn write_picture(dir: &Path, p: &Pic, naming: &RecNaming) -> Result<PathBuf, String> {
    let pics = dir.join(PICTURES);
    std::fs::create_dir_all(&pics).map_err(|e| format!("{}: {e}", pics.display()))?;
    let stem = naming.picture_stem(p.time);
    let path = markdown::unique(&pics, &stem, "png");
    markdown::write_png(&path, p.width, p.height, &p.rgb)?;
    Ok(path)
}

impl std::fmt::Debug for DigitalService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DigitalService")
            .field("config", &self.config)
            .field("lines", &self.lines.len())
            .field("recording", &self.recording())
            .finish_non_exhaustive()
    }
}

impl Drop for DigitalService {
    fn drop(&mut self) {
        self.to = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mfsk::decoder::DEFAULT_SQUELCH;
    use crate::mfsk::modes::by_name;

    fn config() -> Config {
        Config {
            mode: by_name("MFSK-32").unwrap(),
            carrier: 1500.0,
            sideband: Sideband::Upper,
            afc: true,
            rsid: true,
            squelch: DEFAULT_SQUELCH,
        }
    }

    const TUNING: Tuning = Tuning {
        vfo_hz: 15_770_000,
        sideband: Sideband::Upper,
    };

    fn temp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("shellsdr-digital-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn pic() -> Picture {
        Picture {
            width: 2,
            height: 1,
            color: true,
            rgb: vec![1, 2, 3, 4, 5, 6],
            done: 2,
        }
    }

    #[test]
    fn text_marks_and_pictures_make_lines_and_markdown() {
        let d = temp("doc");
        let mut s = DigitalService::new(config());
        let t = 1_795_027_880.0; // 2026-11-18 18:51:20
        let naming = RecNaming {
            dir: d.clone(),
            template: "$y$M$d-$h$m$s_$t_$f".into(),
            vfo_name: "Master".into(),
            freq_hz: 15_770_000.0,
            mode: "MFSK-32".into(),
        };
        s.start_recording(naming.clone(), t);
        s.apply(Output::Text("Hello\nwor".into()), t, TUNING);
        s.apply(Output::Text("ld\n".into()), t, TUNING);
        s.apply(
            Output::Rsid(RsidNote::Switch {
                before: ("MFSK-32", 1496.0),
                after: ("MFSK-64", 1500.0),
            }),
            t,
            TUNING,
        );
        s.apply(Output::Text("# Sending Pic:2x1C;".into()), t, TUNING);
        // the picture is named like the recording, at its own time
        s.apply(Output::PictureDone(pic()), t + 7.0, TUNING);
        s.apply(Output::Text("after\n".into()), t, TUNING);
        s.apply(
            Output::Rsid(RsidNote::Other {
                name: Some("BPSK31"),
                code: 1,
                freq: 1000.0,
            }),
            t,
            TUNING,
        );
        assert_eq!(
            s.lines().iter().cloned().collect::<Vec<_>>(),
            [
                Line::Text("Hello".into()),
                Line::Text("world".into()),
                Line::Mark("Before RSID: <<2026-11-18T18:51Z MFSK-32 @ 15770000+1496>>".into()),
                Line::Mark("New RSID: <<2026-11-18T18:51Z MFSK-64 @ 15770000+1500>>".into()),
                Line::Text("# Sending Pic:2x1C;".into()),
                Line::Picture(1),
                Line::Text("after".into()),
                Line::Mark(
                    "RSID: BPSK31, not decoded here: <<2026-11-18T18:51Z BPSK31 @ 15770000+1000>>"
                        .into()
                ),
            ]
        );
        let st = s.stop_recording().unwrap();
        assert_eq!(st.file, "20261118-185120_digital_15770000Hz.md");
        assert_eq!(st.pictures, 1);
        let md = std::fs::read_to_string(d.join("20261118-185120_digital_15770000Hz.md")).unwrap();
        assert_eq!(
            md,
            "Hello\nworld\n\n\
             ==🔵*Before RSID: <<2026-11-18T18:51Z MFSK-32 @ 15770000+1496>>*==\n\n\
             ==🔵*New RSID: <<2026-11-18T18:51Z MFSK-64 @ 15770000+1500>>*==\n\n\
             \\# Sending Pic:2x1C;\n\n\
             ![Pic:2x1C](pictures/pic_20261118-185127_digital_15770000Hz.png)\n\n\
             after\n\n\
             ==🔵*RSID: BPSK31, not decoded here: <<2026-11-18T18:51Z BPSK31 @ 15770000+1000>>*==\n\n"
        );
        assert!(
            d.join("pictures/pic_20261118-185127_digital_15770000Hz.png")
                .is_file()
        );
        // F2 Save writes the same document elsewhere, with its own pictures
        let other = d.join("saved");
        let p = s.save(&other, "session", &naming).unwrap();
        assert_eq!(std::fs::read_to_string(p).unwrap(), md);
        assert!(
            other
                .join("pictures/pic_20261118-185127_digital_15770000Hz.png")
                .is_file()
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn nothing_is_written_without_a_recording() {
        let d = temp("none");
        let mut s = DigitalService::new(config());
        s.apply(Output::PictureDone(pic()), 0.0, TUNING);
        assert!(!d.exists());
        assert_eq!(s.pictures().len(), 1);
        assert!(s.pictures()[0].file.is_none());
    }

    #[test]
    fn the_worker_decodes_pushed_iq() {
        use crate::mfsk::tx::Tx;
        let mode = by_name("MFSK-32").unwrap();
        let mut tx = Tx::new(mode, 1500.0);
        tx.silence(4000);
        tx.start();
        tx.text(b"from the worker\n");
        tx.end();
        tx.silence(8000);
        // at 8 kHz the channel is the modem's own rate
        let iq: Vec<Complex32> = tx
            .take()
            .iter()
            .map(|z| Complex32::new(z.re as f32, z.im as f32))
            .collect();
        let mut s = DigitalService::new(config());
        for b in iq.chunks(800) {
            s.push(b.to_vec(), 8000.0, 0.0);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            s.tick(TUNING);
            if s.lines()
                .iter()
                .any(|l| l == &Line::Text("from the worker".into()))
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("{:?}", s.lines());
    }
}
