//! The recorder: a writer thread that owns the WAV file.
//!
//! The DSP thread must never block on the disk, so it hands interleaved float
//! blocks to this thread over a bounded channel (`push`, non-blocking; a full
//! channel drops the block and counts it). The thread writes 16-bit PCM and,
//! when a file would pass the classic RIFF 4 GiB limit or the sample rate
//! changes (a new Source decimation during a baseband recording), finishes the
//! file and continues in `<name>_partN.wav`. Each file's ICMT gets the
//! metadata passed at start plus `sample_rate`, `part` and `end_time`.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

use super::naming::{UtcDateTime, part_path};
use super::wav::{MAX_DATA_BYTES, WavWriter, format_icmt};

/// Blocks the channel can hold before the DSP side starts dropping. At the
/// largest SoapySDR block (~64k IQ pairs) this caps the queue near 32 MB,
/// several seconds of slack for a slow SD card on the Pi.
const QUEUE_BLOCKS: usize = 64;

/// How a recording is set up.
#[derive(Debug, Clone)]
pub struct RecorderSpec {
    pub path: PathBuf,
    /// 1 = mono audio, 2 = I/Q.
    pub channels: u16,
    pub sample_rate: u32,
    /// Metadata for the ICMT (without `sample_rate`/`part`/`end_time`).
    pub info: Vec<(String, String)>,
    /// Split threshold for the data chunk (normally `wav::MAX_DATA_BYTES`).
    pub max_data_bytes: u64,
}

impl RecorderSpec {
    pub fn new(
        path: PathBuf,
        channels: u16,
        sample_rate: u32,
        info: Vec<(String, String)>,
    ) -> Self {
        Self {
            path,
            channels,
            sample_rate,
            info,
            max_data_bytes: MAX_DATA_BYTES,
        }
    }
}

/// Live counters the UI reads.
#[derive(Debug, Default)]
pub struct RecStats {
    /// Sample frames written (all parts).
    pub frames: AtomicU64,
    /// Bytes of sample data written (all parts).
    pub bytes: AtomicU64,
    /// Frames dropped because the writer fell behind.
    pub dropped: AtomicU64,
    /// Recorded time in microseconds (all parts, each at its own rate).
    pub micros: AtomicU64,
    /// The file currently being written.
    pub current: Mutex<PathBuf>,
    /// A write error that ended the recording.
    pub error: Mutex<Option<String>>,
}

impl RecStats {
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }

    fn set_error(&self, e: io::Error) {
        if let Ok(mut g) = self.error.lock() {
            *g = Some(e.to_string());
        }
    }

    pub fn current(&self) -> PathBuf {
        self.current.lock().map(|p| p.clone()).unwrap_or_default()
    }
}

enum Msg {
    Data(Vec<f32>),
    /// Continue in a new part at a new sample rate.
    Rate(u32),
}

/// The producer side of a recording, owned by the DSP thread. Dropping it
/// ends the recording (the writer finishes the file).
pub struct RecSink {
    tx: Sender<Msg>,
    stats: Arc<RecStats>,
    channels: u16,
    sample_rate: u32,
}

/// The writer-thread side, owned by whoever started the recording (the UI):
/// `finish` waits until the file is complete.
pub struct RecHandle {
    handle: JoinHandle<()>,
    stats: Arc<RecStats>,
    sample_rate: u32,
}

fn open(path: &Path, channels: u16, rate: u32) -> io::Result<WavWriter<BufWriter<File>>> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let f = File::options().write(true).create_new(true).open(path)?;
    WavWriter::new(BufWriter::with_capacity(1 << 20, f), channels, rate)
}

/// The ICMT for one finished part.
fn comment(info: &[(String, String)], rate: u32, part: u32) -> String {
    let mut all = info.to_vec();
    all.push(("sample_rate".into(), rate.to_string()));
    all.push(("part".into(), part.to_string()));
    all.push(("end_time".into(), UtcDateTime::now().iso()));
    format_icmt(&all)
}

