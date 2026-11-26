//! The run loop's part of sharing (0.4.1, spec 22.2, 22.5): the server
//! follows the Source module's sharing rows; a viewer's input is applied on
//! its own screen, its frames are rendered at its size and sent as changed
//! cells, the engine's audio is copied to it.

use std::time::{Duration, Instant};

use crate::app::AppState;
use crate::engine::EngineHandle;
use crate::remote::cells::Grid;
use crate::remote::share::{ShareEvent, ShareServer};
use crate::report::model::{Category, Level};
use crate::ui::mouse::HitMap;
use crate::ui::screen::{ScreenLocal, handle_event, prepare, render_offscreen};

/// Frames a viewer gets at most (one each 50 ms).
const FRAME_GAP: Duration = Duration::from_millis(50);
/// A picture's pixels at most this often (a FAX picture follows its new
/// lines in steps, not with every line).
const PICTURE_GAP: Duration = Duration::from_millis(500);

/// A connected viewer's screen.
struct ViewerScreen {
    peer: String,
    local: ScreenLocal,
    cols: u16,
    rows: u16,
    /// What it shows (None: send everything).
    last: Option<Grid>,
    /// The clickable regions of its last frame.
    hits: HitMap,
    at: Option<Instant>,
    /// A graphical viewer: it gets the overlays and spectrum rows.
    overlays: bool,
    bins: u32,
    /// The overlays it has.
    sent_overlays: Option<Vec<crate::remote::wire::Overlay>>,
    /// How its audio travels ("Opus 32 kb/s").
    audio: String,
    /// It takes pictures (0.4.8): the pixels it has of each slot, and when
    /// they were sent.
    pictures: bool,
    sent_pictures: std::collections::HashMap<u8, (u64, Instant)>,
}

impl ViewerScreen {
    /// The Source module's viewer line: "10.0.0.2 (window, 120x40, Opus 32
    /// kb/s)".
    fn describe(&self) -> String {
        let what = if self.overlays { "window" } else { "terminal" };
        format!(
            "{} ({what}, {}x{}, {})",
            self.peer, self.cols, self.rows, self.audio
        )
    }
}

/// The sharing state the run loop keeps.
#[derive(Default)]
pub struct ShareLoop {
    server: Option<ShareServer>,
    /// What the server was started with (listen, token).
    key: Option<(String, Option<String>)>,
    viewer: Option<ViewerScreen>,
}

