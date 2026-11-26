//! ShellSDR library crate: exposes the modules so both the binary and
//! examples/tests can use them.

pub mod app;
pub mod bookmarks;
pub mod cli;
pub mod config;
pub mod digital;
pub mod digital_app;
pub mod dsc;
pub mod dsc_app;
pub mod dsp;
pub mod engine;
pub mod engine_app;
pub mod fax;
pub mod fax_app;
pub mod freq;
#[cfg(feature = "gui")]
pub mod gui;
pub mod hfdl;
pub mod hfdl_app;
pub mod mfsk;
pub mod network_app;
pub mod page_app;
pub mod record;
pub mod remote;
pub mod report;
pub mod report_app;
pub mod schedule;
pub mod share_app;
pub mod source;
pub mod ui;
pub mod version;
pub mod vfo;
