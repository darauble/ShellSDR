//! Which cases are switched on (AGENTS.md §4: "Turn cases on in the runner a
//! few at a time, in the milestone order"). A case that is not on is
//! reported as "not implemented yet".

#[allow(dead_code)] // All and Ids are used from M1 on
pub enum On {
    None,
    All,
    Ids(&'static [&'static str]),
    AllExcept(&'static [&'static str]),
}

pub fn on(suite: &str) -> On {
    match suite {
        "codewords" => On::All,
        "parse" => On::All,
        "alerts" => On::All,
        "search-terms" => On::All,
        "search" => On::All,
        "list-row" => On::All,
        "iso8601" => On::All,
        "config-merge" => On::All,
        "ndjson-write" => On::All,
        "ndjson-read" => On::All,
        "distance" => On::All,
        "parse-location" => On::All,
        "file-name-time" => On::All,
        "day-file" => On::All,
        "mmsi-lookup" => On::All,
        "audio-decode" => On::All,
        "detail" => On::All,
        _ => On::None,
    }
}

pub fn is_enabled(suite: &str, id: &str) -> bool {
    match on(suite) {
        On::None => false,
        On::All => true,
        On::Ids(ids) => ids.contains(&id),
        On::AllExcept(ids) => !ids.contains(&id),
    }
}

/// Cases that fail because of an open finding (FINDINGS.md). They are
/// reported as `xfail` with the finding, and fail the run if they pass.
pub fn xfail(suite: &str, id: &str) -> Option<&'static str> {
    // B-01..B-03 were decided upstream (seed F-34..F-36); none is open.
    let _ = (suite, id);
    None
}
