//! The terminal viewer (0.4.1, spec 22.3): while a window into a sharing
//! ShellSDR is open, this terminal shows its screen under a one-line bar of
//! its own and sends it every key and click. The bar holds what stays here
//! (0.4.3): `[Back]` (F12 or Ctrl+]), how the audio travels (a click or
//! Alt+C changes the compression), and the audio heard here, `[Mute]`
//! (Alt+M) and the volume (Ctrl+Up/Down). The local application keeps
//! running underneath, unseen.

use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::style::{Modifier, Style};

use crate::app::AppState;
use crate::remote::viewer::{Viewer, WindowReq};

/// The rows above the shared screen (the bar).
pub const BAR_ROWS: u16 = 1;

/// What a click on the bar does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarHit {
    Back,
    /// The compression: a left click steps on, a right click back.
    Audio,
    Mute,
    VolDown,
    VolUp,
}

/// An open window.
pub struct TermViewer {
    v: Viewer,
    /// The volume here, 0.0..=1.0 (the server's own is the shared screen's).
    volume: f32,
    /// Muted here.
    muted: bool,
    /// The compression asked for (`NET_BITRATES`; 0: PCM).
    bitrate: u32,
    /// A message shown in the bar for a while (a change refused).
    note: Option<(String, Instant)>,
}

/// How long a note stays in the bar.
const NOTE: Duration = Duration::from_secs(5);

impl TermViewer {
    /// Open the window the Network source points at, `cols` x `rows` with
    /// the bar; an error goes to the Network view's status line.
    pub fn open(state: &mut AppState, cols: u16, rows: u16, graphical: bool) -> Option<Self> {
        let n = &state.source.network;
        let req = WindowReq {
            addr: n.addr.trim().to_string(),
            token: n.token(),
            cols,
            rows: rows.saturating_sub(BAR_ROWS).max(1),
            overlays: graphical,
            bins: if graphical { 2048 } else { 0 },
            bitrate: n.bitrate,
            audio_target: crate::engine::audio_target::AudioTarget::from_stored(
                state.radio.active().audio_device.as_deref(),
            ),
            audio_tap: false,
        };
        let _hush = crate::engine::silence::StdioSilencer::new();
        match Viewer::open(&req) {
            Ok(v) => {
                state.report_event(
                    crate::report::model::Level::Info,
                    crate::report::model::Category::Source,
                    format!("Window opened: {} on {}", v.host, req.addr),
                    None,
                );
                Some(Self {
                    v,
                    volume: 1.0,
                    muted: false,
                    bitrate: req.bitrate,
                    note: None,
                })
            }
            Err(e) => {
                state.source.network.status = Some(Err(e));
                None
            }
        }
    }

    /// The remote screen's overlays, placed under the bar (a graphical
    /// front draws them).
    pub fn overlays(&self) -> Vec<crate::remote::wire::Overlay> {
        self.v
            .overlays()
            .into_iter()
            .map(|o| o.shifted(BAR_ROWS))
            .collect()
    }

    /// The remote's pictures (their pixels, by slot).
    pub fn pictures(&self) -> Vec<std::sync::Arc<crate::remote::picture::Picture>> {
        self.v.pictures()
    }

    /// The remote's spectrum rows since the last call.
    pub fn take_rows(&self) -> Vec<(u8, Vec<f32>)> {
        self.v.take_rows()
    }

    /// The compression asked for (kept in the Network source).
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Step the compression through `NET_BITRATES`.
    fn step_audio(&mut self, dir: i32) {
        use crate::source::model::NET_BITRATES;
        let n = NET_BITRATES.len() as i32;
        let i = NET_BITRATES
            .iter()
            .position(|b| *b == self.bitrate)
            .unwrap_or(2) as i32;
        let b = NET_BITRATES[(i + dir).rem_euclid(n) as usize];
        match self.v.set_audio(b) {
            Ok(()) => self.bitrate = b,
            Err(e) => self.note = Some((e, Instant::now())),
        }
    }

    /// Why it closed, once it did.
    pub fn closed(&self) -> Option<String> {
        self.v.closed()
    }

