//! The shared screen as cells (0.4.1, spec 22.4): a neutral grid (no
//! ratatui types on the wire, so any viewer can paint it), the difference
//! of two frames, and its binary encoding.

use std::io::{self, Read, Write};

/// A colour as the wire carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WColor {
    /// The terminal's default.
    #[default]
    Reset,
    /// One of the 16 named colours (0 black .. 15 white, ANSI order).
    Named(u8),
    /// The 256-colour palette.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// One cell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WCell {
    pub symbol: String,
    pub fg: WColor,
    pub bg: WColor,
    /// ratatui `Modifier` bits (bold 1, dim 2, italic 4, underlined 8,
    /// slow blink 16, rapid blink 32, reversed 64, hidden 128, crossed 256).
    pub mods: u16,
}

/// A screen of cells, row by row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<WCell>,
}

/// What changed between two frames: runs of cells, or the whole grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellDiff {
    pub cols: u16,
    pub rows: u16,
    /// The runs cover the whole grid (a new size or the first frame).
    pub full: bool,
    /// (row, first column, the cells from there on).
    pub runs: Vec<(u16, u16, Vec<WCell>)>,
}

impl Grid {
    /// An empty grid of a size (blank cells).
    pub fn blank(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: vec![
                WCell {
                    symbol: " ".into(),
                    ..WCell::default()
                };
                cols as usize * rows as usize
            ],
        }
    }

    /// The cell at (col, row).
    pub fn get(&self, col: u16, row: u16) -> Option<&WCell> {
        (col < self.cols && row < self.rows)
            .then(|| &self.cells[row as usize * self.cols as usize + col as usize])
    }

    /// What turns `prev` (None: nothing shown yet) into this grid.
    pub fn diff(&self, prev: Option<&Grid>) -> CellDiff {
        let full = prev.is_none_or(|p| (p.cols, p.rows) != (self.cols, self.rows));
        let mut runs = Vec::new();
        let w = self.cols as usize;
        for row in 0..self.rows as usize {
            let line = &self.cells[row * w..(row + 1) * w];
            if full {
                runs.push((row as u16, 0, line.to_vec()));
                continue;
            }
            let old = &prev.unwrap().cells[row * w..(row + 1) * w];
            let mut col = 0;
            while col < w {
                if line[col] == old[col] {
                    col += 1;
                    continue;
                }
                let start = col;
                // a run ends after 4 unchanged cells (a new run costs 6
                // bytes, an unchanged blank about as much)
                let mut end = col + 1;
                let mut same = 0;
                while end < w && same < 4 {
                    if line[end] == old[end] {
                        same += 1;
                    } else {
                        same = 0;
                    }
                    end += 1;
                }
                let stop = end - same;
                runs.push((row as u16, start as u16, line[start..stop].to_vec()));
                col = end;
            }
        }
        CellDiff {
            cols: self.cols,
            rows: self.rows,
            full,
            runs,
        }
    }

    /// Apply a diff (a full one replaces the grid and its size).
    pub fn apply(&mut self, d: &CellDiff) {
        if d.full || (d.cols, d.rows) != (self.cols, self.rows) {
            *self = Grid::blank(d.cols, d.rows);
        }
        let w = self.cols as usize;
        for (row, col, cells) in &d.runs {
            let at = *row as usize * w + *col as usize;
            for (i, c) in cells.iter().enumerate() {
                if let Some(slot) = self.cells.get_mut(at + i) {
                    *slot = c.clone();
                }
            }
        }
    }
}

impl CellDiff {
    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        !self.full && self.runs.is_empty()
    }

    /// The wire form: `[u16 cols][u16 rows][u8 full][u32 runs]` then each
    /// run `[u16 row][u16 col][u16 n]` and its cells, deflated.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&self.cols.to_le_bytes());
        b.extend_from_slice(&self.rows.to_le_bytes());
        b.push(u8::from(self.full));
        b.extend_from_slice(&(self.runs.len() as u32).to_le_bytes());
        for (row, col, cells) in &self.runs {
            b.extend_from_slice(&row.to_le_bytes());
            b.extend_from_slice(&col.to_le_bytes());
            b.extend_from_slice(&(cells.len() as u16).to_le_bytes());
            for c in cells {
                let sym = c.symbol.as_bytes();
                let n = sym.len().min(255);
                b.push(n as u8);
                b.extend_from_slice(&sym[..n]);
                put_color(&mut b, c.fg);
                put_color(&mut b, c.bg);
                b.extend_from_slice(&c.mods.to_le_bytes());
            }
        }
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        let _ = z.write_all(&b);
        z.finish().unwrap_or_default()
    }

    pub fn decode(p: &[u8]) -> io::Result<Self> {
        let mut raw = Vec::new();
        flate2::read::DeflateDecoder::new(p)
            .take(64 << 20)
            .read_to_end(&mut raw)?;
        let mut c = Rd(&raw);
        let cols = c.u16()?;
        let rows = c.u16()?;
        let full = c.u8()? != 0;
        let n = c.u32()?;
        let mut runs = Vec::new();
        for _ in 0..n {
            let row = c.u16()?;
            let col = c.u16()?;
            let k = c.u16()?;
            let mut cells = Vec::with_capacity(k as usize);
            for _ in 0..k {
                let len = c.u8()? as usize;
                let symbol = String::from_utf8_lossy(c.take(len)?).into_owned();
                let fg = get_color(&mut c)?;
                let bg = get_color(&mut c)?;
                let mods = c.u16()?;
                cells.push(WCell {
                    symbol,
                    fg,
                    bg,
                    mods,
                });
            }
            runs.push((row, col, cells));
        }
        Ok(Self {
            cols,
            rows,
            full,
            runs,
        })
    }
}

