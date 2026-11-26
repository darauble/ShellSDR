//! `shellsdr --gui` (0.4.1, spec 22.6): ShellSDR in an egui window, with
//! the same functions as in a terminal. The application loop runs on its
//! own thread and draws into an in-memory terminal of the window's size
//! ([`GuiFront`]); the window paints that screen, the spectrum and the
//! waterfall at pixel resolution over their rectangles, and sends its keys
//! and clicks back. A sharing ShellSDR is viewed from it as from the
//! terminal: the Network source's `[Open window]`.

mod icon;
pub mod keys;
pub mod paint;

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, FontFamily, FontId, Pos2, Vec2};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::AppState;
use crate::remote::cells::Grid;
use crate::remote::wire::Overlay;

/// What the window remembers (`~/.config/ShellSDR/gui.toml`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GuiPrefs {
    pub font_size: f32,
    /// The window's size in points.
    pub width: f32,
    pub height: f32,
}

impl Default for GuiPrefs {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            width: 1400.0,
            height: 900.0,
        }
    }
}

impl GuiPrefs {
    fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &std::path::Path) {
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(s) = toml::to_string(self) {
            let _ = std::fs::write(path, s);
        }
    }
}

/// What the loop and the window share.
#[derive(Default)]
struct Shared {
    /// The screen the loop drew last, and what is drawn over it.
    grid: Grid,
    overlays: Vec<Overlay>,
    /// Spectrum rows the window has not taken, oldest first.
    rows: VecDeque<(u8, Vec<f32>)>,
    /// The pictures' pixels, one per slot (0.4.8).
    pictures: Vec<Arc<crate::remote::picture::Picture>>,
    /// The size the window has room for (cols, rows) and a cell in pixels.
    want: (u16, u16),
    cell_px: (u16, u16),
}

/// The loop's side of the window.
pub struct GuiFront {
    shared: Arc<Mutex<Shared>>,
    input: Receiver<crossterm::event::Event>,
    /// The window asks to close (a quit request) or is gone.
    close: Arc<AtomicBool>,
    gone: Arc<AtomicBool>,
    ctx: egui::Context,
    /// An input taken by `poll`, handed out by `read`.
    pending: Option<crossterm::event::Event>,
}

impl crate::ui::front::Front<TestBackend> for GuiFront {
    fn before_draw(&mut self, t: &mut Terminal<TestBackend>, state: &mut AppState) {
        let (want, cell) = self
            .shared
            .lock()
            .map(|s| (s.want, s.cell_px))
            .unwrap_or(((0, 0), (0, 0)));
        if want.0 > 0 && want.1 > 0 {
            let size = t.size().unwrap_or_default();
            if (size.width, size.height) != want {
                t.backend_mut().resize(want.0, want.1);
            }
        }
        state.fax_ui.cell = cell;
    }

    fn after_draw(
        &mut self,
        t: &Terminal<TestBackend>,
        overlays: Vec<Overlay>,
        rows: Vec<(u8, Vec<f32>)>,
        pictures: Vec<Arc<crate::remote::picture::Picture>>,
    ) {
        let grid = Grid::from_buffer(t.backend().buffer());
        if let Ok(mut s) = self.shared.lock() {
            s.grid = grid;
            s.overlays = overlays;
            s.pictures = pictures;
            for r in rows {
                push_row(&mut s.rows, r);
            }
        }
        self.ctx.request_repaint();
    }

    fn on_rows(&mut self, stream: u8, row: &[f32]) {
        if let Ok(mut s) = self.shared.lock() {
            push_row(&mut s.rows, (stream, row.to_vec()));
        }
    }

    fn poll(&mut self, d: Duration) -> io::Result<bool> {
        if self.pending.is_some() {
            return Ok(true);
        }
        if self.gone.load(Ordering::Relaxed) {
            return Ok(false);
        }
        self.pending = if d.is_zero() {
            self.input.try_recv().ok()
        } else {
            self.input.recv_timeout(d).ok()
        };
        Ok(self.pending.is_some())
    }