/// Create the file (and its folder) and start the writer thread. The file is
/// opened here so a bad folder fails at once, not in the thread.
pub fn start(spec: RecorderSpec) -> io::Result<(RecSink, RecHandle)> {
    let first = open(&spec.path, spec.channels, spec.sample_rate)?;
    let stats = Arc::new(RecStats::default());
    if let Ok(mut c) = stats.current.lock() {
        *c = spec.path.clone();
    }
    let (tx, rx) = bounded::<Msg>(QUEUE_BLOCKS);
    let st = Arc::clone(&stats);
    let (channels, sample_rate) = (spec.channels, spec.sample_rate);
    let handle = std::thread::Builder::new()
        .name("shellsdr-rec".into())
        .spawn(move || writer_loop(spec, first, rx, st))?;
    Ok((
        RecSink {
            tx,
            stats: Arc::clone(&stats),
            channels,
            sample_rate,
        },
        RecHandle {
            handle,
            stats,
            sample_rate,
        },
    ))
}

impl std::fmt::Debug for RecSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecSink")
            .field("path", &self.stats.current())
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .finish()
    }
}

impl RecSink {
    /// Queue interleaved samples (never blocks; drops when the queue is full).
    pub fn push(&self, interleaved: Vec<f32>) {
        let frames = (interleaved.len() / self.channels.max(1) as usize) as u64;
        if let Err(TrySendError::Full(_)) = self.tx.try_send(Msg::Data(interleaved)) {
            self.stats.dropped.fetch_add(frames, Ordering::Relaxed);
        }
    }

    /// Continue in a new part file at `rate` (no-op when unchanged).
    pub fn set_sample_rate(&mut self, rate: u32) {
        if rate != self.sample_rate {
            self.sample_rate = rate;
            // Blocking send: a rate change must not be lost.
            let _ = self.tx.send(Msg::Rate(rate));
        }
    }
}

impl RecHandle {
    pub fn stats(&self) -> &RecStats {
        &self.stats
    }

    /// The sample rate the recording started at (for the elapsed time).
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Wait for the writer to finish the file. Call after the sink is gone,
    /// otherwise this waits until it is dropped.
    pub fn finish(self) -> Option<String> {
        let _ = self.handle.join();
        self.stats.error()
    }
}

type FileWriter = WavWriter<BufWriter<File>>;

/// Finish the current part (at `old_rate`) and open the next one at `new_rate`.
fn rotate(
    w: FileWriter,
    spec: &RecorderSpec,
    stats: &RecStats,
    old_rate: u32,
    new_rate: u32,
    part: &mut u32,
) -> io::Result<FileWriter> {
    w.finish(&comment(&spec.info, old_rate, *part))?;
    *part += 1;
    let path = part_path(&spec.path, *part);
    let next = open(&path, spec.channels, new_rate)?;
    if let Ok(mut c) = stats.current.lock() {
        *c = path;
    }
    Ok(next)
}

