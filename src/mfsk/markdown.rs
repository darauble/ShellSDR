//! The received text as Markdown (spec 16.6, 16.7): text written as it
//! came except what Markdown (and Obsidian) would read as markup, marks as
//! blue highlighted italic paragraphs, pictures as PNG files in `pictures/`
//! linked in the paragraph after their header.

use std::path::{Path, PathBuf};

use super::frontend::Sideband;
use crate::record::naming::UtcDateTime;

/// The folder of the pictures, inside the recordings folder.
pub const PICTURES: &str = "pictures";

/// fldigi's frequency mark: `<<2026-11-18T18:51Z MFSK-32 @ 15770000+1496>>`.
pub fn qrg(time: UtcDateTime, mode: &str, vfo_hz: u64, sideband: Sideband, carrier: f64) -> String {
    format!(
        "<<{:04}-{:02}-{:02}T{:02}:{:02}Z {mode} @ {vfo_hz}{}{:04}>>",
        time.year,
        time.month,
        time.day,
        time.hour,
        time.minute,
        sideband.sign(),
        carrier.round().max(0.0) as u64
    )
}

/// A mark paragraph: `==🔵*Before RSID: <<...>>*==`.
pub fn mark(text: &str) -> String {
    format!("==🔵*{text}*==")
}

/// Markdown escaping of decoded text, aware of line starts.
#[derive(Debug, Clone)]
pub struct Writer {
    /// Line ends at the end of what was written (2 = a blank line).
    newlines: usize,
    /// A block was the last thing written: line ends before the next text
    /// are dropped (the block already ends its paragraph).
    after_block: bool,
}

impl Default for Writer {
    fn default() -> Self {
        // the start of a file counts as a paragraph break
        Self {
            newlines: 2,
            after_block: false,
        }
    }
}

impl Writer {
    fn at_line_start(&self) -> bool {
        self.newlines > 0
    }

    /// Decoded text as Markdown.
    pub fn text(&mut self, s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 8);
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\n' {
                if !self.after_block {
                    out.push('\n');
                    self.newlines += 1;
                }
                continue;
            }
            self.after_block = false;
            let line_start = self.at_line_start();
            self.newlines = 0;
            let next = chars.peek().copied();
            let escape = match c {
                '\\' | '*' | '`' | '[' | ']' | '<' | '$' | '~' | '#' | '|' => true,
                '=' | '%' => next == Some(c),
                '>' | '+' | '-' => line_start,
                _ => false,
            };
            if escape {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    /// A paragraph of its own (a mark, a picture).
    pub fn block(&mut self, md: &str) -> String {
        let mut out = "\n".repeat(2usize.saturating_sub(self.newlines));
        out.push_str(md);
        out.push_str("\n\n");
        self.newlines = 2;
        self.after_block = true;
        out
    }
}

/// A free path for `stem` + `ext` in `dir` (`_2`, `_3`... if taken).
pub fn unique(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|i| dir.join(format!("{stem}_{i}.{ext}")))
        .find(|p| !p.exists())
        .expect("some suffix is free")
}

/// The picture's link from a Markdown file in its parent folder.
pub fn picture_link(width: usize, height: usize, color: bool, file: &Path) -> String {
    let name = file
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    format!(
        "![Pic:{width}x{height}{}]({PICTURES}/{})",
        if color { "C" } else { "" },
        name.replace(' ', "%20")
    )
}

/// Write an RGB picture as PNG.
pub fn write_png(path: &Path, width: usize, height: usize, rgb: &[u8]) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), width as u32, height as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Best);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&rgb[..width * height * 3])
        .map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> UtcDateTime {
        UtcDateTime::from_unix(1_795_027_880) // 2026-11-18 18:51:20 UTC
    }

    #[test]
    fn marks_look_like_fldigis() {
        let q = qrg(t(), "MFSK-32", 15_770_000, Sideband::Upper, 1496.4);
        assert_eq!(q, "<<2026-11-18T18:51Z MFSK-32 @ 15770000+1496>>");
        assert_eq!(
            mark(&format!("Before RSID: {q}")),
            "==🔵*Before RSID: <<2026-11-18T18:51Z MFSK-32 @ 15770000+1496>>*=="
        );
        assert!(
            qrg(t(), "MFSK-64", 7_040_000, Sideband::Lower, 800.0).ends_with("@ 7040000-0800>>")
        );
    }

    #[test]
    fn markup_in_the_text_is_escaped_and_the_rest_kept() {
        let mut w = Writer::default();
        let md = w.text("# not a heading\n- not a list\n> no quote\nPrice $5 *now* <b> [x] a==b 2+2-1\nhttps://a.b/c_d?e=1\n");
        assert_eq!(
            md,
            "\\# not a heading\n\\- not a list\n\\> no quote\nPrice \\$5 \\*now\\* \\<b> \\[x\\] a\\==b 2+2-1\nhttps://a.b/c_d?e=1\n"
        );
    }

    #[test]
    fn blocks_are_paragraphs_of_their_own() {
        let mut w = Writer::default();
        let mut out = w.block(&mark("Before RSID"));
        out += &w.block(&mark("New RSID"));
        out += &w.text("\n\nSending Pic:2x1C;");
        out += &w.block("![Pic:2x1C](pictures/p.png)");
        out += &w.text("next\n");
        assert_eq!(
            out,
            "==🔵*Before RSID*==\n\n==🔵*New RSID*==\n\nSending Pic:2x1C;\n\n![Pic:2x1C](pictures/p.png)\n\nnext\n"
        );
    }

    #[test]
    fn a_png_is_written_and_names_do_not_collide() {
        let d = std::env::temp_dir().join(format!("shellsdr-md-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = unique(&d, "pic", "png");
        write_png(&p, 2, 1, &[255, 0, 0, 0, 255, 0]).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        assert_eq!(unique(&d, "pic", "png").file_name().unwrap(), "pic_2.png");
        assert_eq!(
            picture_link(2, 1, true, &p),
            "![Pic:2x1C](pictures/pic.png)"
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