    fn read(&mut self) -> io::Result<crossterm::event::Event> {
        match self.pending.take() {
            Some(e) => Ok(e),
            None => self
                .input
                .recv()
                .map_err(|_| io::Error::other("the window is closed")),
        }
    }

    fn graphical(&self) -> bool {
        true
    }

    fn quit_requested(&mut self) -> bool {
        self.close.swap(false, Ordering::Relaxed)
    }

    fn gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }
}

/// Keep at most this many rows the window has not taken.
const MAX_ROWS: usize = 512;

fn push_row(rows: &mut VecDeque<(u8, Vec<f32>)>, r: (u8, Vec<f32>)) {
    if rows.len() >= MAX_ROWS {
        rows.pop_front();
    }
    rows.push_back(r);
}

/// Run ShellSDR in a window until it quits. `config_dir`: ShellSDR's config
/// folder (`gui.toml`, the window's own settings, is kept there).
pub fn run_app(state: &mut AppState, config_dir: PathBuf) -> io::Result<()> {
    preflight().map_err(io::Error::other)?;
    let path = config_dir.join("gui.toml");
    let prefs = GuiPrefs::load(&path);
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (tx, rx) = crossbeam_channel::unbounded();
    let close = Arc::new(AtomicBool::new(false));
    let gone = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let (ctx_tx, ctx_rx) = crossbeam_channel::bounded::<egui::Context>(1);
    let options = eframe::NativeOptions {
        viewport: {
            let v = egui::ViewportBuilder::default()
                .with_title("ShellSDR")
                .with_app_id("shellsdr")
                .with_inner_size([prefs.width, prefs.height]);
            match icon::icon() {
                Some(i) => v.with_icon(std::sync::Arc::new(i)),
                None => v,
            }
        },
        ..Default::default()
    };
    let mut result = Ok(());
    std::thread::scope(|scope| {
        // the application loop, once the window gave its context
        let loop_thread = {
            let (shared, close, gone, done) = (
                Arc::clone(&shared),
                Arc::clone(&close),
                Arc::clone(&gone),
                Arc::clone(&done),
            );
            scope.spawn(move || {
                let Ok(ctx) = ctx_rx.recv() else {
                    return Ok(());
                };
                let mut front = GuiFront {
                    shared,
                    input: rx,
                    close,
                    gone,
                    ctx: ctx.clone(),
                    pending: None,
                };
                let mut terminal = Terminal::new(TestBackend::new(160, 50))?;
                let r = crate::ui::run::event_loop(&mut terminal, state, &mut front);
                done.store(true, Ordering::Relaxed);
                ctx.request_repaint();
                r
            })
        };
        let app_shared = Arc::clone(&shared);
        let (app_close, app_done) = (Arc::clone(&close), Arc::clone(&done));
        let r = eframe::run_native(
            "ShellSDR",
            options,
            Box::new(move |cc| {
                set_fonts(&cc.egui_ctx);
                cc.egui_ctx.set_visuals(egui::Visuals::dark());
                let _ = ctx_tx.send(cc.egui_ctx.clone());
                Ok(Box::new(App {
                    shared: app_shared,
                    input: tx,
                    close: app_close,
                    done: app_done,
                    prefs,
                    path,
                    plots: paint::Plots::default(),
                    down: None,
                }))
            }),
        );
        // the window is gone: the loop ends (its engine stops cleanly)
        gone.store(true, Ordering::Relaxed);
        if let Err(e) = r {
            result = Err(io::Error::other(window_error(e)));
        }
        if let Ok(Err(e)) = loop_thread.join() {
            result = Err(e);
        }
    });
    result
}

/// A readable message for a window that could not open.
fn window_error(e: eframe::Error) -> String {
    let shown = std::env::var("WAYLAND_DISPLAY")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("DISPLAY").ok())
        .unwrap_or_default();
    format!(
        "cannot open the window on display {shown:?}: {}",
        e.to_string().rsplit(": ").next().unwrap_or("unknown error")
    )
}

