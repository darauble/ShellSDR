//! Frequency entry with digit editing (pure).
//!
//! Step 3 seam 1. Encodes the spec's frequency-entry semantics:
//! - a cursor selects one decimal digit (a power of ten);
//! - Left/Right move the cursor, Up/Down (or scroll) inc/dec the selected
//!   digit by its power of ten;
//! - `z` zeros all digits to the right of the cursor;
//! - typing a digit overwrites the selected digit and advances the cursor;
//! - edits are *pending*: the value does not take effect until committed
//!   (ENTER or focus-out);
//! - committing clamps to the allowed band, and signals whether a recenter is
//!   required (used by Free mode when the entry falls outside current bounds).
//!
//! The frequency is held as an integer number of Hz. `digits` is the number of
//! decimal positions the editor exposes (position 0 = ones, up to
//! `digits - 1`).
//
// NOTE(step3): `selected_digit` and `committed_hz` are accessors used by tests
// and the render layer; kept as part of the editor's public API.
#![allow(dead_code)]

/// Number of decimal digit positions exposed by the editor. 10 positions
/// covers up to 9_999_999_999 Hz (~10 GHz), enough for any SoapySDR device.
pub const DIGIT_POSITIONS: usize = 10;

/// Result of committing an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitResult {
    /// The committed frequency in Hz (already clamped to the band).
    pub hz: u64,
    /// True when the requested value was outside the band and clamping/recenter
    /// is needed. Free mode uses this to recenter the source.
    pub out_of_bounds: bool,
}

/// Editable frequency field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreqEntry {
    /// Committed value in Hz (what the radio is actually tuned to).
    committed_hz: u64,
    /// Working value in Hz while editing. Equals `committed_hz` when not dirty.
    working_hz: u64,
    /// Selected digit position (0 = ones place).
    cursor: usize,
    /// True when `working_hz` differs from `committed_hz` via a pending edit.
    dirty: bool,
}

impl FreqEntry {
    /// Create an entry tuned to `hz`, cursor on the kHz digit (position 3) as a
    /// sensible default for SDR tuning.
    pub fn new(hz: u64) -> Self {
        Self {
            committed_hz: hz,
            working_hz: hz,
            cursor: 3,
            dirty: false,
        }
    }

    /// The value currently shown (working value while editing).
    pub fn display_hz(&self) -> u64 {
        self.working_hz
    }

    /// The last committed value.
    pub fn committed_hz(&self) -> u64 {
        self.committed_hz
    }

    /// Selected digit position (0 = ones).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether there is an uncommitted edit.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The decimal digit (0-9) at the cursor of the working value.
    pub fn selected_digit(&self) -> u8 {
        let p = 10u64.saturating_pow(self.cursor as u32);
        ((self.working_hz / p) % 10) as u8
    }

    /// Put the cursor on digit position `pos` (0 = ones), clamped to the
    /// editor's range. Used by mouse clicks on a specific digit.
    pub fn set_cursor(&mut self, pos: usize) {
        self.cursor = pos.min(DIGIT_POSITIONS - 1);
    }

    /// Move the cursor one position left (toward higher powers of ten).
    pub fn cursor_left(&mut self) {
        if self.cursor + 1 < DIGIT_POSITIONS {
            self.cursor += 1;
        }
    }

