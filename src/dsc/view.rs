//! The message list as the user sees it: search and distress-only filter
//! (VIEW-06/07), the selection with follow and hold (06-behaviour.md §6,
//! VIEW-08), the position text and the day files in view (VIEW-09).

use crate::dsc::entries::{Entry, Snapshot};
use std::sync::Arc;

#[derive(Default)]
pub struct ListView {
    snapshot: Snapshot,
    query: String,
    distress_only: bool,
    follow: bool,
    /// The selected message: its sequence number and time-order key.
    selected: Option<(i64, u64)>,
    /// Indices into the snapshot's entries, in time order.
    visible: Vec<usize>,
}

impl ListView {
    pub fn new() -> ListView {
        ListView {
            follow: true,
            ..ListView::default()
        }
    }

    /// A new snapshot of the store.
    pub fn update(&mut self, snapshot: Snapshot) {
        self.snapshot = snapshot;
        self.refilter();
    }

    pub fn revision(&self) -> u64 {
        self.snapshot.revision
    }

    fn key_of(&self, i: usize) -> (i64, u64) {
        let e = &self.snapshot.entries[i];
        (e.record.utc, e.seq)
    }

    fn passes(&self, e: &Entry) -> bool {
        (!self.distress_only || crate::dsc::presentation::distress_only(&e.record))
            && (self.query.is_empty()
                || crate::dsc::presentation::matches(
                    &self.query,
                    &crate::dsc::presentation::search_terms(&e.record),
                ))
    }

    /// Applies search and filter, then keeps the selection on a visible
    /// message: the newest in follow mode, else the same one, else the
    /// nearest one.
    fn refilter(&mut self) {
        self.visible = (0..self.snapshot.entries.len())
            .filter(|&i| self.passes(&self.snapshot.entries[i]))
            .collect();
        let Some(&last) = self.visible.last() else {
            return;
        };
        if self.follow || self.selected.is_none() {
            self.selected = Some(self.key_of(last));
            return;
        }
        let want = self.selected.unwrap_or_default();
        if self.selected_row().is_none() {
            let p = self.visible.partition_point(|&i| self.key_of(i) < want);
            let i = self.visible[p.min(self.visible.len() - 1)];
            self.selected = Some(self.key_of(i));
        }
    }

    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// The entry shown in a row.
    pub fn entry(&self, row: usize) -> Option<&Arc<Entry>> {
        self.visible
            .get(row)
            .and_then(|&i| self.snapshot.entries.get(i))
    }

    pub fn selected_row(&self) -> Option<usize> {
        let want = self.selected?;
        // The entry's time may have changed (a lookup update): match by seq.
        let p = self.visible.partition_point(|&i| self.key_of(i) < want);
        if p < self.visible.len() && self.key_of(self.visible[p]).1 == want.1 {
            return Some(p);
        }
        self.visible
            .iter()
            .position(|&i| self.snapshot.entries[i].seq == want.1)
    }

    pub fn selected(&self) -> Option<&Arc<Entry>> {
        self.selected_row().and_then(|r| self.entry(r))
    }

    pub fn follow(&self) -> bool {
        self.follow
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn distress_only(&self) -> bool {
        self.distress_only
    }

    /// Selecting by hand switches to hold.
    pub fn select_row(&mut self, row: usize) {
        if let Some(&i) = self.visible.get(row) {
            self.selected = Some(self.key_of(i));
            self.follow = false;
        }
    }

    /// Up/down by `delta` rows (a page is a larger delta); hold.
    pub fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let row = self.selected_row().unwrap_or(self.visible.len() - 1) as isize;
        self.select_row((row + delta).clamp(0, self.visible.len() as isize - 1) as usize);
    }

    pub fn first(&mut self) {
        self.select_row(0);
    }

    /// Jumping to the last message also enables follow.
    pub fn last(&mut self) {
        self.set_follow(true);
    }

    pub fn set_follow(&mut self, on: bool) {
        self.follow = on;
        if on && let Some(&i) = self.visible.last() {
            self.selected = Some(self.key_of(i));
        }
    }

    pub fn set_query(&mut self, q: &str) {
        self.query = q.trim().to_string();
        self.refilter();
    }

    pub fn set_distress_only(&mut self, on: bool) {
        self.distress_only = on;
        self.refilter();
    }

    pub fn clear_filters(&mut self) {
        self.query.clear();
        self.distress_only = false;
        self.refilter();
    }

    /// "message x of n" (n = rows after search and filter).
    pub fn position_text(&self) -> String {
        match (self.visible.len(), self.selected_row()) {
            (0, _) => "no messages".into(),
            (n, Some(r)) => format!("message {} of {n}", r + 1),
            (n, None) => format!("{n} messages"),
        }
    }

