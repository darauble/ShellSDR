//! The fax receiver of a FAX VFO (spec 15.4, 15.5): the FAX library's live
//! decoder fed with the VFO's audio, the automatic slant measurement on a
//! worker thread, saving a picture, and recording pictures to growing PNGs.
//! Lives on the UI thread; the view draws from it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use super::ffi::{Config, FaxLib, Live, LiveStatus, PictureInfo, PngFile, Slant};
use crate::record::naming::UtcDateTime;

/// A recorded picture continues in `_part2`, ... after this many lines
/// (about 2 h 45 min at 120 LPM).
pub const MAX_FILE_LINES: i32 = 20_000;
/// Auto slant: the first measurement at this many lines, then every
/// `SLANT_EVERY` new lines.
pub const SLANT_FIRST: i32 = 200;
pub const SLANT_EVERY: i32 = 100;
/// Lines measured at most (the last ones).
pub const SLANT_LINES: i32 = 2000;
/// A picture without START is recorded once it has this many lines (the
/// library drops shorter gaps between faxes).
pub const RECORD_MIN_LINES: i32 = 200;

/// The receiving state shown in the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// No audio yet.
    Waiting,
    Receiving,
    Start,
    /// A START was heard; its phasing lines are being checked.
    Phasing,
    Stop,
}

/// The slant state of a picture (spec 15.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SlantState {
    /// Auto, fewer lines than the first measurement needs.
    Waiting { lines: i32 },
    /// Auto, measuring or not found yet.
    Measuring { lines: i32 },
    /// Auto found it (the picture is drawn with it), from `lines` lines:
    /// rough (a few ppm) at 200 lines, within about 1 ppm from 1000.
    Found {
        ppm: f64,
        confidence: f64,
        lines: i32,
    },
    /// Auto is off: the picture keeps its value.
    Manual,
}

#[derive(Debug, Clone, Copy, Default)]
struct PictureSlant {
    /// Lines of the last measurement started.
    measured_at: i32,
    found: Option<(f64, f64, i32)>,
}

/// How recordings are named (the VFO's recording template, spec 13.7).
#[derive(Debug, Clone, PartialEq)]
pub struct RecNaming {
    pub dir: PathBuf,
    pub template: String,
    pub vfo_name: String,
    pub freq_hz: f64,
}

impl RecNaming {
    fn name(&self, time: UtcDateTime) -> String {
        use crate::record::naming::{NameFields, expand};
        expand(
            &self.template,
            &NameFields {
                kind: crate::record::RecKind::Native,
                native_token: "fax",
                freq_hz: self.freq_hz,
                time,
                mode: "FAX",
                vfo_name: &self.vfo_name,
            },
        )
    }
}

/// The picture being recorded.
struct RecFile {
    picture: u64,
    /// The first file's path (parts are `_partN` beside it).
    first: PathBuf,
    part: u32,
    /// The picture line the current part starts at.
    part_start: i32,
    /// The next picture line to write.
    next: i32,
    /// The ppm the lines were written with (a change rewrites the part).
    ppm: f64,
    png: Option<PngFile>,
}

/// The recording state shown in the view and the Radio module.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecStatus {
    pub file: String,
    pub lines: i32,
    pub files: u32,
    /// Bytes of the current file.
    pub bytes: u64,
}

/// The fax receiver.
pub struct FaxService {
    /// Pictures recorded to a file: (picture id, its first file), the last
    /// 64 (the Report links them).
    recorded: Vec<(u64, PathBuf)>,
    lib: Arc<FaxLib>,
    live: Option<Live>,
    config: Config,
    rate: f64,
    cache_limit: u64,
    /// Wall clock (UTC s) of stream second 0.
    wall0: f64,
    auto: bool,
    slants: Vec<(u64, PictureSlant)>,
    worker: Option<(u64, Receiver<Slant>)>,
    /// A clock correction found by auto slant, for the settings.
    ppm_found: Option<f64>,
    /// The picture shown (None: the newest).
    selected: Option<u64>,
    rec: Option<(RecNaming, Option<RecFile>)>,
    rec_status: RecStatus,
    rec_error: Option<String>,
    notices: Vec<String>,
}

