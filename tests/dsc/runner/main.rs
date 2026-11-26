//! Conformance runner (harness = false): every case of every suite in
//! tests/conformance/manifest.json, reported by suite and id as pass, fail,
//! informative difference, or not implemented yet.
//!
//! `cargo test --test conformance` prints a summary and every case that is
//! not a pass; `-- -v` prints every case. The full report is written to
//! target/conformance-report.json. Exit status 1 on any failure.

#[path = "adapters.rs"]
mod adapters;
#[path = "compare.rs"]
mod compare;
#[path = "enabled.rs"]
mod enabled;
#[path = "report.rs"]
mod report;
#[path = "rules.rs"]
mod rules;
#[path = "shape.rs"]
mod shape;
#[path = "suites.rs"]
mod suites;

use report::Outcome;
use serde_json::{Value, json};

fn main() {
    let verbose = std::env::args().any(|a| a == "-v" || a == "--verbose");
    let mut failed = 0usize;
    let mut rows = Vec::new();
    println!(
        "{:<16} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
        "suite", "cases", "pass", "fail", "info", "todo", "xfail"
    );
    for suite in suites::load_suites() {
        let (mut pass, mut fail, mut info, mut todo, mut xfail) = (0, 0, 0, 0, 0);
        let mut lines = Vec::new();
        for case in suite.cases() {
            let id = case["id"].as_str().unwrap_or("?");
            let outcome = run_case(&suite, case, id);
            let mut finding = None;
            let (word, diffs) = match &outcome {
                Outcome::ExpectedFailure(f, d) => {
                    xfail += 1;
                    finding = Some(*f);
                    ("xfail", Some(d))
                }
                Outcome::Pass => {
                    pass += 1;
                    ("pass", None)
                }
                Outcome::Fail(d) => {
                    fail += 1;
                    ("FAIL", Some(d))
                }
                Outcome::Informative(d) => {
                    info += 1;
                    ("info", Some(d))
                }
                Outcome::NotImplemented => {
                    todo += 1;
                    ("todo", None)
                }
            };
            if verbose || diffs.is_some() {
                lines.push(format!(
                    "  {word} {}/{id}{}",
                    suite.name,
                    finding.map_or(String::new(), |f| format!(" ({f})"))
                ));
                for d in diffs.into_iter().flatten() {
                    lines.push(format!(
                        "       {}{}: expected {} got {}",
                        if d.informative { "(informative) " } else { "" },
                        if d.path.is_empty() {
                            "<value>"
                        } else {
                            &d.path
                        },
                        d.expected,
                        d.actual
                            .as_ref()
                            .map_or("<absent>".to_string(), |a| a.to_string())
                    ));
                }
            }
            rows.push(
                json!({"suite": suite.name, "id": id, "outcome": word.to_lowercase(),
                             "finding": finding, "req": case["req"].clone()}),
            );
        }
        failed += fail;
        println!(
            "{:<16} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
            suite.name,
            suite.cases().len(),
            pass,
            fail,
            info,
            todo,
            xfail
        );
        for l in lines {
            println!("{l}");
        }
    }
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/conformance-report.json");
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&rows).unwrap_or_default(),
    );
    if failed > 0 {
        println!("conformance: {failed} case(s) failed");
        std::process::exit(1);
    }
}

fn run_case(suite: &suites::Suite, case: &Value, id: &str) -> Outcome {
    if !enabled::is_enabled(&suite.name, id) {
        return Outcome::NotImplemented;
    }
    let Some(mut actual) = adapters::actual(&suite.name, case, &suite.json) else {
        let d = compare::Diff {
            path: String::new(),
            expected: json!("an adapter"),
            actual: None,
            informative: false,
        };
        return Outcome::Fail(vec![d]);
    };
    let mut expected = case["expected"].clone();
    report::prepare(&suite.name, &mut expected, &mut actual);
    let rules = rules::rules_for(&suite.json, case);
    report::settle(
        report::classify(compare::compare(&expected, &actual, &rules)),
        enabled::xfail(&suite.name, id),
    )
}