    /// Move the cursor one position right (toward lower powers of ten).
    pub fn cursor_right(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// Increment the selected digit's place by one power of ten (with carry).
    /// Saturates at the max representable value; marks the edit pending.
    pub fn increment(&mut self) {
        let p = 10u64.saturating_pow(self.cursor as u32);
        self.working_hz = self.working_hz.saturating_add(p);
        self.dirty = self.working_hz != self.committed_hz;
    }

    /// Decrement the selected digit's place by one power of ten (with borrow).
    /// Saturates at zero; marks the edit pending.
    pub fn decrement(&mut self) {
        let p = 10u64.saturating_pow(self.cursor as u32);
        self.working_hz = self.working_hz.saturating_sub(p);
        self.dirty = self.working_hz != self.committed_hz;
    }

    /// Zero every digit to the right of (less significant than) the cursor.
    pub fn zero_right(&mut self) {
        let p = 10u64.saturating_pow(self.cursor as u32);
        self.working_hz = (self.working_hz / p) * p;
        self.dirty = self.working_hz != self.committed_hz;
    }

    /// Overwrite the selected digit with `d` (0-9) and advance the cursor one
    /// place to the right (toward lower powers), as when typing a number.
    pub fn type_digit(&mut self, d: u8) {
        if d > 9 {
            return;
        }
        let p = 10u64.saturating_pow(self.cursor as u32);
        let current = (self.working_hz / p) % 10;
        // Replace the digit at the cursor.
        self.working_hz = self.working_hz - current * p + d as u64 * p;
        self.dirty = self.working_hz != self.committed_hz;
        self.cursor_right();
    }

    /// Discard the pending edit, restoring the committed value.
    pub fn cancel(&mut self) {
        self.working_hz = self.committed_hz;
        self.dirty = false;
    }

    /// Commit the working value, clamping to `[lo, hi]`. Returns the committed
    /// frequency and whether the requested value was out of bounds.
    pub fn commit(&mut self, lo: u64, hi: u64) -> CommitResult {
        let requested = self.working_hz;
        let out_of_bounds = requested < lo || requested > hi;
        let clamped = requested.clamp(lo, hi);
        self.committed_hz = clamped;
        self.working_hz = clamped;
        self.dirty = false;
        CommitResult {
            hz: clamped,
            out_of_bounds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_committed_and_working() {
        let f = FreqEntry::new(100_000_000);
        assert_eq!(f.committed_hz(), 100_000_000);
        assert_eq!(f.display_hz(), 100_000_000);
        assert!(!f.is_dirty());
    }

    #[test]
    fn cursor_moves_within_bounds() {
        let mut f = FreqEntry::new(0);
        // Default cursor is at position 3 (kHz).
        assert_eq!(f.cursor(), 3);
        f.cursor_right();
        assert_eq!(f.cursor(), 2);
        f.cursor_left();
        f.cursor_left();
        assert_eq!(f.cursor(), 4);
        // Cannot go below 0.
        for _ in 0..10 {
            f.cursor_right();
        }
        assert_eq!(f.cursor(), 0);
        // Cannot exceed the top position.
        for _ in 0..30 {
            f.cursor_left();
        }
        assert_eq!(f.cursor(), DIGIT_POSITIONS - 1);
    }

    #[test]
    fn set_cursor_jumps_to_a_digit_and_clamps() {
        let mut f = FreqEntry::new(0);
        f.set_cursor(6);
        assert_eq!(f.cursor(), 6);
        f.set_cursor(99);
        assert_eq!(f.cursor(), DIGIT_POSITIONS - 1, "clamped to the top digit");
    }

    #[test]
    fn increment_decrement_by_power_of_ten() {
        let mut f = FreqEntry::new(100_000_000); // 100 MHz, cursor at kHz (1e3)
        f.increment();
        assert_eq!(f.display_hz(), 100_001_000, "cursor at kHz adds 1000");
        assert!(f.is_dirty());
        f.decrement();
        assert_eq!(f.display_hz(), 100_000_000);
        // Editing back to the committed value clears dirty.
        assert!(!f.is_dirty());
    }

    #[test]
    fn selected_digit_reads_cursor_place() {
        let mut f = FreqEntry::new(145_500_000); // 145.5 MHz
        // cursor at 1e3 -> digit is (145500000 / 1000) % 10 = 0
        assert_eq!(f.selected_digit(), 0);
        f.cursor_left(); // 1e4
        f.cursor_left(); // 1e5
        assert_eq!(f.selected_digit(), 5, "the 500 kHz -> 5 at 1e5");
    }

    #[test]
    fn editing_is_pending_until_commit() {
        let mut f = FreqEntry::new(100_000_000);
        f.increment();
        // Committed value unchanged until commit.
        assert_eq!(f.committed_hz(), 100_000_000);
        assert_eq!(f.display_hz(), 100_001_000);
        let r = f.commit(0, 2_000_000_000);
        assert_eq!(r.hz, 100_001_000);
        assert!(!r.out_of_bounds);
        assert_eq!(f.committed_hz(), 100_001_000);
        assert!(!f.is_dirty());
    }

    #[test]
    fn cancel_restores_committed() {
        let mut f = FreqEntry::new(100_000_000);
        f.increment();
        f.cancel();
        assert_eq!(f.display_hz(), 100_000_000);
        assert!(!f.is_dirty());
    }

    #[test]
    fn zero_right_clears_lower_digits() {
        let mut f = FreqEntry::new(145_567_890);
        // Move cursor to 1e5 (hundreds of kHz).
        f.cursor_left(); // 1e4
        f.cursor_left(); // 1e5
        f.zero_right();
        assert_eq!(f.display_hz(), 145_500_000, "digits below 1e5 zeroed");
    }

    #[test]
    fn type_digit_overwrites_and_advances() {
        let mut f = FreqEntry::new(100_000_000);
        // Put cursor at 1e8 (hundreds of MHz).
        for _ in 0..5 {
            f.cursor_left();
        }
        assert_eq!(f.cursor(), 8);
        f.type_digit(1); // set hundreds-of-MHz digit to 1 -> already 1
        assert_eq!(f.display_hz(), 100_000_000);
        assert_eq!(f.cursor(), 7, "cursor advanced right");
        f.type_digit(4); // tens-of-MHz -> 4
        assert_eq!(f.display_hz(), 140_000_000);
    }

    #[test]
    fn commit_clamps_and_flags_out_of_bounds() {
        let mut f = FreqEntry::new(100_000_000);
        // Drive the value way above the band.
        f.cursor = 9;
        f.increment();
        f.increment();
        let r = f.commit(100_000, 1_000_000_000);
        assert!(r.out_of_bounds, "above band flagged");
        assert_eq!(r.hz, 1_000_000_000, "clamped to hi");

        let mut g = FreqEntry::new(50_000);
        let r = g.commit(100_000, 1_000_000_000);
        assert!(r.out_of_bounds, "below band flagged");
        assert_eq!(r.hz, 100_000, "clamped to lo");
    }
}
