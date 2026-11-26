//! The window icon (0.4.4): `assets/icon/shell-sdr-icon.svg`, compiled
//! into the binary and drawn into pixels when the window opens. (The
//! packages also install it with a menu entry, for any build.)

use eframe::egui;

/// The icon's SVG source.
const SVG: &[u8] = include_bytes!("../../assets/icon/shell-sdr-icon.svg");

/// The icon's size in pixels.
const SIZE: u32 = 256;

/// The icon as RGBA pixels (None if it could not be drawn: the window
/// then opens without it).
pub fn icon() -> Option<egui::IconData> {
    let tree = resvg::usvg::Tree::from_data(SVG, &resvg::usvg::Options::default()).ok()?;
    let mut pix = resvg::tiny_skia::Pixmap::new(SIZE, SIZE)?;
    let s = tree.size();
    let scale = SIZE as f32 / s.width().max(s.height());
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pix.as_mut(),
    );
    // tiny-skia keeps premultiplied alpha; the window wants it straight
    let rgba = pix
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Some(egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_icon_is_drawn() {
        let i = super::icon().expect("the icon");
        assert_eq!(i.rgba.len(), (i.width * i.height * 4) as usize);
        // the dark green background in the middle of the left edge
        let at = |x: u32, y: u32| {
            let o = ((y * i.width + x) * 4) as usize;
            [i.rgba[o], i.rgba[o + 1], i.rgba[o + 2], i.rgba[o + 3]]
        };
        assert_eq!(at(4, 128), [0x0B, 0x2A, 0x14, 255]);
        assert_eq!(at(0, 0)[3], 0, "rounded corners are clear");
    }
}