fn put_color(b: &mut Vec<u8>, c: WColor) {
    match c {
        WColor::Reset => b.push(0),
        WColor::Named(i) => b.extend_from_slice(&[1, i]),
        WColor::Indexed(i) => b.extend_from_slice(&[2, i]),
        WColor::Rgb(r, g, bl) => b.extend_from_slice(&[3, r, g, bl]),
    }
}

fn get_color(c: &mut Rd) -> io::Result<WColor> {
    Ok(match c.u8()? {
        0 => WColor::Reset,
        1 => WColor::Named(c.u8()?),
        2 => WColor::Indexed(c.u8()?),
        3 => WColor::Rgb(c.u8()?, c.u8()?, c.u8()?),
        t => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("colour {t}"),
            ));
        }
    })
}

struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short cells"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

// ── ratatui ─────────────────────────────────────────────────────────────

const NAMED: [ratatui::style::Color; 16] = {
    use ratatui::style::Color::*;
    [
        Black,
        Red,
        Green,
        Yellow,
        Blue,
        Magenta,
        Cyan,
        Gray,
        DarkGray,
        LightRed,
        LightGreen,
        LightYellow,
        LightBlue,
        LightMagenta,
        LightCyan,
        White,
    ]
};

impl From<ratatui::style::Color> for WColor {
    fn from(c: ratatui::style::Color) -> Self {
        use ratatui::style::Color;
        match c {
            Color::Reset => WColor::Reset,
            Color::Indexed(i) => WColor::Indexed(i),
            Color::Rgb(r, g, b) => WColor::Rgb(r, g, b),
            named => WColor::Named(NAMED.iter().position(|n| *n == named).unwrap_or(7) as u8),
        }
    }
}

impl From<WColor> for ratatui::style::Color {
    fn from(c: WColor) -> Self {
        use ratatui::style::Color;
        match c {
            WColor::Reset => Color::Reset,
            WColor::Named(i) => NAMED[i as usize % 16],
            WColor::Indexed(i) => Color::Indexed(i),
            WColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }
}

impl Grid {
    /// The cells of a ratatui buffer.
    pub fn from_buffer(b: &ratatui::buffer::Buffer) -> Self {
        Self {
            cols: b.area.width,
            rows: b.area.height,
            cells: b
                .content
                .iter()
                .map(|c| WCell {
                    symbol: c.symbol().to_string(),
                    fg: c.fg.into(),
                    bg: c.bg.into(),
                    mods: c.modifier.bits(),
                })
                .collect(),
        }
    }

    /// Paint into a ratatui buffer (cells outside it are left out).
    pub fn paint(&self, buf: &mut ratatui::buffer::Buffer) {
        self.paint_from(buf, 0);
    }

    /// [`Self::paint`] starting `top` rows down the buffer.
    pub fn paint_from(&self, buf: &mut ratatui::buffer::Buffer, top: u16) {
        let mut a = buf.area;
        a.y += top;
        a.height = a.height.saturating_sub(top);
        for row in 0..self.rows.min(a.height) {
            for col in 0..self.cols.min(a.width) {
                let Some(c) = self.get(col, row) else {
                    continue;
                };
                if let Some(cell) = buf.cell_mut((a.x + col, a.y + row)) {
                    cell.set_symbol(&c.symbol);
                    cell.fg = c.fg.into();
                    cell.bg = c.bg.into();
                    cell.modifier = ratatui::style::Modifier::from_bits_truncate(c.mods);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};

    fn buf(text: &[&str]) -> ratatui::buffer::Buffer {
        let w = text[0].chars().count() as u16;
        let mut b =
            ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, w, text.len() as u16));
        for (y, t) in text.iter().enumerate() {
            b.set_string(0, y as u16, *t, Style::default().fg(Color::Rgb(1, 2, 3)));
        }
        b
    }

    #[test]
    fn a_diff_turns_the_old_frame_into_the_new() {
        let a = Grid::from_buffer(&buf(&["hello world", "second row!", "third......"]));
        let mut nb = buf(&["hello there", "second row!", "Third......"]);
        nb[(0, 2)].set_style(
            Style::default()
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        );
        let b = Grid::from_buffer(&nb);
        let d = b.diff(Some(&a));
        assert!(!d.full);
        assert_eq!(d.runs.len(), 2, "{:?}", d.runs);
        let back = CellDiff::decode(&d.encode()).unwrap();
        assert_eq!(back, d);
        let mut v = a.clone();
        v.apply(&back);
        assert_eq!(v, b);
        assert!(b.diff(Some(&b)).is_empty());
        // and back to ratatui
        let mut out = ratatui::buffer::Buffer::empty(nb.area);
        v.paint(&mut out);
        assert_eq!(out, nb);
    }

    #[test]
    fn a_new_size_sends_everything() {
        let a = Grid::from_buffer(&buf(&["ab", "cd"]));
        let b = Grid::from_buffer(&buf(&["abc", "def", "ghi"]));
        let d = b.diff(Some(&a));
        assert!(d.full);
        let mut v = a;
        v.apply(&d);
        assert_eq!(v, b);
        assert!(Grid::default().diff(None).full);
    }

    #[test]
    fn colours_survive_both_ways() {
        for c in [
            Color::Reset,
            Color::LightCyan,
            Color::Black,
            Color::Indexed(200),
            Color::Rgb(9, 8, 7),
        ] {
            assert_eq!(Color::from(WColor::from(c)), c);
        }
    }
}