fn writer_loop(spec: RecorderSpec, first: FileWriter, rx: Receiver<Msg>, stats: Arc<RecStats>) {
    let mut w = Some(first);
    let mut part = 1u32;
    let mut rate = spec.sample_rate;
    for msg in rx {
        // After a failure, drain until stopped.
        let Some(cur) = w.take() else { continue };
        let res = match msg {
            Msg::Rate(r) => {
                let old = rate;
                rate = r;
                rotate(cur, &spec, &stats, old, r, &mut part)
            }
            Msg::Data(block) => {
                let bytes = block.len() as u64 * 2;
                let full = cur.data_bytes() > 0 && cur.data_bytes() + bytes > spec.max_data_bytes;
                let target = if full {
                    rotate(cur, &spec, &stats, rate, rate, &mut part)
                } else {
                    Ok(cur)
                };
                target.and_then(|mut x| {
                    x.write_f32(&block)?;
                    stats.bytes.fetch_add(bytes, Ordering::Relaxed);
                    let frames = block.len() as u64 / spec.channels.max(1) as u64;
                    stats.frames.fetch_add(frames, Ordering::Relaxed);
                    stats
                        .micros
                        .fetch_add(frames * 1_000_000 / rate.max(1) as u64, Ordering::Relaxed);
                    Ok(x)
                })
            }
        };
        match res {
            Ok(x) => w = Some(x),
            Err(e) => stats.set_error(e),
        }
    }
    if let Some(x) = w
        && let Err(e) = x.finish(&comment(&spec.info, rate, part))
    {
        stats.set_error(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::wav::read_info;

    /// A fresh, empty scratch folder for one test.
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shellsdr-rec-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn info() -> Vec<(String, String)> {
        vec![("rec_type".into(), "baseband".into())]
    }

    #[test]
    fn records_blocks_and_finishes_with_metadata() {
        let dir = scratch("basic");
        let path = dir.join("sub/a.wav");
        let (sink, handle) = start(RecorderSpec::new(path.clone(), 2, 250_000, info())).unwrap();
        for _ in 0..10 {
            sink.push(vec![0.25; 2000]); // 1000 IQ frames
        }
        drop(sink);
        let stats = Arc::clone(&handle.stats);
        assert!(handle.finish().is_none(), "no error");
        assert_eq!(stats.frames.load(Ordering::Relaxed), 10_000);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
        let got = read_info(File::open(&path).unwrap()).unwrap();
        assert_eq!(got.channels, 2);
        assert_eq!(got.sample_rate, 250_000);
        assert_eq!(got.data_bytes, 40_000);
        assert_eq!(got.get("rec_type"), Some("baseband"));
        assert_eq!(got.get("sample_rate"), Some("250000"));
        assert_eq!(got.get("part"), Some("1"));
        assert!(got.get("end_time").is_some_and(|t| t.ends_with('Z')));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn never_overwrites_an_existing_file() {
        let dir = scratch("exists");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        std::fs::write(&path, b"keep").unwrap();
        assert!(start(RecorderSpec::new(path.clone(), 1, 48_000, info())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn splits_into_parts_at_the_size_limit() {
        let dir = scratch("split");
        let path = dir.join("a.wav");
        let mut spec = RecorderSpec::new(path.clone(), 1, 48_000, info());
        spec.max_data_bytes = 1000; // 500 mono samples per part
        let (sink, handle) = start(spec).unwrap();
        for _ in 0..5 {
            sink.push(vec![0.1; 300]); // 600 bytes each
        }
        drop(sink);
        handle.finish();
        let p1 = read_info(File::open(&path).unwrap()).unwrap();
        let p2 = read_info(File::open(dir.join("a_part2.wav")).unwrap()).unwrap();
        assert_eq!(p1.data_bytes, 600, "a block never straddles two files");
        assert_eq!(p2.data_bytes, 600);
        assert_eq!(p2.get("part"), Some("2"));
        assert!(dir.join("a_part5.wav").exists());
        assert!(!dir.join("a_part6.wav").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sample_rate_change_continues_in_a_new_part() {
        let dir = scratch("rate");
        let path = dir.join("bb.wav");
        let (mut sink, handle) =
            start(RecorderSpec::new(path.clone(), 2, 2_000_000, info())).unwrap();
        sink.push(vec![0.0; 200]);
        sink.set_sample_rate(250_000);
        sink.push(vec![0.0; 400]);
        drop(sink);
        let stats = Arc::clone(&handle.stats);
        handle.finish();
        assert_eq!(stats.current(), dir.join("bb_part2.wav"));
        let p1 = read_info(File::open(&path).unwrap()).unwrap();
        let p2 = read_info(File::open(dir.join("bb_part2.wav")).unwrap()).unwrap();
        assert_eq!((p1.sample_rate, p1.data_bytes), (2_000_000, 400));
        assert_eq!((p2.sample_rate, p2.data_bytes), (250_000, 800));
        // 100 frames at 2 MHz (50 us) + 200 frames at 250 kHz (800 us).
        assert_eq!(stats.micros.load(Ordering::Relaxed), 850);
        assert_eq!(p2.get("sample_rate"), Some("250000"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
