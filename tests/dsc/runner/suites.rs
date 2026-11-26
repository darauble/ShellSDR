//! Loading the conformance suites listed in tests/conformance/manifest.json.
#![allow(dead_code)]

use serde_json::Value;
use std::path::{Path, PathBuf};

/// The copied seed conformance directory.
pub fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dsc/conformance")
}

/// One suite file with its manifest entry.
pub struct Suite {
    pub file: String,
    pub name: String,
    /// Case count stated by the manifest.
    pub manifest_cases: usize,
    /// The whole suite file.
    pub json: Value,
}

impl Suite {
    pub fn cases(&self) -> &[Value] {
        self.json["cases"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Every suite of the manifest, in manifest order.
pub fn load_suites() -> Vec<Suite> {
    let dir = conformance_dir();
    let manifest = read_json(&dir.join("manifest.json"));
    manifest["suites"]
        .as_array()
        .expect("manifest.json: suites")
        .iter()
        .map(|m| {
            let file = m["file"].as_str().expect("manifest: file").to_string();
            Suite {
                json: read_json(&dir.join(&file)),
                name: m["suite"].as_str().expect("manifest: suite").to_string(),
                manifest_cases: m["cases"].as_u64().expect("manifest: cases") as usize,
                file,
            }
        })
        .collect()
}

fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
