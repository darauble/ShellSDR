//! One screen showing the application (0.4.1, spec 22.5): the terminal, or a
//! remote viewer's window of another size.
//!
//! The application state is shared by every screen (one focus, one set of
//! popups, like a remote desktop); what depends on a screen's size (the left
//! panel's scroll, what is visible, the views' row counts) is a
//! [`ScreenLocal`], swapped into the state while that screen is prepared,
//! drawn and handles its input.

use ratatui::layout::Rect;

use crate::app::AppState;

/// The part of the state that belongs to one screen.
#[derive(Debug, Default)]
pub struct ScreenLocal {
    screen_width: u16,
    left_scroll: u16,
    left_viewport: u16,
    spectrum_visible: bool,
    work_slot_visible: bool,
    vfo_view_visible: bool,
    fax_visible_lines: i32,
    fax_kitty: bool,
    fax_kitty_allowed: bool,
    fax_cell: (u16, u16),
    #[allow(clippy::type_complexity)]
    fax_blocks: Option<((u64, i32, i32, usize, usize, i32, i32, u64), Vec<u8>)>,
    digital_back_max: usize,
    digital_rows: usize,
    pixel_pictures: bool,
}

impl ScreenLocal {
    /// A screen without Kitty pictures, cells of `cell_px` pixels (a remote
    /// viewer: half-block pictures).
    pub fn remote(cell_px: (u16, u16), pictures: bool) -> Self {
        Self {
            fax_cell: cell_px,
            pixel_pictures: pictures,
            ..Self::default()
        }
    }

    /// Exchange this screen's part with what the state holds now.
    pub fn swap(&mut self, s: &mut AppState) {
        use std::mem::swap;
        swap(&mut self.screen_width, &mut s.screen_width);
        swap(&mut self.left_scroll, &mut s.left_scroll);
        swap(&mut self.left_viewport, &mut s.left_viewport);
        swap(&mut self.spectrum_visible, &mut s.spectrum_visible);
        swap(&mut self.work_slot_visible, &mut s.work_slot_visible);
        swap(&mut self.vfo_view_visible, &mut s.vfo_view_visible);
        swap(&mut self.fax_visible_lines, &mut s.fax_ui.visible_lines);
        swap(&mut self.fax_kitty, &mut s.fax_ui.kitty);
        swap(&mut self.fax_kitty_allowed, &mut s.fax_ui.kitty_allowed);
        swap(&mut self.fax_cell, &mut s.fax_ui.cell);
        swap(&mut self.pixel_pictures, &mut s.pixel_pictures);
        swap(&mut self.fax_blocks, s.fax_ui.blocks.get_mut());
        let b = s.digital_ui.back_max.get();
        s.digital_ui.back_max.set(self.digital_back_max);
        self.digital_back_max = b;
        let r = s.digital_ui.rows.get();
        s.digital_ui.rows.set(self.digital_rows);
        self.digital_rows = r;
    }

    /// What this screen shows of the spectrum and the VFO view (with the
    /// state swapped out: its own values).
    pub fn shows(&self) -> (bool, bool) {
        (self.spectrum_visible, self.vfo_view_visible)
    }
}

/// Bring the size-dependent state in line with a screen of `full` (the
/// state holds that screen's [`ScreenLocal`]).
pub fn prepare(state: &mut AppState, full: Rect) {
    state.screen_width = full.width;
    let regions = state.work_regions(full);
    state.spectrum_visible = regions.spectrum_shown();
    state.set_work_slot_visible(!regions.vfo_view.is_empty());
    state.reconcile_left_scroll(regions.left_panel.height);
    if let Some(b) = &mut state.file_browser {
        b.reconcile_scroll(crate::ui::file_browser::list_rows(full));
    }
    if let Some(h) = &mut state.help {
        h.reconcile(full);
    }
    let v = regions.vfo_view;
    let inner = Rect::new(
        v.x + 1,
        v.y + 1,
        v.width.saturating_sub(2),
        v.height.saturating_sub(2),
    );
    // The DSC view keeps its selection and detail scroll in range.
    if state.dsc_view_shown() {
        let l = crate::ui::dsc_view::layout_for(inner, state);
        let lines =
            crate::ui::dsc_view::detail_lines(state, l.detail.width.saturating_sub(2) as usize)
                .len();
        state.reconcile_dsc(l.list_rows(), l.detail_rows(), lines);
    }
    if state.hfdl_view_shown() {
        let l = crate::ui::hfdl_view::layout_for(inner, state);
        let lines =
            crate::ui::hfdl_view::detail_lines(state, l.detail.width.saturating_sub(2) as usize)
                .len();
        state.reconcile_hfdl(l.list_rows(), l.detail_rows(), lines);
    }
    // The FAX view: picture lines on screen (for scrolling), and whether a
    // real picture may be placed (nothing drawn over it).
    if state.fax_view_shown() {
        state.fax_ui.visible_lines = crate::ui::fax_view::visible_lines(inner, state);
        state.fax_ui.kitty_allowed = state.fax_ui.kitty
            && !state.popup_open()
            && state.help.is_none()
            && state.fax_ui.menu.is_none()
            && state.file_browser.is_none();
    }
}