/// Half the computer's memory (spec 15.4), 256 MB if unknown.
pub fn default_cache_limit() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .map_or(256 << 20, |kb| kb * 1024 / 2)
}

/// A free name for `stem` + `.png` in `dir` (`_2`, `_3`, ... if taken).
pub fn unique_png(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.png"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|i| dir.join(format!("{stem}_{i}.png")))
        .find(|p| !p.exists())
        .expect("a free name exists")
}

fn part_path(first: &Path, part: u32) -> PathBuf {
    if part <= 1 {
        return first.to_path_buf();
    }
    let stem = first
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    first.with_file_name(format!("{stem}_part{part}.png"))
}

impl FaxService {
    pub fn new(lib: Arc<FaxLib>, config: Config, auto: bool, cache_limit: u64) -> Self {
        Self {
            lib,
            live: None,
            config,
            rate: 0.0,
            cache_limit,
            wall0: 0.0,
            auto,
            slants: Vec::new(),
            worker: None,
            ppm_found: None,
            selected: None,
            rec: None,
            rec_status: RecStatus::default(),
            rec_error: None,
            recorded: Vec::new(),
            notices: Vec::new(),
        }
    }

    pub fn config(&self) -> Config {
        self.config
    }

    /// New LPM / width: the pictures so far are dropped (they were cut for
    /// the old ones). The ppm alone does not reset anything.
    pub fn set_config(&mut self, config: Config) {
        if (config.lpm, config.width) != (self.config.lpm, self.config.width) {
            self.stop_recording();
            self.live = None;
            self.slants.clear();
            self.worker = None;
            self.selected = None;
        }
        self.config = config;
    }

    pub fn auto(&self) -> bool {
        self.auto
    }

    /// Auto slant on: the pictures are drawn with what it found again (a
    /// manual value in between is dropped).
    pub fn set_auto(&mut self, on: bool) {
        if on
            && !self.auto
            && let Some(live) = self.live.as_mut()
        {
            for (id, s) in &self.slants {
                if let Some((ppm, _, _)) = s.found {
                    live.set_ppm(*id, ppm);
                    // the newest one found goes to the settings
                    self.ppm_found = Some(ppm);
                }
            }
        }
        self.auto = on;
    }

    /// Audio from the VFO (`now`: the wall clock, UTC s).
    pub fn push(&mut self, samples: &[i16], rate: f64, now: f64) {
        if samples.is_empty() {
            return;
        }
        if self.live.is_none() || rate != self.rate {
            match Live::new(self.lib.clone(), self.config, rate) {
                Ok(mut l) => {
                    l.set_cache_limit(self.cache_limit);
                    self.live = Some(l);
                    self.rate = rate;
                    self.slants.clear();
                    self.selected = None;
                }
                Err(e) => {
                    self.notices.push(format!("fax decoder: {e}"));
                    return;
                }
            }
        }
        let live = self.live.as_mut().expect("created above");
        live.push(samples);
        self.wall0 = now - live.status().seconds;
        self.after_push();
    }

    fn after_push(&mut self) {
        self.poll_worker();
        self.start_slant();
        self.write_recording();
    }

    /// Periodic work without audio (the slant worker's result).
    pub fn tick(&mut self) {
        self.poll_worker();
    }

    pub fn status(&self) -> Option<LiveStatus> {
        self.live.as_ref().map(Live::status)
    }

    pub fn signal(&self) -> Signal {
        match self.status() {
            None => Signal::Waiting,
            Some(s) if s.start_pending != 0 => Signal::Phasing,
            Some(s) => match s.signal {
                1 => Signal::Start,
                2 => Signal::Stop,
                _ => Signal::Receiving,
            },
        }
    }

