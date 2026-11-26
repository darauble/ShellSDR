//! Waterfall (scrolling spectrogram) widget and its rolling history (pure
//! history + mapping, thin render shell).
//!
//! Step 5. The waterfall shows recent FFT frames stacked over time, newest at
//! the top, each frame a horizontal line colored by power. It reuses the same
//! inputs as the spectrum: the DC-centered dB frames from
//! `dsp::fft::SpectrumAnalyzer`, the shared `DbRange` for the dB->color
//! mapping, and the theme's full `waterfall` gradient for the palette (so the
//! spectrum trace and the waterfall share one color scheme). Columns are
//! peak-decimated with the same helper as the spectrum so the two line up.
//!
//! Terminal rows pack two time-steps each via the upper-half-block glyph
//! ("▀"): the foreground color is the newer row, the background the older,
//! doubling vertical resolution. `WaterfallHistory` is a bounded ring of raw
//! frames (decimated at render time so resize is handled); it and the color
//! mapping are unit-tested, the drawing is a thin shell.

use std::collections::VecDeque;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

use crate::ui::spectrum::{DbRange, peak_decimate};
use crate::ui::theme::Theme;

/// Glyph whose top half is foreground and bottom half background, letting one
/// character cell show two stacked time-steps.
const UPPER_HALF_BLOCK: &str = "▀";

/// A bounded, newest-first history of raw dB frames for the waterfall.
#[derive(Debug, Clone, PartialEq)]
pub struct WaterfallHistory {
    frames: VecDeque<Vec<f32>>,
    capacity: usize,
}

impl WaterfallHistory {
    /// A history holding up to `capacity` frames.
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity.min(1024)),
            capacity: capacity.max(1),
        }
    }

    /// Push the newest frame to the front, dropping the oldest past capacity.
    pub fn push(&mut self, frame: Vec<f32>) {
        self.frames.push_front(frame);
        while self.frames.len() > self.capacity {
            self.frames.pop_back();
        }
    }

    /// Number of frames currently held.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Clear the history (e.g. when the engine stops).
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// Frames newest-first.
    pub fn iter(&self) -> impl Iterator<Item = &Vec<f32>> {
        self.frames.iter()
    }
}

/// Map a dB value to a gradient color through the display range: normalize to
/// `[0,1]` across `[range.min, range.max]` (clamped) and sample the theme's
/// `waterfall` gradient. Non-finite values map to the lowest stop.
pub fn db_to_color(db: f32, range: &DbRange, theme: &Theme) -> Color {
    let span = (range.max - range.min) as f32;
    let t = if !db.is_finite() || span <= 0.0 {
        0.0
    } else {
        ((db - range.min as f32) / span).clamp(0.0, 1.0)
    };
    let (r, g, b) = theme.waterfall_color(t);
    Color::Rgb(r, g, b)
}

/// The waterfall widget: draws `history` into `area`, newest at the top, two
/// frames per character row via the half-block glyph. Columns are peak-
/// decimated to the area width so they align with the spectrum above.
pub struct Waterfall<'a> {
    pub history: &'a WaterfallHistory,
    pub range: &'a DbRange,
    pub theme: &'a Theme,
}

impl Widget for Waterfall<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = area.width as usize;
        let height = area.height as usize;
        if width == 0 || height == 0 {
            return;
        }
        // Decimate just the rows we can show (2 per character row).
        let needed = (height * 2).min(self.history.len());
        let rows: Vec<Vec<(f64, f64)>> = self
            .history
            .iter()
            .take(needed)
            .map(|f| peak_decimate(f, width))
            .collect();

        let bg = Color::Black;
        for row in 0..height {
            let upper = rows.get(row * 2); // newer
            let lower = rows.get(row * 2 + 1); // older
            for col in 0..width {
                let x = area.x + col as u16;
                let y = area.y + row as u16;
                let fg = upper
                    .and_then(|r| r.get(col))
                    .map(|&(_, db)| db_to_color(db as f32, self.range, self.theme))
                    .unwrap_or(bg);
                let bgc = lower
                    .and_then(|r| r.get(col))
                    .map(|&(_, db)| db_to_color(db as f32, self.range, self.theme))
                    .unwrap_or(bg);
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(UPPER_HALF_BLOCK).set_fg(fg).set_bg(bgc);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_newest_first_and_bounded() {
        let mut h = WaterfallHistory::new(3);
        h.push(vec![1.0]);
        h.push(vec![2.0]);
        h.push(vec![3.0]);
        h.push(vec![4.0]); // evicts the oldest (1.0)
        let got: Vec<f32> = h.iter().map(|f| f[0]).collect();
        assert_eq!(got, vec![4.0, 3.0, 2.0], "newest first, capacity enforced");
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn history_clear_empties() {
        let mut h = WaterfallHistory::new(8);
        h.push(vec![0.0]);
        assert!(!h.is_empty());
        h.clear();
        assert!(h.is_empty());
    }

    #[test]
    fn db_to_color_maps_range_to_gradient_ends() {
        let theme = Theme::builtin_retro();
        let range = DbRange {
            min: -100.0,
            max: -20.0,
            auto: false,
        };
        // Lowest stop of the retro gradient is black (0,0,0), top is yellow.
        let lo = db_to_color(-100.0, &range, &theme);
        let hi = db_to_color(-20.0, &range, &theme);
        assert_eq!(lo, Color::Rgb(0, 0, 0), "floor maps to the bottom stop");
        assert_eq!(
            hi,
            Color::Rgb(0xFF, 0xFF, 0x00),
            "ceiling maps to the top stop"
        );
    }

    #[test]
    fn db_to_color_clamps_out_of_range_and_handles_nan() {
        let theme = Theme::builtin_retro();
        let range = DbRange {
            min: -100.0,
            max: -20.0,
            auto: false,
        };
        assert_eq!(db_to_color(-200.0, &range, &theme), Color::Rgb(0, 0, 0));
        assert_eq!(
            db_to_color(f32::NEG_INFINITY, &range, &theme),
            Color::Rgb(0, 0, 0),
            "non-finite maps to the floor color"
        );
    }

    #[test]
    fn widget_renders_newest_row_at_top() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let theme = Theme::builtin_retro();
        let range = DbRange {
            min: -100.0,
            max: 0.0,
            auto: false,
        };
        let mut h = WaterfallHistory::new(16);
        // Oldest = floor (black), newest = ceiling (hot). Newest must be on top.
        h.push(vec![-100.0; 8]); // older
        h.push(vec![0.0; 8]); // newest
        let backend = TestBackend::new(8, 4);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let w = Waterfall {
                history: &h,
                range: &range,
                theme: &theme,
            };
            f.render_widget(w, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        // Top-left cell foreground = newest frame (hot/top stop), not black.
        let fg = buf[(0, 0)].style().fg;
        assert_eq!(
            fg,
            Some(Color::Rgb(0xFF, 0xFF, 0x00)),
            "newest frame on top"
        );
    }
}