/// Act on one terminal event against the frame whose clickable regions are
/// `hits`. Returns whether it was a mouse event that was acted on (the
/// caller then draws before the next one).
pub fn handle_event(
    state: &mut AppState,
    hits: &crate::ui::mouse::HitMap,
    ev: crossterm::event::Event,
) -> bool {
    use crossterm::event::{Event, KeyEventKind};
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            let action = if state.text_capture() {
                crate::ui::run::translate_text_key(key)
            } else {
                crate::ui::run::translate_key(key)
            };
            if let Some(action) = action {
                crate::ui::run::apply_action(state, action);
            }
            false
        }
        Event::Mouse(m) => match crate::ui::mouse::MouseInput::from_crossterm(m) {
            Some(input) => {
                crate::ui::mouse::apply_mouse(state, hits, input);
                true
            }
            None => false, // motion/drag/release: ignored
        },
        _ => false,
    }
}

/// Draw the state for a screen of `cols` x `rows` into an off-screen buffer
/// (the state holds that screen's [`ScreenLocal`] and was prepared for it).
pub fn render_offscreen(
    state: &AppState,
    cols: u16,
    rows: u16,
) -> (ratatui::buffer::Buffer, crate::ui::mouse::HitMap) {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows))
        .expect("an in-memory terminal");
    let mut hits = crate::ui::mouse::HitMap::default();
    let _ = term.draw(|f| hits = crate::ui::render::render_with_hits(f, state));
    (term.backend().buffer().clone(), hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_screens_keep_their_own_scroll_and_visibility() {
        let mut s = AppState::with_mock_source();
        let mut remote = ScreenLocal::remote((8, 16), false);
        // the local screen, tall
        prepare(&mut s, Rect::new(0, 0, 200, 80));
        let local_view = s.left_viewport;
        // the remote screen, short
        remote.swap(&mut s);
        prepare(&mut s, Rect::new(0, 0, 100, 30));
        let (buf, hits) = render_offscreen(&s, 100, 30);
        assert_eq!(buf.area, Rect::new(0, 0, 100, 30));
        assert!(hits.hit(2, 6).is_some(), "the remote frame's own hit map");
        let remote_view = s.left_viewport;
        remote.swap(&mut s);
        assert_eq!(s.left_viewport, local_view, "the local screen's back");
        assert_ne!(local_view, remote_view);
        assert_eq!(s.screen_width, 200);
        assert!(!remote.fax_kitty, "no Kitty pictures remotely");
    }

    #[test]
    fn the_plots_are_recorded_as_overlays() {
        use crate::remote::wire::Overlay;
        let mut s = AppState::with_mock_source();
        s.spectrum = vec![-90.0; 4096];
        s.waterfall.push(vec![-90.0; 4096]);
        prepare(&mut s, Rect::new(0, 0, 160, 50));
        let (_, hits) = render_offscreen(&s, 160, 50);
        let rects: Vec<[u16; 4]> = hits
            .overlays
            .iter()
            .map(|o| match o {
                Overlay::Spectrum { rect, .. }
                | Overlay::Waterfall { rect, .. }
                | Overlay::Picture { rect, .. } => *rect,
            })
            .collect();
        assert_eq!(rects.len(), 2, "{:?}", hits.overlays);
        for &[x, y, w, h] in &rects {
            assert!(w > 0 && h > 0 && x + w <= 160 && y + h <= 50);
        }
        // the waterfall sits under the spectrum, in the same columns
        assert_eq!(rects_x(&hits.overlays), (rects[0][0], rects[0][2]));
    }

    fn rects_x(o: &[crate::remote::wire::Overlay]) -> (u16, u16) {
        match &o[1] {
            crate::remote::wire::Overlay::Waterfall { rect, .. } => (rect[0], rect[2]),
            other => panic!("{other:?}"),
        }
    }
}