impl ShareLoop {
    /// One turn of the loop.
    pub fn tick(&mut self, state: &mut AppState, engine: Option<&dyn EngineHandle>) {
        self.follow_settings(state);
        let Some(server) = &self.server else {
            if let Some(e) = engine {
                e.set_audio_tee(None);
            }
            return;
        };
        if std::mem::take(&mut state.source.share.disconnect) {
            server.disconnect();
        }
        for ev in server.events() {
            match ev {
                ShareEvent::Connected {
                    peer,
                    cols,
                    rows,
                    overlays,
                    bins,
                    audio,
                    pictures,
                } => {
                    let v = ViewerScreen {
                        peer,
                        local: ScreenLocal::remote(state.fax_ui.cell_px(), pictures),
                        cols,
                        rows,
                        last: None,
                        hits: HitMap::default(),
                        at: None,
                        overlays,
                        bins,
                        sent_overlays: None,
                        audio,
                        pictures,
                        sent_pictures: Default::default(),
                    };
                    state.source.share.viewer = Some(v.describe());
                    state.report_event(
                        Level::Info,
                        Category::App,
                        format!("Viewer connected: {}", v.describe()),
                        None,
                    );
                    self.viewer = Some(v);
                }
                ShareEvent::Audio(audio) => {
                    if let Some(v) = &mut self.viewer {
                        v.audio = audio;
                        state.source.share.viewer = Some(v.describe());
                        state.report_event(
                            Level::Info,
                            Category::App,
                            format!("Viewer's audio now {}", v.audio),
                            None,
                        );
                    }
                }
                ShareEvent::Resize { cols, rows } => {
                    if let Some(v) = &mut self.viewer {
                        v.cols = cols;
                        v.rows = rows;
                        state.source.share.viewer = Some(v.describe());
                    }
                }
                ShareEvent::Input(event) => {
                    if let Some(v) = &mut self.viewer {
                        v.local.swap(state);
                        handle_event(state, &v.hits, event);
                        v.local.swap(state);
                        // the program is quit where it runs
                        if !state.running {
                            state.running = true;
                            state.show_notice(
                                "A viewer cannot quit ShellSDR: quit it on its machine".into(),
                            );
                        }
                        // the next frame at once: the viewer waits for it
                        v.at = None;
                    }
                }
                ShareEvent::Disconnected { peer, reason } => {
                    self.viewer = None;
                    state.source.share.viewer = None;
                    state.remote_shows = (false, false);
                    state.report_event(
                        Level::Info,
                        Category::App,
                        format!("Viewer left: {peer} ({reason})"),
                        None,
                    );
                }
                ShareEvent::Refused { peer, reason } => state.report_event(
                    Level::Warning,
                    Category::App,
                    format!("Viewer refused: {peer} ({reason})"),
                    None,
                ),
            }
        }
        if let Some(e) = engine {
            e.set_audio_tee(self.viewer.as_ref().map(|_| server.audio_sender()));
        }
        let Some(v) = &mut self.viewer else {
            return;
        };
        if v.at.is_some_and(|t| t.elapsed() < FRAME_GAP) || !server.ready_for_cells() {
            return;
        }
        v.local.swap(state);
        let full = ratatui::layout::Rect::new(0, 0, v.cols.max(1), v.rows.max(1));
        prepare(state, full);
        let (buf, hits) = render_offscreen(state, full.width, full.height);
        v.local.swap(state);
        state.remote_shows = v.local.shows();
        v.at = Some(Instant::now());
        let grid = Grid::from_buffer(&buf);
        let diff = grid.diff(v.last.as_ref());
        if !diff.is_empty() {
            server.send_cells(diff);
        }
        v.last = Some(grid);
        if v.overlays {
            // nothing drawn over a dialog
            let mut items = if modal_open(state) {
                Vec::new()
            } else {
                hits.overlays.clone()
            };
            if !v.pictures {
                // an older viewer would refuse the whole message
                items.retain(|o| !matches!(o, crate::remote::wire::Overlay::Picture { .. }));
            }
            if v.sent_overlays.as_ref() != Some(&items) {
                server.send_overlays(items.clone());
                v.sent_overlays = Some(items);
            }
            if v.pictures {
                for p in &hits.pictures {
                    let due = match v.sent_pictures.get(&p.slot) {
                        Some((key, _)) if *key == p.key => false,
                        Some((_, at)) => at.elapsed() >= PICTURE_GAP,
                        None => true,
                    };
                    if due && server.send_picture(std::sync::Arc::clone(p)) {
                        v.sent_pictures.insert(p.slot, (p.key, Instant::now()));
                    }
                }
            }
        }
        v.hits = hits;
    }

    /// A new spectrum row of an overlay stream: a graphical viewer's
    /// waterfalls get every one.
    pub fn on_rows(&self, stream: u8, row: &[f32]) {
        if let (Some(s), Some(v)) = (&self.server, &self.viewer)
            && v.overlays
        {
            s.send_rows(stream, row, v.bins);
        }
    }

    /// Start, restart or stop the server as the settings say.
    fn follow_settings(&mut self, state: &mut AppState) {
        let sh = &state.source.share;
        let want = sh.on.then(|| (sh.listen.trim().to_string(), sh.token()));
        if want == self.key {
            return;
        }
        if let Some(v) = self.viewer.take() {
            state.report_event(
                Level::Info,
                Category::App,
                format!("Viewer left: {} (sharing changed)", v.peer),
                None,
            );
        }
        self.server = None;
        state.source.share.viewer = None;
        state.remote_shows = (false, false);
        self.key = want.clone();
        let Some((listen, token)) = want else {
            state.source.share.status = None;
            return;
        };
        match ShareServer::start(&listen, token.clone()) {
            Ok(s) => {
                let warn = if s.is_public() && token.is_none() {
                    " (no token: anyone can connect!)"
                } else {
                    ""
                };
                state.source.share.status =
                    Some(Ok(format!("waiting on {}{warn}", s.local_addr())));
                state.report_event(
                    Level::Info,
                    Category::App,
                    format!("Sharing on {}", s.local_addr()),
                    None,
                );
                self.server = Some(s);
            }
            Err(e) => {
                state.report_event(
                    Level::Warning,
                    Category::App,
                    format!("Cannot share: {e}"),
                    None,
                );
                state.source.share.status = Some(Err(e));
            }
        }
    }
}

/// Whether a dialog may be over the plots (no overlays then).
pub fn modal_open(s: &AppState) -> bool {
    s.popup_open()
        || s.help.is_some()
        || s.prompt.is_some()
        || s.file_browser.is_some()
        || s.bm_editor.is_some()
        || s.sched_editor.is_some()
        || s.fax_ui.menu.is_some()
        || s.digital_ui.menu.is_some()
}
