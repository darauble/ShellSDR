//! Vertical scroll geometry for the left panel (pure).
//!
//! Step 3.6b. The left panel is a tall virtual column of stacked module blocks
//! (each at its natural height). A scroll `offset` (number of virtual rows
//! hidden above the top of the viewport) maps virtual rows to screen rows.
//! This module computes the clamped offset, whether a scrollbar is needed, and
//! the minimal offset that brings a target row span (a focused field or
//! module) fully into view. All pure, so it is unit-tested without a terminal.

/// A half-open row span `[start, end)` in virtual (pre-scroll) coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u16,
    pub end: u16,
}

impl Span {
    pub fn new(start: u16, height: u16) -> Self {
        Span {
            start,
            end: start.saturating_add(height),
        }
    }
}

/// Total virtual content height for a stack of module heights.
pub fn content_height(module_heights: &[u16]) -> u16 {
    module_heights
        .iter()
        .copied()
        .fold(0u16, |a, h| a.saturating_add(h))
}

/// Clamp a desired scroll offset so the viewport never scrolls past the end.
/// When content fits, the offset is 0.
pub fn clamp_offset(offset: u16, content: u16, viewport: u16) -> u16 {
    let max = content.saturating_sub(viewport);
    offset.min(max)
}

/// Whether a scrollbar/indicator is needed (content taller than viewport).
pub fn needs_scrollbar(content: u16, viewport: u16) -> bool {
    content > viewport
}

/// Given a target span (e.g. the focused field's absolute virtual rows) and
/// the current offset + viewport height, return the minimal new offset that
/// makes the whole span visible. If the span is taller than the viewport, its
/// top is aligned to the viewport top.
pub fn ensure_visible(target: Span, offset: u16, viewport: u16) -> u16 {
    if viewport == 0 {
        return offset;
    }
    let view_top = offset;
    let view_bottom = offset.saturating_add(viewport); // exclusive
    if target.start < view_top {
        // Scroll up so the target's top is at the viewport top.
        target.start
    } else if target.end > view_bottom {
        // Scroll down so the target's bottom is at the viewport bottom,
        // but never past aligning the target's top to the viewport top
        // (handles spans taller than the viewport).
        let desired = target.end.saturating_sub(viewport);
        desired.min(target.start)
    } else {
        offset
    }
}

/// One page of scrolling is the viewport height minus a one-row overlap for
/// context (at least 1).
pub fn page_step(viewport: u16) -> u16 {
    viewport.saturating_sub(1).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_height_sums_modules() {
        assert_eq!(content_height(&[3, 10, 3]), 16);
        assert_eq!(content_height(&[]), 0);
    }

    #[test]
    fn clamp_offset_never_scrolls_past_end() {
        // content 20, viewport 8 -> max offset 12.
        assert_eq!(clamp_offset(0, 20, 8), 0);
        assert_eq!(clamp_offset(12, 20, 8), 12);
        assert_eq!(clamp_offset(50, 20, 8), 12, "clamped to max");
        // content fits -> offset 0.
        assert_eq!(clamp_offset(5, 6, 8), 0);
    }

    #[test]
    fn needs_scrollbar_only_when_overflowing() {
        assert!(needs_scrollbar(20, 8));
        assert!(!needs_scrollbar(8, 8));
        assert!(!needs_scrollbar(3, 8));
    }

    #[test]
    fn ensure_visible_scrolls_up_for_target_above() {
        // Viewport shows [10,18); target at rows [4,6) is above.
        let off = ensure_visible(Span { start: 4, end: 6 }, 10, 8);
        assert_eq!(off, 4, "scroll up to the target top");
    }

    #[test]
    fn ensure_visible_scrolls_down_for_target_below() {
        // Viewport [0,8); target [10,12) is below -> bottom-align: 12-8 = 4.
        let off = ensure_visible(Span { start: 10, end: 12 }, 0, 8);
        assert_eq!(off, 4);
    }

    #[test]
    fn ensure_visible_noop_when_already_visible() {
        // Viewport [4,12); target [6,9) fully inside.
        let off = ensure_visible(Span { start: 6, end: 9 }, 4, 8);
        assert_eq!(off, 4);
    }

    #[test]
    fn ensure_visible_aligns_top_for_tall_target() {
        // Target [5,25) is taller than the viewport (8). Align its top.
        let off = ensure_visible(Span { start: 5, end: 25 }, 0, 8);
        assert_eq!(off, 5, "tall target: top-aligned");
    }

    #[test]
    fn page_step_is_viewport_minus_one() {
        assert_eq!(page_step(10), 9);
        assert_eq!(page_step(1), 1);
        assert_eq!(page_step(0), 1);
    }
}