    pub fn pictures(&self) -> Vec<PictureInfo> {
        self.live.as_ref().map(Live::pictures).unwrap_or_default()
    }

    /// The picture shown: the selected one, else the newest.
    pub fn current(&self) -> Option<PictureInfo> {
        let live = self.live.as_ref()?;
        self.selected
            .and_then(|id| live.picture(id))
            .or_else(|| live.pictures().pop())
    }

    /// Whether the view follows the newest picture.
    pub fn following(&self) -> bool {
        self.selected.is_none()
    }

    /// Show the picture `by` places older (-) or newer (+); past the newest
    /// it follows the newest again.
    pub fn select_by(&mut self, by: i32) {
        let pics = self.pictures();
        if pics.is_empty() {
            return;
        }
        let cur = self
            .current()
            .and_then(|c| pics.iter().position(|p| p.id == c.id))
            .unwrap_or(pics.len() - 1) as i32;
        let to = (cur + by).clamp(0, pics.len() as i32 - 1) as usize;
        self.selected = (to + 1 < pics.len()).then(|| pics[to].id);
    }

    pub fn follow(&mut self) {
        self.selected = None;
    }

    /// The wall clock (UTC s) of a picture's start.
    pub fn picture_time(&self, p: &PictureInfo) -> f64 {
        self.wall0 + p.start_seconds
    }

    pub fn render(&self, id: u64, first: i32, lines: i32, w: usize, h: usize) -> Option<Vec<u8>> {
        self.live.as_ref()?.render(id, first, lines, w, h)
    }

    pub fn lib(&self) -> &Arc<FaxLib> {
        &self.lib
    }

    // ── slant ──────────────────────────────────────────────────────────

    fn slant_mut(&mut self, id: u64) -> &mut PictureSlant {
        if let Some(i) = self.slants.iter().position(|(p, _)| *p == id) {
            return &mut self.slants[i].1;
        }
        self.slants.push((id, PictureSlant::default()));
        &mut self.slants.last_mut().expect("pushed").1
    }

    fn slant_of(&self, id: u64) -> PictureSlant {
        self.slants
            .iter()
            .find(|(p, _)| *p == id)
            .map(|(_, s)| *s)
            .unwrap_or_default()
    }

    /// The slant state of a picture.
    pub fn slant_state(&self, p: &PictureInfo) -> SlantState {
        if !self.auto {
            return SlantState::Manual;
        }
        match self.slant_of(p.id).found {
            Some((ppm, confidence, lines)) => SlantState::Found {
                ppm,
                confidence,
                lines,
            },
            None if p.lines - p.first_line < SLANT_FIRST => SlantState::Waiting { lines: p.lines },
            None => SlantState::Measuring { lines: p.lines },
        }
    }

    fn start_slant(&mut self) {
        if !self.auto || self.worker.is_some() {
            return;
        }
        let Some(live) = &self.live else { return };
        // the newest picture that has grown enough since its last measurement
        let pick = live.pictures().into_iter().rev().find(|p| {
            let s = self.slant_of(p.id);
            let n = p.lines - p.first_line;
            n >= SLANT_FIRST && (s.measured_at == 0 || p.lines - s.measured_at >= SLANT_EVERY)
        });
        let Some(p) = pick else { return };
        let Some(job) = live.slant_job(p.id, SLANT_LINES) else {
            return;
        };
        self.slant_mut(p.id).measured_at = p.lines;
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("fax-slant".into())
            .spawn(move || {
                let _ = tx.send(job.run());
            });
        if spawned.is_ok() {
            self.worker = Some((p.id, rx));
        }
    }