    /// The day files in view, each with whether the selected message
    /// belongs to it.
    pub fn files(&self) -> Vec<(String, bool)> {
        let sel = self.selected().map(|e| e.file.clone());
        self.snapshot
            .files
            .iter()
            .map(|f| {
                let name = f.file_name().map_or_else(
                    || f.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                (name, sel.as_ref() == Some(f))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsc::enrich::samples::{DISTRESS, SAFETY};
    use crate::dsc::record::{Record, TimeSource};
    use std::path::PathBuf;

    fn entry(seq: u64, utc: i64, symbols: &[u8]) -> Arc<Entry> {
        let record = Record::new(symbols.to_vec(), utc, 10800, TimeSource::Wallclock);
        let file = PathBuf::from(format!(
            "/m/{}",
            crate::dsc::time::day_file_name(utc, 10800)
        ));
        Arc::new(Entry { seq, record, file })
    }

    /// A snapshot as the store keeps it: in (utc, seq) order.
    fn snap(mut list: Vec<Arc<Entry>>, revision: u64) -> Snapshot {
        list.sort_by_key(|e| (e.record.utc, e.seq));
        let mut files: Vec<PathBuf> = list.iter().map(|e| e.file.clone()).collect();
        files.sort();
        files.dedup();
        Snapshot {
            entries: Arc::new(list),
            revision,
            files,
        }
    }

    fn seq(v: &ListView) -> Option<u64> {
        v.selected().map(|e| e.seq)
    }

    #[test]
    fn follow_selects_each_new_message() {
        let mut v = ListView::new();
        v.update(snap(
            vec![entry(1, 100, &SAFETY), entry(2, 200, &SAFETY)],
            1,
        ));
        assert_eq!(seq(&v), Some(2));
        v.update(snap(
            vec![
                entry(1, 100, &SAFETY),
                entry(2, 200, &SAFETY),
                entry(3, 300, &SAFETY),
            ],
            2,
        ));
        assert_eq!(seq(&v), Some(3));
        assert_eq!(v.position_text(), "message 3 of 3");
    }

    #[test]
    fn hold_keeps_the_selection_on_its_message_when_others_arrive_before_it() {
        let mut v = ListView::new();
        v.update(snap(
            vec![entry(1, 100, &SAFETY), entry(2, 200, &SAFETY)],
            1,
        ));
        v.select_row(0);
        assert!(!v.follow());
        // A late message from an earlier time goes before it.
        v.update(snap(
            vec![
                entry(1, 100, &SAFETY),
                entry(2, 200, &SAFETY),
                entry(3, 50, &SAFETY),
            ],
            2,
        ));
        assert_eq!(seq(&v), Some(1));
        assert_eq!(v.selected_row(), Some(1));
    }

    #[test]
    fn moving_holds_and_jumping_to_the_last_follows() {
        let mut v = ListView::new();
        v.update(snap(
            (1..=5).map(|i| entry(i, i as i64 * 10, &SAFETY)).collect(),
            1,
        ));
        v.move_by(-1);
        assert_eq!((seq(&v), v.follow()), (Some(4), false));
        v.move_by(-10);
        assert_eq!(seq(&v), Some(1));
        v.first();
        assert_eq!(seq(&v), Some(1));
        v.last();
        assert_eq!((seq(&v), v.follow()), (Some(5), true));
        v.set_follow(false);
        v.move_by(-2);
        v.set_follow(true);
        assert_eq!(seq(&v), Some(5), "follow jumps to the newest");
    }

    #[test]
    fn search_and_distress_only_combine_by_and() {
        let mut v = ListView::new();
        v.update(snap(
            vec![
                entry(1, 100, &SAFETY),
                entry(2, 200, &DISTRESS),
                entry(3, 300, &SAFETY),
            ],
            1,
        ));
        v.set_query("477231700");
        assert_eq!(v.len(), 2);
        v.set_distress_only(true);
        assert_eq!(v.len(), 0);
        assert_eq!(v.position_text(), "no messages");
        v.set_query("");
        assert_eq!((v.len(), seq(&v)), (1, Some(2)));
        v.clear_filters();
        assert_eq!((v.len(), v.query(), v.distress_only()), (3, "", false));
    }

    #[test]
    fn a_hidden_selection_moves_to_the_nearest_visible_message() {
        let mut v = ListView::new();
        v.update(snap(
            vec![
                entry(1, 100, &SAFETY),
                entry(2, 200, &DISTRESS),
                entry(3, 300, &SAFETY),
            ],
            1,
        ));
        v.select_row(0);
        v.set_distress_only(true);
        assert_eq!(seq(&v), Some(2));
        v.set_distress_only(false);
        assert_eq!(seq(&v), Some(2));
    }

    #[test]
    fn the_files_in_view_mark_the_selected_ones() {
        let mut v = ListView::new();
        // 2026-10-20 23:59 and 2026-10-21 00:01 local
        v.update(snap(
            vec![entry(1, 1792529940, &SAFETY), entry(2, 1792530060, &SAFETY)],
            1,
        ));
        assert_eq!(
            v.files(),
            [
                ("2026-10-20.ndjson".to_string(), false),
                ("2026-10-21.ndjson".to_string(), true)
            ]
        );
    }
}