    /// The bar's pieces, left to right, and what a click on each does.
    fn bar(&self) -> Vec<(String, Option<BarHit>)> {
        let mute = if self.muted { "[MUTED]" } else { "[Mute]" };
        let mut bar: Vec<(String, Option<BarHit>)> = vec![
            (" [Back] ".into(), Some(BarHit::Back)),
            (
                format!(
                    "F12 | {} (ShellSDR {}) | Audio ",
                    self.v.host, self.v.version
                ),
                None,
            ),
            (format!("[{}]", self.v.audio()), Some(BarHit::Audio)),
        ];
        if let Some((note, at)) = &self.note
            && at.elapsed() < NOTE
        {
            bar.push((format!(" {note} "), None));
        }
        bar.extend([
            (" Alt+C | Here ".into(), None),
            (mute.into(), Some(BarHit::Mute)),
            (" Alt+M  Vol ".into(), None),
            ("[-]".into(), Some(BarHit::VolDown)),
            (format!(" {:>3.0}% ", self.volume * 100.0), None),
            ("[+]".into(), Some(BarHit::VolUp)),
            (" Ctrl+Up/Down ".into(), None),
        ]);
        bar
    }

    /// What the bar holds at column `col`.
    fn bar_hit(&self, col: u16) -> Option<BarHit> {
        let mut x = 0u16;
        for (text, hit) in self.bar() {
            let w = text.chars().count() as u16;
            if col >= x && col < x + w {
                return hit;
            }
            x += w;
        }
        None
    }

    /// Draw the bar and the shared screen under it.
    pub fn draw(&self, f: &mut Frame) {
        let area = f.area();
        let buf = f.buffer_mut();
        self.v.grid().paint_from(buf, BAR_ROWS);
        let base = Style::default().add_modifier(Modifier::REVERSED);
        buf.set_stringn(
            area.x,
            area.y,
            " ".repeat(area.width as usize),
            area.width as usize,
            base,
        );
        let mut x = area.x;
        for (text, hit) in self.bar() {
            let left = (area.x + area.width).saturating_sub(x) as usize;
            if left == 0 {
                break;
            }
            let style = if hit.is_some() {
                base.add_modifier(Modifier::BOLD)
            } else {
                base
            };
            let (nx, _) = buf.set_stringn(x, area.y, &text, left, style);
            x = nx;
        }
    }

    fn set_volume(&mut self, v: f32) {
        self.volume = (v * 10.0).round().clamp(0.0, 10.0) / 10.0;
        self.apply_volume();
    }

    fn apply_volume(&self) {
        self.v
            .set_volume(if self.muted { 0.0 } else { self.volume });
    }

    /// Act on the bar; false: leave the window.
    fn bar_action(&mut self, hit: BarHit) -> bool {
        match hit {
            BarHit::Back => return false,
            BarHit::Audio => self.step_audio(1),
            BarHit::Mute => {
                self.muted = !self.muted;
                self.apply_volume();
            }
            BarHit::VolDown => self.set_volume(self.volume - 0.1),
            BarHit::VolUp => self.set_volume(self.volume + 0.1),
        }
        true
    }

    /// A terminal event: returns false to leave the window (`[Back]`, F12,
    /// `Ctrl+]`).
    pub fn event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                let alt = k.modifiers.contains(KeyModifiers::ALT);
                let shift = k.modifiers.contains(KeyModifiers::SHIFT);
                match k.code {
                    KeyCode::F(12) => return false,
                    // Ctrl+] arrives as Ctrl+5 from most terminals (0x1D)
                    KeyCode::Char(']') | KeyCode::Char('5') if ctrl => return false,
                    KeyCode::Char('m') | KeyCode::Char('M') if alt && !ctrl => {
                        return self.bar_action(BarHit::Mute);
                    }
                    KeyCode::Char('c') | KeyCode::Char('C') if alt && !ctrl => {
                        return self.bar_action(BarHit::Audio);
                    }
                    KeyCode::Up if ctrl && !shift => return self.bar_action(BarHit::VolUp),
                    KeyCode::Down if ctrl && !shift => return self.bar_action(BarHit::VolDown),
                    _ => self.v.input(Event::Key(k)),
                }
            }
            Event::Mouse(m) if m.row < BAR_ROWS => match (m.kind, self.bar_hit(m.column)) {
                (MouseEventKind::Down(MouseButton::Left), Some(hit)) => {
                    return self.bar_action(hit);
                }
                (MouseEventKind::Down(MouseButton::Right), Some(BarHit::Audio)) => {
                    self.step_audio(-1);
                }
                _ => {}
            },
            Event::Mouse(m) => self.v.input(Event::Mouse(MouseEvent {
                row: m.row - BAR_ROWS,
                ..m
            })),
            Event::Resize(c, r) => self.v.resize(c, r.saturating_sub(BAR_ROWS).max(1)),
            _ => {}
        }
        true
    }
}