    fn poll_worker(&mut self) {
        let Some((id, rx)) = &self.worker else { return };
        let id = *id;
        match rx.try_recv() {
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.worker = None,
            Ok(r) => {
                self.worker = None;
                if r.valid && self.auto {
                    self.slant_mut(id).found = Some((r.ppm, r.confidence, r.lines));
                    if let Some(live) = self.live.as_mut()
                        && live.set_ppm(id, r.ppm)
                    {
                        self.ppm_found = Some(r.ppm);
                        self.config.clock_ppm = r.ppm;
                    }
                }
            }
        }
    }

    /// Whether a measurement is running.
    pub fn measuring(&self) -> bool {
        self.worker.is_some()
    }

    /// A clock correction auto slant found since the last call (for the
    /// VFO's settings).
    pub fn take_ppm_found(&mut self) -> Option<f64> {
        self.ppm_found.take()
    }

    /// Set the clock correction of the picture shown (and of the newer ones
    /// still to come). The recording of that picture is rewritten.
    pub fn set_ppm(&mut self, ppm: f64) {
        self.config.clock_ppm = ppm;
        let Some(cur) = self.current() else { return };
        if let Some(live) = self.live.as_mut() {
            live.set_ppm(cur.id, ppm);
        }
        self.write_recording();
    }

    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    // ── save ──────────────────────────────────────────────────────────