/// Whether a window can be opened here: a display to open it on, and the
/// libraries the window loads at run time (the release build aborts on a
/// panic, so this is checked before eframe could fail deep inside).
fn preflight() -> Result<(), String> {
    let set = |v: &str| std::env::var_os(v).is_some_and(|s| !s.is_empty());
    let wayland = set("WAYLAND_DISPLAY");
    let x11 = set("DISPLAY");
    if !wayland && !x11 {
        return Err(
            "no graphical display here (neither DISPLAY nor WAYLAND_DISPLAY is set): \
             run shellsdr without --gui in a terminal"
                .into(),
        );
    }
    let loads = |names: &[&str]| {
        names
            .iter()
            // SAFETY: loading system libraries whose initializers are benign.
            .any(|n| unsafe { libloading::Library::new(n) }.is_ok())
    };
    let mut missing = Vec::new();
    let x11_ok = x11
        && loads(&["libX11.so.6", "libX11.so"])
        && loads(&["libxkbcommon-x11.so.0", "libxkbcommon-x11.so"]);
    let wayland_ok = wayland
        && loads(&["libwayland-client.so.0", "libwayland-client.so"])
        && loads(&["libxkbcommon.so.0", "libxkbcommon.so"]);
    if !x11_ok && !wayland_ok {
        missing.push(if wayland {
            "libwayland-client0 and libxkbcommon0"
        } else {
            "libx11-6 and libxkbcommon-x11-0"
        });
    }
    if !loads(&["libEGL.so.1", "libEGL.so", "libGL.so.1", "libGL.so"]) {
        missing.push("libegl1 (OpenGL)");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the window needs {} (sudo apt install ...); in a terminal ShellSDR works without them",
            missing.join(", ")
        ))
    }
}

/// A monospace font with the box drawing and blocks the screen uses
/// (DejaVu Sans Mono when installed; braille is drawn as dots).
fn set_fonts(ctx: &egui::Context) {
    let candidates = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    ];
    let Some(bytes) = candidates.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dejavu-mono".into(),
        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
    );
    if let Some(f) = fonts.families.get_mut(&FontFamily::Monospace) {
        f.insert(0, "dejavu-mono".into());
    }
    ctx.set_fonts(fonts);
}

/// The window.
struct App {
    shared: Arc<Mutex<Shared>>,
    input: Sender<crossterm::event::Event>,
    close: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    prefs: GuiPrefs,
    path: PathBuf,
    plots: paint::Plots,
    /// The button held down and the cell it went down on (its release goes
    /// to the same cell).
    down: Option<(egui::PointerButton, u16, u16)>,
}

impl App {
    fn font(&self) -> FontId {
        FontId::monospace(self.prefs.font_size)
    }

    /// One cell in points.
    fn cell(&self, ctx: &egui::Context) -> Vec2 {
        let f = self.font();
        ctx.fonts_mut(|fonts| Vec2::new(fonts.glyph_width(&f, 'M'), fonts.row_height(&f)))
    }

    fn send(&self, e: crossterm::event::Event) {
        let _ = self.input.send(e);
    }

