//! The C API of the FAX library (`libfax.so.1`, FAX's `include/fax/fax_c.h`),
//! loaded at run time (spec 13.10, 15.1), and safe wrappers over it.
//!
//! Structs mirror the C ones (`#[repr(C)]`, the size first, so an older or
//! newer library fills in only what both know).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// The file the library is loaded from.
pub const LIB_FILE: &str = "libfax.so.1";
/// The C API version this build needs (`FAX_C_API_VERSION`).
pub const API_VERSION: c_int = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LiveOptions {
    pub size: usize,
    pub lpm: i32,
    pub width: i32,
    pub carrier: f64,
    pub deviation: f64,
    pub bandwidth: i32,
    pub decimate: i32,
    pub clock_ppm: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveStatus {
    pub size: usize,
    pub signal: i32,
    pub start_pending: i32,
    pub seconds: f64,
    pub cache_bytes: u64,
    pub generation: u64,
    pub pictures: i32,
    pub width: i32,
    pub sample_rate: f64,
    pub samples_per_line: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PictureInfo {
    pub size: usize,
    pub id: u64,
    pub open: i32,
    pub started: i32,
    pub aligned: i32,
    pub ppm: f64,
    pub start_seconds: f64,
    pub first_line: i32,
    pub lines: i32,
}

#[repr(C)]
pub struct RawLive {
    _p: [u8; 0],
}
#[repr(C)]
pub struct RawSlant {
    _p: [u8; 0],
}
#[repr(C)]
pub struct RawPng {
    _p: [u8; 0],
}

type LogFn = extern "C" fn(*mut c_void, c_int, *const c_char);

/// The library's functions.
pub struct FaxLib {
    _lib: libloading::Library,
    api_version: unsafe extern "C" fn() -> c_int,
    version: unsafe extern "C" fn() -> *const c_char,
    last_error: unsafe extern "C" fn() -> *const c_char,
    free: unsafe extern "C" fn(*mut c_void),
    options_init: unsafe extern "C" fn(*mut LiveOptions),
    live_new:
        unsafe extern "C" fn(*const LiveOptions, f64, Option<LogFn>, *mut c_void) -> *mut RawLive,
    live_free: unsafe extern "C" fn(*mut RawLive),
    live_push: unsafe extern "C" fn(*mut RawLive, *const i16, usize),
    live_set_cache_limit: unsafe extern "C" fn(*mut RawLive, u64),
    live_status: unsafe extern "C" fn(*const RawLive, *mut LiveStatus) -> c_int,
    live_picture: unsafe extern "C" fn(*const RawLive, i32, *mut PictureInfo) -> c_int,
    live_picture_by_id: unsafe extern "C" fn(*const RawLive, u64, *mut PictureInfo) -> c_int,
    live_set_ppm: unsafe extern "C" fn(*mut RawLive, u64, f64) -> c_int,
    live_render_line: unsafe extern "C" fn(*const RawLive, u64, i32, *mut u8, i32) -> c_int,
    live_render: unsafe extern "C" fn(*const RawLive, u64, i32, i32, *mut u8, i32, i32) -> c_int,
    live_release_before: unsafe extern "C" fn(*mut RawLive, u64, i32),
    slant_new: unsafe extern "C" fn(*const RawLive, u64, i32) -> *mut RawSlant,
    slant_run: unsafe extern "C" fn(*mut RawSlant),
    slant_result:
        unsafe extern "C" fn(*const RawSlant, *mut i32, *mut f64, *mut f64, *mut i32) -> c_int,
    slant_free: unsafe extern "C" fn(*mut RawSlant),
    png_open: unsafe extern "C" fn(*const c_char, i32) -> *mut RawPng,
    png_write_line: unsafe extern "C" fn(*mut RawPng, *const u8) -> c_int,
    png_sync: unsafe extern "C" fn(*mut RawPng) -> c_int,
    png_close: unsafe extern "C" fn(*mut RawPng) -> c_int,
    png_encode: unsafe extern "C" fn(*const u8, i32, i32, *mut usize) -> *mut u8,
}

impl FaxLib {
    /// Load the library from `path`.
    pub fn open(path: &Path) -> Result<Self, String> {
        // SAFETY: ShellSDR's own type library (see `vfo::types::libs`).
        let lib = unsafe { libloading::Library::new(path) }
            .map_err(|e| format!("cannot load {}: {e}", path.display()))?;
        Self::from_library(lib)
    }

    /// Load it from the library folders (spec 13.10).
    pub fn open_default() -> Result<Self, String> {
        Self::from_library(crate::vfo::types::libs::open(LIB_FILE)?)
    }

    fn from_library(lib: libloading::Library) -> Result<Self, String> {
        macro_rules! sym {
            ($name:literal) => {
                // SAFETY: the signatures match fax_c.h of FAX_C_API_VERSION 1.
                *unsafe { lib.get(concat!($name, "\0").as_bytes()) }
                    .map_err(|e| format!("{LIB_FILE}: {e}"))?
            };
        }
        let f = Self {
            api_version: sym!("fax_api_version"),
            version: sym!("fax_version"),
            last_error: sym!("fax_last_error"),
            free: sym!("fax_free"),
            options_init: sym!("fax_live_options_init"),
            live_new: sym!("fax_live_new"),
            live_free: sym!("fax_live_free"),
            live_push: sym!("fax_live_push"),
            live_set_cache_limit: sym!("fax_live_set_cache_limit"),
            live_status: sym!("fax_live_status_get"),
            live_picture: sym!("fax_live_picture"),
            live_picture_by_id: sym!("fax_live_picture_by_id"),
            live_set_ppm: sym!("fax_live_set_ppm"),
            live_render_line: sym!("fax_live_render_line"),
            live_render: sym!("fax_live_render"),
            live_release_before: sym!("fax_live_release_before"),
            slant_new: sym!("fax_slant_new"),
            slant_run: sym!("fax_slant_run"),
            slant_result: sym!("fax_slant_result"),
            slant_free: sym!("fax_slant_free"),
            png_open: sym!("fax_png_open"),
            png_write_line: sym!("fax_png_write_line"),
            png_sync: sym!("fax_png_sync"),
            png_close: sym!("fax_png_close"),
            png_encode: sym!("fax_png_encode"),
            _lib: lib,
        };
        // SAFETY: no arguments.
        let v = unsafe { (f.api_version)() };
        if v < API_VERSION {
            return Err(format!("{LIB_FILE} has C API {v}, {API_VERSION} is needed"));
        }
        Ok(f)
    }

    /// The library's version, e.g. `1.3.0`.
    pub fn version(&self) -> String {
        // SAFETY: a static string.
        unsafe { CStr::from_ptr((self.version)()) }
            .to_string_lossy()
            .into_owned()
    }

    fn error(&self) -> String {
        // SAFETY: a thread-local string of the library.
        unsafe { CStr::from_ptr((self.last_error)()) }
            .to_string_lossy()
            .into_owned()
    }

    /// A whole grey image as the bytes of a PNG file.
    pub fn encode_png(
        &self,
        pixels: &[u8],
        width: usize,
        height: usize,
    ) -> Result<Vec<u8>, String> {
        if width == 0 || height == 0 || pixels.len() < width * height {
            return Err("bad image size".into());
        }
        let mut size = 0usize;
        // SAFETY: pixels holds width * height bytes; the result is freed below.
        let p =
            unsafe { (self.png_encode)(pixels.as_ptr(), width as i32, height as i32, &mut size) };
        if p.is_null() {
            return Err(self.error());
        }
        // SAFETY: the library returned `size` bytes at `p`.
        let out = unsafe { std::slice::from_raw_parts(p, size) }.to_vec();
        // SAFETY: allocated by the library.
        unsafe { (self.free)(p.cast()) };
        Ok(out)
    }
}

/// The library of this process, loaded once (the reason if it cannot be).
pub fn shared() -> Result<Arc<FaxLib>, String> {
    static LIB: OnceLock<Result<Arc<FaxLib>, String>> = OnceLock::new();
    LIB.get_or_init(|| FaxLib::open_default().map(Arc::new))
        .clone()
}

extern "C" fn log_to_log_crate(_: *mut c_void, level: c_int, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    // SAFETY: a NUL-terminated message for the duration of the call.
    let m = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    match level {
        0 => log::trace!("fax: {m}"),
        1 => log::debug!("fax: {m}"),
        2 => log::warn!("fax: {m}"),
        _ => log::error!("fax: {m}"),
    }
}

/// Decoder settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub lpm: i32,
    pub width: i32,
    /// The clock correction of the first picture.
    pub clock_ppm: f64,
}

/// A live decoder (`fax_live`): for one thread.
pub struct Live {
    lib: Arc<FaxLib>,
    raw: *mut RawLive,
}

// SAFETY: a fax_live may move between threads; it is only used by one at a time (&mut / &self
// from the owner).
unsafe impl Send for Live {}

impl Live {
    pub fn new(lib: Arc<FaxLib>, config: Config, sample_rate: f64) -> Result<Self, String> {
        let mut o = LiveOptions {
            size: std::mem::size_of::<LiveOptions>(),
            lpm: 0,
            width: 0,
            carrier: 0.0,
            deviation: 0.0,
            bandwidth: 0,
            decimate: 0,
            clock_ppm: 0.0,
        };
        // SAFETY: o is a valid struct of the declared size.
        unsafe { (lib.options_init)(&mut o) };
        o.lpm = config.lpm;
        o.width = config.width;
        o.decimate = 1;
        o.clock_ppm = config.clock_ppm;
        // SAFETY: valid options; the log function is 'static and ignores its user pointer.
        let raw = unsafe {
            (lib.live_new)(
                &o,
                sample_rate,
                Some(log_to_log_crate),
                std::ptr::null_mut(),
            )
        };
        if raw.is_null() {
            return Err(lib.error());
        }
        Ok(Self { lib, raw })
    }

    pub fn lib(&self) -> &Arc<FaxLib> {
        &self.lib
    }

    pub fn push(&mut self, samples: &[i16]) {
        // SAFETY: a valid decoder and slice.
        unsafe { (self.lib.live_push)(self.raw, samples.as_ptr(), samples.len()) }
    }

    pub fn set_cache_limit(&mut self, bytes: u64) {
        // SAFETY: a valid decoder.
        unsafe { (self.lib.live_set_cache_limit)(self.raw, bytes) }
    }

    pub fn status(&self) -> LiveStatus {
        let mut s = LiveStatus {
            size: std::mem::size_of::<LiveStatus>(),
            ..Default::default()
        };
        // SAFETY: a valid decoder and struct.
        unsafe { (self.lib.live_status)(self.raw, &mut s) };
        s
    }

    /// All pictures, oldest first.
    pub fn pictures(&self) -> Vec<PictureInfo> {
        let n = self.status().pictures.max(0);
        (0..n)
            .filter_map(|i| {
                let mut p = PictureInfo {
                    size: std::mem::size_of::<PictureInfo>(),
                    ..Default::default()
                };
                // SAFETY: a valid decoder and struct.
                (unsafe { (self.lib.live_picture)(self.raw, i, &mut p) } == 0).then_some(p)
            })
            .collect()
    }

    pub fn picture(&self, id: u64) -> Option<PictureInfo> {
        let mut p = PictureInfo {
            size: std::mem::size_of::<PictureInfo>(),
            ..Default::default()
        };
        // SAFETY: a valid decoder and struct.
        (unsafe { (self.lib.live_picture_by_id)(self.raw, id, &mut p) } == 0).then_some(p)
    }

    pub fn set_ppm(&mut self, id: u64, ppm: f64) -> bool {
        // SAFETY: a valid decoder.
        unsafe { (self.lib.live_set_ppm)(self.raw, id, ppm) == 0 }
    }

    /// One line, `out.len()` pixels wide. False if it is not in memory.
    pub fn render_line(&self, id: u64, line: i32, out: &mut [u8]) -> bool {
        if out.is_empty() {
            return false;
        }
        // SAFETY: a valid decoder; out holds out.len() bytes.
        unsafe {
            (self.lib.live_render_line)(self.raw, id, line, out.as_mut_ptr(), out.len() as i32) == 0
        }
    }

    /// Lines `first..first+lines` scaled to `w` x `h` (white where missing).
    pub fn render(&self, id: u64, first: i32, lines: i32, w: usize, h: usize) -> Option<Vec<u8>> {
        if w == 0 || h == 0 || lines <= 0 {
            return None;
        }
        let mut out = vec![255u8; w * h];
        // SAFETY: a valid decoder; out holds w * h bytes.
        let rc = unsafe {
            (self.lib.live_render)(
                self.raw,
                id,
                first,
                lines,
                out.as_mut_ptr(),
                w as i32,
                h as i32,
            )
        };
        (rc == 0).then_some(out)
    }

    pub fn release_before(&mut self, id: u64, line: i32) {
        // SAFETY: a valid decoder.
        unsafe { (self.lib.live_release_before)(self.raw, id, line) }
    }

    /// A slant measurement of the last `max_lines` lines (None: too few).
    pub fn slant_job(&self, id: u64, max_lines: i32) -> Option<SlantJob> {
        // SAFETY: a valid decoder.
        let raw = unsafe { (self.lib.slant_new)(self.raw, id, max_lines) };
        (!raw.is_null()).then(|| SlantJob {
            lib: self.lib.clone(),
            raw,
            picture: id,
        })
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // SAFETY: created by live_new, freed once.
        unsafe { (self.lib.live_free)(self.raw) }
    }
}

/// The result of a slant measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slant {
    pub valid: bool,
    /// The picture's total clock correction to use.
    pub ppm: f64,
    pub confidence: f64,
    pub lines: i32,
}

