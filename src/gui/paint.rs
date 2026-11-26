//! Painting the shared screen in egui (0.4.1, spec 22.6): the cell grid in
//! a monospace font, and over the overlays' rectangles the spectrum trace
//! and the waterfall at pixel resolution.

use eframe::egui::{self, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};

use crate::remote::cells::{Grid, WColor};
use crate::remote::picture::Picture;
use crate::remote::wire::Overlay;

/// The terminal's default colours.
pub const DEFAULT_FG: Color32 = Color32::from_rgb(0xd0, 0xd0, 0xd0);
pub const DEFAULT_BG: Color32 = Color32::from_rgb(0x10, 0x10, 0x10);

/// xterm's 16 colours.
const ANSI: [[u8; 3]; 16] = [
    [0, 0, 0],
    [205, 0, 0],
    [0, 205, 0],
    [205, 205, 0],
    [0, 0, 238],
    [205, 0, 205],
    [0, 205, 205],
    [229, 229, 229],
    [127, 127, 127],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [92, 92, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
];

/// A 256-colour palette entry.
fn indexed(i: u8) -> Color32 {
    match i {
        0..=15 => {
            let [r, g, b] = ANSI[i as usize];
            Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            let i = i - 16;
            let v = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
            Color32::from_rgb(v(i / 36), v((i / 6) % 6), v(i % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            Color32::from_rgb(g, g, g)
        }
    }
}

/// A cell colour (`fg`: the default is the text colour).
pub fn color(c: WColor, fg: bool) -> Color32 {
    match c {
        WColor::Reset if fg => DEFAULT_FG,
        WColor::Reset => DEFAULT_BG,
        WColor::Named(i) => indexed(i.min(15)),
        WColor::Indexed(i) => indexed(i),
        WColor::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

const REVERSED: u16 = 64;
const DIM: u16 = 2;

/// A cell's colours after its modifiers.
fn cell_colors(fg: WColor, bg: WColor, mods: u16) -> (Color32, Color32) {
    let (mut f, mut b) = (color(fg, true), color(bg, false));
    if mods & REVERSED != 0 {
        std::mem::swap(&mut f, &mut b);
    }
    if mods & DIM != 0 {
        f = f.gamma_multiply(0.6);
    }
    (f, b)
}

/// Whether the monospace font draws `s` one cell per character (ASCII
/// and the box and block drawing it has): such text goes in runs, other
/// characters one by one at their cells.
fn in_runs(s: &str) -> bool {
    s.chars().all(|c| {
        c.is_ascii()
            || ('\u{2500}'..='\u{259F}').contains(&c)
            || ('\u{25A0}'..='\u{25FF}').contains(&c)
    })
}

/// Paint `grid` with its top left at `origin`, cells of `cell` points.
pub fn grid(p: &Painter, origin: Pos2, cell: Vec2, font: &FontId, g: &Grid) {
    let at = |col: usize, row: usize| origin + Vec2::new(col as f32 * cell.x, row as f32 * cell.y);
    p.rect_filled(
        Rect::from_min_size(
            origin,
            Vec2::new(g.cols as f32 * cell.x, g.rows as f32 * cell.y),
        ),
        0.0,
        DEFAULT_BG,
    );
    let w = g.cols as usize;
    for row in 0..g.rows as usize {
        let line = &g.cells[row * w..(row + 1) * w];
        // backgrounds, merged
        let mut col = 0;
        while col < w {
            let (_, b) = cell_colors(line[col].fg, line[col].bg, line[col].mods);
            let start = col;
            while col < w && cell_colors(line[col].fg, line[col].bg, line[col].mods).1 == b {
                col += 1;
            }
            if b != DEFAULT_BG {
                p.rect_filled(Rect::from_min_max(at(start, row), at(col, row + 1)), 0.0, b);
            }
        }
        // text: runs of one colour, the rest cell by cell
        let mut col = 0;
        while col < w {
            let c = &line[col];
            let (f, _) = cell_colors(c.fg, c.bg, c.mods);
            if c.symbol.trim().is_empty() {
                col += 1;
                continue;
            }
            if let Some(dots) = braille(&c.symbol) {
                braille_dots(p, Rect::from_min_size(at(col, row), cell), dots, f);
                col += 1;
                continue;
            }
            if !in_runs(&c.symbol) {
                p.text(
                    at(col, row),
                    egui::Align2::LEFT_TOP,
                    &c.symbol,
                    font.clone(),
                    f,
                );
                col += 1;
                continue;
            }
            let start = col;
            let mut run = String::new();
            while col < w {
                let d = &line[col];
                if cell_colors(d.fg, d.bg, d.mods).0 != f
                    || !in_runs(&d.symbol)
                    || d.symbol.is_empty()
                {
                    break;
                }
                run.push_str(&d.symbol);
                col += 1;
            }
            p.text(
                at(start, row),
                egui::Align2::LEFT_TOP,
                run.trim_end(),
                font.clone(),
                f,
            );
        }
    }
}

/// The dot bits of a braille pattern character (U+2800..U+28FF).
fn braille(s: &str) -> Option<u8> {
    let mut it = s.chars();
    let c = it.next()?;
    (it.next().is_none() && ('\u{2800}'..='\u{28FF}').contains(&c))
        .then(|| (c as u32 - 0x2800) as u8)
}

/// Draw braille dots in a cell: two columns of four (bits 0,1,2,6 left,
/// 3,4,5,7 right, top to bottom), as the terminal's font would.
fn braille_dots(p: &Painter, cell: Rect, bits: u8, color: Color32) {
    const POS: [(u8, f32, f32); 8] = [
        (0, 0.0, 0.0),
        (1, 0.0, 1.0),
        (2, 0.0, 2.0),
        (3, 1.0, 0.0),
        (4, 1.0, 1.0),
        (5, 1.0, 2.0),
        (6, 0.0, 3.0),
        (7, 1.0, 3.0),
    ];
    let (w, h) = (cell.width() / 2.0, cell.height() / 4.0);
    let r = (w.min(h) * 0.35).max(0.8);
    for (bit, x, y) in POS {
        if bits & (1 << bit) != 0 {
            let c = cell.min + Vec2::new((x + 0.5) * w, (y + 0.5) * h);
            p.circle_filled(c, r, color);
        }
    }
}

/// One overlay stream's pixel state: its latest row, its rows newest
/// first, and its waterfall texture.
#[derive(Default)]
struct Stream {
    last: Option<Vec<f32>>,
    history: std::collections::VecDeque<Vec<f32>>,
    texture: Option<egui::TextureHandle>,
    /// What the texture shows: (width, height, rows taken).
    shown: (usize, usize, u64),
    rows_in: u64,
    /// The span the rows were made for (a new zoom starts over).
    span_hz: f64,
}

/// The overlays' pixel state, per stream (`wire::STREAM_*`).
#[derive(Default)]
pub struct Plots {
    streams: std::collections::HashMap<u8, Stream>,
    /// Each picture slot's texture and the key of the pixels in it.
    pictures: std::collections::HashMap<u8, (u64, egui::TextureHandle)>,
}

impl Plots {
    /// Take new spectrum rows.
    pub fn push(&mut self, rows: Vec<(u8, Vec<f32>)>) {
        for (id, r) in rows {
            let s = self.streams.entry(id).or_default();
            s.history.push_front(r.clone());
            s.last = Some(r);
            s.rows_in += 1;
            s.history.truncate(2048);
        }
    }

    /// Paint the overlays (cells to pixels with `origin` and `cell`).
    pub fn paint(
        &mut self,
        ctx: &egui::Context,
        p: &Painter,
        origin: Pos2,
        cell: Vec2,
        overlays: &[Overlay],
        pictures: &[std::sync::Arc<Picture>],
    ) {
        let to_rect = |r: &[u16; 4]| {
            Rect::from_min_size(
                origin + Vec2::new(r[0] as f32 * cell.x, r[1] as f32 * cell.y),
                Vec2::new(r[2] as f32 * cell.x, r[3] as f32 * cell.y),
            )
        };
        let rgb = |c: &[u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
        for o in overlays {
            match o {
                Overlay::Spectrum {
                    rect,
                    center_hz,
                    span_hz,
                    vfo_hz,
                    db_min,
                    db_max,
                    trace,
                    center,
                    marker,
                    stream,
                    edges_hz,
                } => {
                    let r = to_rect(rect);
                    p.rect_filled(r, 0.0, DEFAULT_BG);
                    let x_of = |off: f64| r.left() + ((off / span_hz + 0.5) as f32) * r.width();
                    let vline = |x: f32, stroke: Stroke| {
                        p.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.bottom())], stroke)
                    };
                    vline(x_of(0.0), Stroke::new(1.0, rgb(center)));
                    edges(p, r, *span_hz, *edges_hz, rgb(center));
                    if let Some(row) = self.streams.get(stream).and_then(|s| s.last.as_ref()) {
                        let cols = r.width().max(1.0) as usize;
                        let pts: Vec<Pos2> = peak(row, cols)
                            .iter()
                            .enumerate()
                            .map(|(i, db)| {
                                let t = ((*db as f64 - db_min) / (db_max - db_min)).clamp(0.0, 1.0);
                                Pos2::new(
                                    r.left() + i as f32 + 0.5,
                                    r.bottom() - t as f32 * r.height(),
                                )
                            })
                            .collect();
                        p.add(egui::Shape::line(pts, Stroke::new(1.0, rgb(trace))));
                    }
                    let off = vfo_hz - center_hz;
                    if off.abs() <= span_hz / 2.0 {
                        vline(x_of(off), Stroke::new(2.0, rgb(marker)));
                    }
                }
                Overlay::Waterfall {
                    rect,
                    span_hz,
                    db_min,
                    db_max,
                    gradient,
                    stream,
                    edges_hz,
                    ..
                } => {
                    let r = to_rect(rect);
                    let (w, h) = (r.width().max(1.0) as usize, r.height().max(1.0) as usize);
                    let s = self.streams.entry(*stream).or_default();
                    if (s.span_hz - span_hz).abs() > 1e-6 {
                        // another zoom: the old rows have another scale
                        if s.span_hz != 0.0 {
                            s.history.clear();
                            s.rows_in += 1;
                        }
                        s.span_hz = *span_hz;
                    }
                    if s.shown != (w, h, s.rows_in) || s.texture.is_none() {
                        let img = waterfall_image(&s.history, w, h, *db_min, *db_max, gradient);
                        match &mut s.texture {
                            Some(t) => t.set(img, egui::TextureOptions::NEAREST),
                            None => {
                                s.texture = Some(ctx.load_texture(
                                    format!("waterfall-{stream}"),
                                    img,
                                    egui::TextureOptions::NEAREST,
                                ))
                            }
                        }
                        s.shown = (w, h, s.rows_in);
                    }
                    if let Some(t) = &s.texture {
                        p.image(
                            t.id(),
                            r,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    edges(p, r, *span_hz, *edges_hz, Color32::from_gray(160));
                }
                Overlay::Picture { rect, slot, fit } => {
                    let r = to_rect(rect);
                    let new = pictures.iter().find(|q| q.slot == *slot);
                    if let Some(pic) = new
                        && self.pictures.get(slot).is_none_or(|(k, _)| *k != pic.key)
                        && pic.valid()
                    {
                        let img = egui::ColorImage::from_rgba_unmultiplied(
                            [pic.width as usize, pic.height as usize],
                            &pic.rgba(),
                        );
                        let opts = egui::TextureOptions::LINEAR;
                        match self.pictures.get_mut(slot) {
                            Some((k, t)) => {
                                t.set(img, opts);
                                *k = pic.key;
                            }
                            None => {
                                let t = ctx.load_texture(format!("picture-{slot}"), img, opts);
                                self.pictures.insert(*slot, (pic.key, t));
                            }
                        }
                    }
                    // until the first pixels come, the cells' version shows
                    let Some((_, t)) = self.pictures.get(slot) else {
                        continue;
                    };
                    let at = if *fit {
                        p.rect_filled(r, 0.0, DEFAULT_BG);
                        fitted(r, t.size_vec2())
                    } else {
                        r
                    };
                    p.image(
                        t.id(),
                        at,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            }
        }
    }
}

/// The largest rectangle of `size`'s aspect in `r`, at its top, centered.
pub fn fitted(r: Rect, size: Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return r;
    }
    let s = (r.width() / size.x).min(r.height() / size.y);
    let (w, h) = (size.x * s, size.y * s);
    Rect::from_min_size(
        Pos2::new(r.left() + (r.width() - w) / 2.0, r.top()),
        Vec2::new(w, h),
    )
}

/// The channel filter's edges, dashed, `bw` wide around the middle of `r`
/// (which spans `span` Hz).
fn edges(p: &Painter, r: Rect, span: f64, bw: f64, color: Color32) {
    if bw <= 0.0 || span <= 0.0 {
        return;
    }
    for off in [-bw / 2.0, bw / 2.0] {
        if off.abs() > span / 2.0 {
            continue;
        }
        let x = r.left() + ((off / span + 0.5) as f32) * r.width();
        p.add(egui::Shape::dashed_line(
            &[Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
            Stroke::new(1.0, color),
            4.0,
            4.0,
        ));
    }
}

/// `row` reduced (or stretched) to `n` values, each the peak of its group.
pub fn peak(row: &[f32], n: usize) -> Vec<f32> {
    if row.is_empty() || n == 0 {
        return Vec::new();
    }
    let len = row.len();
    (0..n)
        .map(|i| {
            let a = i * len / n;
            let b = ((i + 1) * len / n).max(a + 1).min(len);
            row[a..b].iter().copied().fold(f32::NEG_INFINITY, f32::max)
        })
        .collect()
}

/// The waterfall as pixels: one row per spectrum row, newest on top.
pub fn waterfall_image(
    history: &std::collections::VecDeque<Vec<f32>>,
    w: usize,
    h: usize,
    db_min: f64,
    db_max: f64,
    gradient: &[[u8; 3]],
) -> egui::ColorImage {
    let mut px = vec![Color32::BLACK; w * h];
    for (y, row) in history.iter().take(h).enumerate() {
        for (x, db) in peak(row, w).into_iter().enumerate() {
            let t = if db.is_finite() && db_max > db_min {
                ((db as f64 - db_min) / (db_max - db_min)).clamp(0.0, 1.0) as f32
            } else {
                0.0
            };
            px[y * w + x] = sample(gradient, t);
        }
    }
    egui::ColorImage::new([w, h], px)
}

/// The gradient at `t` in 0..1 (stops evenly spaced).
fn sample(g: &[[u8; 3]], t: f32) -> Color32 {
    match g.len() {
        0 => Color32::from_gray((t * 255.0) as u8),
        1 => Color32::from_rgb(g[0][0], g[0][1], g[0][2]),
        n => {
            let x = t * (n - 1) as f32;
            let i = (x.floor() as usize).min(n - 2);
            let f = x - i as f32;
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
            Color32::from_rgb(
                mix(g[i][0], g[i + 1][0]),
                mix(g[i][1], g[i + 1][1]),
                mix(g[i][2], g[i + 1][2]),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_the_gradient() {
        assert_eq!(
            color(WColor::Rgb(1, 2, 3), true),
            Color32::from_rgb(1, 2, 3)
        );
        assert_eq!(
            color(WColor::Indexed(196), true),
            Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(color(WColor::Indexed(232), true), Color32::from_gray(8));
        let (f, b) = cell_colors(WColor::Reset, WColor::Reset, REVERSED);
        assert_eq!((f, b), (DEFAULT_BG, DEFAULT_FG));
        let g = [[0, 0, 0], [255, 255, 0]];
        assert_eq!(sample(&g, 1.0), Color32::from_rgb(255, 255, 0));
        assert_eq!(sample(&g, 0.5), Color32::from_rgb(128, 128, 0));
    }

    #[test]
    fn the_waterfall_has_the_newest_row_on_top_at_pixel_width() {
        let mut h = std::collections::VecDeque::new();
        h.push_front(vec![-120.0f32; 4096]);
        let mut hot = vec![-120.0f32; 4096];
        hot[2048] = -20.0;
        h.push_front(hot);
        let img = waterfall_image(&h, 1000, 3, -120.0, -20.0, &[[0, 0, 0], [255, 255, 0]]);
        assert_eq!(img.size, [1000, 3]);
        assert_eq!(
            img.pixels[500],
            Color32::from_rgb(255, 255, 0),
            "the peak kept"
        );
        assert_eq!(
            img.pixels[1000 + 500],
            Color32::BLACK,
            "the older row below"
        );
        assert_eq!(img.pixels[2000], Color32::BLACK, "no third row yet");
        assert!(in_runs("│Kind ─▀") && !in_runs("⡇"));
        // a picture fits at the top, centered
        let r = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(400.0, 300.0));
        let f = fitted(r, Vec2::new(320.0, 320.0));
        assert_eq!(
            f,
            Rect::from_min_size(Pos2::new(60.0, 20.0), Vec2::new(300.0, 300.0))
        );
        assert_eq!(braille("⡇"), Some(0b0100_0111));
        assert_eq!(braille("a"), None);
    }
}