    /// Ctrl+= / Ctrl+- / Ctrl+0: the window's own keys (the text size).
    fn is_window_key(key: egui::Key, m: egui::Modifiers) -> bool {
        m.ctrl
            && matches!(
                key,
                egui::Key::Equals | egui::Key::Plus | egui::Key::Minus | egui::Key::Num0
            )
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.done.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // closing the window is quitting: the program asks first if needed
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close.store(true, Ordering::Relaxed);
        }
        let zoom = ctx.input(|i| {
            let c = i.modifiers.ctrl;
            if c && (i.key_pressed(egui::Key::Equals) || i.key_pressed(egui::Key::Plus)) {
                1.0
            } else if c && i.key_pressed(egui::Key::Minus) {
                -1.0
            } else if c && i.key_pressed(egui::Key::Num0) {
                14.0 - self.prefs.font_size
            } else {
                0.0
            }
        });
        if zoom != 0.0 {
            self.prefs.font_size = (self.prefs.font_size + zoom).clamp(8.0, 32.0);
        }
        let size = ctx.content_rect().size();
        if zoom != 0.0
            || (size.x - self.prefs.width).abs() > 1.0
            || (size.y - self.prefs.height).abs() > 1.0
        {
            self.prefs.width = size.x;
            self.prefs.height = size.y;
            self.prefs.save(&self.path);
        }
        let cell = self.cell(&ctx);
        let font = self.font();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(paint::DEFAULT_BG))
            .show(ui, |ui| {
                let area = ui.max_rect();
                let cols = (area.width() / cell.x).floor().max(20.0) as u16;
                let rows = (area.height() / cell.y).floor().max(10.0) as u16;
                let ppp = ctx.pixels_per_point();
                let (grid, overlays, new_rows, pictures) = match self.shared.lock() {
                    Ok(mut s) => {
                        s.want = (cols, rows);
                        s.cell_px = ((cell.x * ppp) as u16, (cell.y * ppp) as u16);
                        let rows: Vec<(u8, Vec<f32>)> = s.rows.drain(..).collect();
                        (s.grid.clone(), s.overlays.clone(), rows, s.pictures.clone())
                    }
                    Err(_) => return,
                };
                let p = ui.painter_at(area);
                let origin = area.min;
                paint::grid(&p, origin, cell, &font, &grid);
                self.plots.push(new_rows);
                self.plots
                    .paint(&ctx, &p, origin, cell, &overlays, &pictures);
                // input as the terminal would give it
                let (gc, gr) = (grid.cols.max(1), grid.rows.max(1));
                let to_cell = |pos: Pos2| {
                    let d = pos - origin;
                    (
                        (d.x / cell.x).floor().clamp(0.0, gc as f32 - 1.0) as u16,
                        (d.y / cell.y).floor().clamp(0.0, gr as f32 - 1.0) as u16,
                    )
                };
                let events = ctx.input(|i| i.events.clone());
                // Alt+letter came as a key already (Alt+M: a window's mute)
                let alt = ctx.input(|i| i.modifiers.alt && !i.modifiers.ctrl);
                let hover = ctx.input(|i| i.pointer.hover_pos());
                for ev in events {
                    match ev {
                        egui::Event::Key {
                            key,
                            pressed: true,
                            modifiers,
                            ..
                        } => {
                            if !Self::is_window_key(key, modifiers)
                                && let Some(e) = keys::key(key, modifiers)
                            {
                                self.send(e);
                            }
                        }
                        // typed text; through an input method (IBus) letters
                        // come as a commit
                        egui::Event::Text(_) if alt => {}
                        egui::Event::Text(t) | egui::Event::Ime(egui::ImeEvent::Commit(t)) => {
                            for e in keys::text(&t) {
                                self.send(e);
                            }
                        }
                        egui::Event::PointerButton {
                            pos,
                            button,
                            pressed,
                            modifiers,
                        } => {
                            let at = if pressed {
                                if !area.contains(pos) {
                                    continue;
                                }
                                let (c, r) = to_cell(pos);
                                self.down = Some((button, c, r));
                                (c, r)
                            } else {
                                match self.down.take() {
                                    Some((b, c, r)) if b == button => (c, r),
                                    _ => continue,
                                }
                            };
                            if let Some(e) = keys::click(button, pressed, at.0, at.1, modifiers) {
                                self.send(e);
                            }
                        }
                        egui::Event::MouseWheel {
                            delta, modifiers, ..
                        } if delta.y != 0.0 => {
                            if let Some(pos) = hover.filter(|p| area.contains(*p)) {
                                let (c, r) = to_cell(pos);
                                self.send(keys::wheel(delta.y, c, r, modifiers));
                            }
                        }
                        _ => {}
                    }
                }
            });
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
