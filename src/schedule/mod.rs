//! Recording scheduler (Step 8b).
//!
//! `window`: the weekly UTC time-window core, shared with bookmarks.
//! `job`: a scheduled recording (a bookmark + a window + a record type).
//! `planner`: the level-triggered scheduler state machine (pure).
//! `store`: the per-instance schedule file (JSON, atomic save).

pub mod editor;
pub mod glue;
pub mod job;
pub mod module;
pub mod planner;
pub mod store;
pub mod window;
