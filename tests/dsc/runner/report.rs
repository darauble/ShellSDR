//! What a case's comparison means for the report.
#![allow(dead_code)]

use crate::compare::Diff;
use serde_json::Value;

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Pass,
    /// At least one normative difference.
    Fail(Vec<Diff>),
    /// Only informative differences.
    Informative(Vec<Diff>),
    /// The case is not switched on yet.
    NotImplemented,
    /// Fails as an open finding says it does (AGENTS.md §5).
    ExpectedFailure(&'static str, Vec<Diff>),
}

/// Apply an expected failure: a failing case that cites an open finding is
/// reported as such; if it passes, that is a failure (the mark is stale).
pub fn settle(outcome: Outcome, xfail: Option<&'static str>) -> Outcome {
    match (outcome, xfail) {
        (Outcome::Fail(d), Some(f)) => Outcome::ExpectedFailure(f, d),
        (Outcome::Pass | Outcome::Informative(_), Some(f)) => {
            let stale = Diff {
                path: String::new(),
                expected: Value::String(format!("a failure (expected failure of {f})")),
                actual: Some(Value::String(
                    "pass: remove the expected-failure mark".into(),
                )),
                informative: false,
            };
            Outcome::Fail(vec![stale])
        }
        (o, _) => o,
    }
}

pub fn classify(diffs: Vec<Diff>) -> Outcome {
    if diffs.is_empty() {
        Outcome::Pass
    } else if diffs.iter().all(|d| d.informative) {
        Outcome::Informative(diffs)
    } else {
        Outcome::Fail(diffs)
    }
}

/// Suite-specific shaping before the comparison.
pub fn prepare(suite: &str, expected: &mut Value, actual: &mut Value) {
    if suite == "ndjson-write" {
        // "Compare as JSON values": the expected line is parsed too.
        if let Some(line) = expected
            .get("line")
            .and_then(Value::as_str)
            .map(String::from)
        {
            expected["line"] = serde_json::from_str(&line).unwrap_or(Value::String(line));
        }
    }
    if suite == "search-terms" {
        // "The same terms, compared exactly as strings; order is free,
        // duplicates count once."
        for v in [expected, actual] {
            if let Some(Value::Array(terms)) = v.get_mut("terms") {
                terms.sort_by_key(|a| a.to_string());
                terms.dedup();
            }
        }
    }
}