/// A slant measurement: owns a copy of the lines, runs in any thread.
pub struct SlantJob {
    lib: Arc<FaxLib>,
    raw: *mut RawSlant,
    picture: u64,
}

// SAFETY: fax_slant jobs hold their own data and may run in another thread (fax_c.h).
unsafe impl Send for SlantJob {}

impl SlantJob {
    pub fn picture(&self) -> u64 {
        self.picture
    }

    /// The slow part (about 1 s per 1000 lines on a Pi 3).
    pub fn run(self) -> Slant {
        let (mut valid, mut ppm, mut confidence, mut lines) = (0i32, 0.0, 0.0, 0i32);
        // SAFETY: a valid job, run once, then read.
        unsafe {
            (self.lib.slant_run)(self.raw);
            (self.lib.slant_result)(self.raw, &mut valid, &mut ppm, &mut confidence, &mut lines);
        }
        Slant {
            valid: valid != 0,
            ppm,
            confidence,
            lines,
        }
    }
}

impl Drop for SlantJob {
    fn drop(&mut self) {
        // SAFETY: created by slant_new, freed once.
        unsafe { (self.lib.slant_free)(self.raw) }
    }
}

/// A PNG file that is valid while it grows (`fax_png`).
pub struct PngFile {
    lib: Arc<FaxLib>,
    raw: *mut RawPng,
    lines: usize,
}

