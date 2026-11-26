//! Where the run loop's screen goes and its input comes from (0.4.1): the
//! terminal, or the egui window (`--gui`), which is the same ShellSDR in a
//! window: the loop draws into an in-memory terminal of the window's size,
//! the window paints it (the spectrum and the waterfall at pixel
//! resolution) and sends its keys and clicks back.

use std::io;
use std::time::Duration;

use crossterm::event::Event;
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::AppState;
use std::sync::Arc;

use crate::remote::picture::Picture;
use crate::remote::wire::Overlay;

/// The run loop's front end.
pub trait Front<B: Backend> {
    /// Before a frame: bring the terminal to the front's size and tell the
    /// state what it needs (the FAX view's cell size).
    fn before_draw(&mut self, _t: &mut Terminal<B>, _state: &mut AppState) {}

    /// After a frame: the overlays drawn on it, spectrum rows from an open
    /// window and the pictures' pixels (a graphical front paints them).
    fn after_draw(
        &mut self,
        _t: &Terminal<B>,
        _overlays: Vec<Overlay>,
        _rows: Vec<(u8, Vec<f32>)>,
        _pictures: Vec<Arc<Picture>>,
    ) {
    }

    /// A new spectrum row of an overlay stream (`wire::STREAM_*`; a
    /// graphical front's waterfalls).
    fn on_rows(&mut self, _stream: u8, _row: &[f32]) {}

    /// Whether input is waiting, waiting at most `d`.
    fn poll(&mut self, d: Duration) -> io::Result<bool>;

    /// The next input.
    fn read(&mut self) -> io::Result<Event>;

    /// Whether the front draws overlays (the loop then sends them, and a
    /// window into another ShellSDR asks for them).
    fn graphical(&self) -> bool {
        false
    }

    /// Whether Kitty pictures can be placed.
    fn kitty(&self) -> bool {
        false
    }

    /// Ring the bell (a DSC alert).
    fn bell(&mut self) {}

    /// The front wants the program to end (its window was closed).
    fn quit_requested(&mut self) -> bool {
        false
    }

    /// The front is gone (the program must end now).
    fn gone(&self) -> bool {
        false
    }
}

/// The terminal (crossterm).
pub struct TermFront;

impl<B: Backend> Front<B> for TermFront {
    fn before_draw(&mut self, _t: &mut Terminal<B>, state: &mut AppState) {
        // The FAX view's pictures are sized by the terminal's cell pixels.
        if state.fax_view_shown()
            && let Ok(ws) = crossterm::terminal::window_size()
            && ws.columns > 0
            && ws.rows > 0
            && ws.width > 0
        {
            state.fax_ui.cell = (ws.width / ws.columns, ws.height / ws.rows);
        }
    }

    fn poll(&mut self, d: Duration) -> io::Result<bool> {
        crossterm::event::poll(d)
    }

    fn read(&mut self) -> io::Result<Event> {
        crossterm::event::read()
    }

    fn kitty(&self) -> bool {
        crate::fax::kitty::supported()
    }

    fn bell(&mut self) {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x07");
        let _ = out.flush();
    }
}