    /// Save lines `..end` of the picture shown (from its oldest line in
    /// memory) to `dir`, named after its start, and free the memory before
    /// `end`. Returns the file.
    pub fn save(&mut self, dir: &Path, stem: &str, end: i32) -> Result<PathBuf, String> {
        let cur = self.current().ok_or("no picture yet")?;
        let end = end.min(cur.lines);
        if end <= cur.first_line {
            return Err("nothing to save".into());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = unique_png(dir, stem);
        let live = self.live.as_ref().ok_or("no picture yet")?;
        let width = self.config.width as usize;
        let mut png = PngFile::create(self.lib.clone(), &path, width)?;
        let mut line = vec![255u8; width];
        for l in cur.first_line..end {
            if !live.render_line(cur.id, l, &mut line) {
                line.fill(255);
            }
            png.write_line(&line)?;
        }
        png.close()?;
        // a recording of this picture still needs its lines
        let recorded = self
            .rec
            .as_ref()
            .and_then(|(_, f)| f.as_ref())
            .filter(|f| f.picture == cur.id)
            .map(|f| f.next);
        let release = recorded.map_or(end, |n| n.min(end));
        if let Some(live) = self.live.as_mut() {
            live.release_before(cur.id, release);
        }
        Ok(path)
    }

    // ── recording ─────────────────────────────────────────────────────

    pub fn recording(&self) -> bool {
        self.rec.is_some()
    }

    pub fn rec_status(&self) -> &RecStatus {
        &self.rec_status
    }

    /// The file picture `id` was recorded to, if it was.
    pub fn recorded_file(&self, id: u64) -> Option<&Path> {
        self.recorded
            .iter()
            .rev()
            .find(|(p, _)| *p == id)
            .map(|(_, f)| f.as_path())
    }

    /// A write error ended the recording (taken once).
    pub fn take_rec_error(&mut self) -> Option<String> {
        self.rec_error.take()
    }

    /// Record the newest picture and the ones after it (spec 15.4).
    pub fn start_recording(&mut self, naming: RecNaming) {
        if self.rec.is_none() {
            self.rec_status = RecStatus::default();
        }
        self.rec = Some((naming, None));
        self.write_recording();
    }

    pub fn stop_recording(&mut self) {
        if let Some((_, Some(mut f))) = self.rec.take()
            && let Err(e) = f.close()
        {
            self.rec_error = Some(e);
        }
    }

    fn write_recording(&mut self) {
        if let Err(e) = self.try_write_recording() {
            self.stop_recording();
            self.rec_error = Some(e);
        }
    }

    fn try_write_recording(&mut self) -> Result<(), String> {
        let Some(live) = &self.live else {
            return Ok(());
        };
        let Some((naming, file)) = &mut self.rec else {
            return Ok(());
        };
        let pics = live.pictures();
        // the newest picture worth a file: a fax (START), or a long enough
        // stretch without one
        let Some(target) = pics
            .iter()
            .rev()
            .find(|p| p.started != 0 || p.lines >= RECORD_MIN_LINES)
            .copied()
        else {
            return Ok(());
        };
        let width = self.config.width as usize;
        let mut line = vec![255u8; width];

        // a newer picture: the old file is complete. The old picture ended
        // where the new fax's START began, which is found a few lines late:
        // lines written past its end are cut off again.
        if let Some(mut f) = file.take_if(|f| f.picture != target.id) {
            if let Some(p) = pics.iter().find(|p| p.id == f.picture)
                && f.next > p.lines
            {
                f.next = f.part_start.max(p.first_line);
                f.reopen(&self.lib, width)?;
                write_lines(live, p, &mut f, p.lines, &mut line)?;
            }
            f.close()?;
        }
        if file.is_none() {
            std::fs::create_dir_all(&naming.dir)
                .map_err(|e| format!("{}: {e}", naming.dir.display()))?;
            let t = self.wall0 + target.start_seconds;
            let stem = naming.name(UtcDateTime::from_unix(t.max(0.0) as u64));
            let path = unique_png(&naming.dir, &stem);
            self.recorded.push((target.id, path.clone()));
            if self.recorded.len() > 64 {
                self.recorded.remove(0);
            }
            *file = Some(RecFile {
                picture: target.id,
                png: Some(PngFile::create(self.lib.clone(), &path, width)?),
                first: path,
                part: 1,
                part_start: target.first_line,
                next: target.first_line,
                ppm: target.ppm,
            });
            self.rec_status.files += 1;
        }
        let f = file.as_mut().expect("opened above");
        // a new clock correction: this part again, straight
        if f.ppm != target.ppm {
            f.ppm = target.ppm;
            f.part_start = f.part_start.max(target.first_line);
            f.next = f.part_start;
            f.reopen(&self.lib, width)?;
        }
        while f.next < target.lines {
            if f.next - f.part_start >= MAX_FILE_LINES {
                f.close()?;
                f.part += 1;
                f.part_start = f.next;
                f.reopen(&self.lib, width)?;
                self.rec_status.files += 1;
            }
            let end = target.lines.min(f.part_start + MAX_FILE_LINES);
            write_lines(live, &target, f, end, &mut line)?;
        }
        self.rec_status.lines = f.next - f.part_start;
        let path = part_path(&f.first, f.part);
        self.rec_status.bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        self.rec_status.file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(())
    }
}

impl RecFile {
    /// Start the current part's file again (empty).
    fn reopen(&mut self, lib: &Arc<FaxLib>, width: usize) -> Result<(), String> {
        // close first: the new file has the same name
        if let Some(png) = self.png.take() {
            png.close()?;
        }
        self.png = Some(PngFile::create(
            lib.clone(),
            &part_path(&self.first, self.part),
            width,
        )?);
        Ok(())
    }

    fn close(&mut self) -> Result<(), String> {
        self.png.take().map_or(Ok(()), PngFile::close)
    }
}

/// Write the picture's lines `f.next..end` to its file.
fn write_lines(
    live: &Live,
    p: &PictureInfo,
    f: &mut RecFile,
    end: i32,
    line: &mut [u8],
) -> Result<(), String> {
    let Some(png) = f.png.as_mut() else {
        return Ok(());
    };
    while f.next < end {
        if !live.render_line(p.id, f.next, line) {
            line.fill(255);
        }
        png.write_line(line)?;
        f.next += 1;
    }
    Ok(())
}

impl Drop for FaxService {
    fn drop(&mut self) {
        self.stop_recording();
    }
}

impl std::fmt::Debug for FaxService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaxService")
            .field("config", &self.config)
            .field("auto", &self.auto)
            .field("recording", &self.recording())
            .finish_non_exhaustive()
    }
}