// SAFETY: used by one thread at a time.
unsafe impl Send for PngFile {}

impl PngFile {
    pub fn create(lib: Arc<FaxLib>, path: &Path, width: usize) -> Result<Self, String> {
        let c = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| "a NUL in the path".to_string())?;
        // SAFETY: a valid C string.
        let raw = unsafe { (lib.png_open)(c.as_ptr(), width as i32) };
        if raw.is_null() {
            return Err(lib.error());
        }
        Ok(Self { lib, raw, lines: 0 })
    }

    pub fn write_line(&mut self, pixels: &[u8]) -> Result<(), String> {
        // SAFETY: a valid writer; pixels holds the width (checked by the caller).
        if unsafe { (self.lib.png_write_line)(self.raw, pixels.as_ptr()) } != 0 {
            return Err(self.lib.error());
        }
        self.lines += 1;
        Ok(())
    }

    /// Make the lines written so far readable in the file.
    pub fn sync(&mut self) -> Result<(), String> {
        // SAFETY: a valid writer.
        if unsafe { (self.lib.png_sync)(self.raw) } != 0 {
            return Err(self.lib.error());
        }
        Ok(())
    }

    pub fn lines(&self) -> usize {
        self.lines
    }

    pub fn close(mut self) -> Result<(), String> {
        let raw = std::mem::replace(&mut self.raw, std::ptr::null_mut());
        // SAFETY: closes and frees once (Drop skips a null writer).
        if unsafe { (self.lib.png_close)(raw) } != 0 {
            return Err(self.lib.error());
        }
        Ok(())
    }
}

impl Drop for PngFile {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: closes and frees once.
            unsafe { (self.lib.png_close)(self.raw) };
        }
    }
}
