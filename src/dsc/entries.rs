//! The messages in view, time-ordered, with a revision that changes on
//! every change (02-architecture.md "Store"). A snapshot is cheap: the list
//! is shared until the next change.

use crate::dsc::record::Record;
use std::path::PathBuf;
use std::sync::Arc;

/// One message in view.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Runtime sequence number (not stored).
    pub seq: u64,
    pub record: Record,
    /// The day file it belongs to (VIEW-09).
    pub file: PathBuf,
}

/// Messages and revision, taken together (rule 3).
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub entries: Arc<Vec<Arc<Entry>>>,
    pub revision: u64,
    /// The day files in view.
    pub files: Vec<PathBuf>,
}

#[derive(Default)]
pub struct Store {
    entries: Arc<Vec<Arc<Entry>>>,
    revision: u64,
    files: Vec<PathBuf>,
}

fn key(e: &Entry) -> (i64, u64) {
    (e.record.utc, e.seq)
}

impl Store {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self.entries.clone(),
            revision: self.revision,
            files: self.files.clone(),
        }
    }

    pub fn replace(&mut self, mut entries: Vec<Arc<Entry>>, files: Vec<PathBuf>) {
        entries.sort_by_key(|e| key(e));
        self.entries = Arc::new(entries);
        let mut files = files;
        files.sort();
        files.dedup();
        self.files = files;
        self.revision += 1;
    }

    /// Inserts in time order (messages can arrive out of order).
    pub fn add(&mut self, e: Arc<Entry>) {
        if !self.files.contains(&e.file) {
            self.files.push(e.file.clone());
            self.files.sort();
        }
        let list = Arc::make_mut(&mut self.entries);
        let at = list.partition_point(|x| key(x) <= key(&e));
        list.insert(at, e);
        self.revision += 1;
    }

    pub fn update(&mut self, seq: u64, record: Record) -> bool {
        let list = Arc::make_mut(&mut self.entries);
        let Some(e) = list.iter_mut().find(|e| e.seq == seq) else {
            return false;
        };
        *e = Arc::new(Entry {
            seq,
            record,
            file: e.file.clone(),
        });
        self.revision += 1;
        true
    }

    pub fn get(&self, seq: u64) -> Option<Arc<Entry>> {
        self.entries.iter().find(|e| e.seq == seq).cloned()
    }
}
