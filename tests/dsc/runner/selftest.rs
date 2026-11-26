//! Self-tests of the runner's comparison rules (AGENTS.md M0).
#[path = "compare.rs"]
mod compare;

use compare::{Rules, compare};
use serde_json::json;

fn paths(d: &[compare::Diff]) -> Vec<&str> {
    d.iter().map(|d| d.path.as_str()).collect()
}

// Rule 1: `null` means absent; the implementation MUST NOT produce a value.
#[test]
fn rule1_null_expected_value_present_is_a_difference() {
    let d = compare(&json!({"x": null}), &json!({"x": 5}), &Rules::default());
    assert_eq!(paths(&d), ["x"]);
    assert!(!d[0].informative);
}

#[test]
fn rule1_null_expected_absent_or_null_actual_is_equal() {
    assert!(compare(&json!({"x": null}), &json!({}), &Rules::default()).is_empty());
    assert!(compare(&json!({"x": null}), &json!({"x": null}), &Rules::default()).is_empty());
}

// Rule 3: strings and integers compare exactly.
#[test]
fn rule3_equal_strings_integers_and_bools_are_equal() {
    let v = json!({"s": "INDV", "i": 477231700, "b": true, "n": {"k": "ok"}});
    assert!(compare(&v, &v.clone(), &Rules::default()).is_empty());
}

#[test]
fn rule3_strings_differ_by_case_or_space() {
    let d = compare(
        &json!({"a": "OK", "b": "x"}),
        &json!({"a": "ok", "b": "x "}),
        &Rules::default(),
    );
    assert_eq!(paths(&d), ["a", "b"]);
}

#[test]
fn rule3_integers_exact() {
    let d = compare(&json!({"a": 101}), &json!({"a": 100}), &Rules::default());
    assert_eq!(paths(&d), ["a"]);
}

#[test]
fn rule3_arrays_item_by_item_and_length() {
    let r = Rules::default();
    assert!(compare(&json!([120, 120, 0]), &json!([120, 120, 0]), &r).is_empty());
    assert_eq!(
        paths(&compare(&json!([120, 120, 0]), &json!([120, 121, 0]), &r)),
        ["[1]"]
    );
    assert_eq!(
        paths(&compare(
            &json!({"s": [1, 2]}),
            &json!({"s": [1, 2, 3]}),
            &r
        )),
        ["s"]
    );
}

// Rule 2: floats compare with the suite's tolerance; overrides per dotted key.
fn abs(t: f64) -> Rules {
    Rules {
        abs_tol: Some(t),
        ..Rules::default()
    }
}

#[test]
fn rule2_abs_tolerance() {
    assert!(compare(&json!({"x": 1.0}), &json!({"x": 1.0000005}), &abs(1e-6)).is_empty());
    assert_eq!(
        paths(&compare(
            &json!({"x": 1.0}),
            &json!({"x": 1.00001}),
            &abs(1e-6)
        )),
        ["x"]
    );
}

#[test]
fn rule2_no_tolerance_means_exact_floats() {
    let d = compare(
        &json!({"lat": 0.1}),
        &json!({"lat": 0.1000000001}),
        &Rules::default(),
    );
    assert_eq!(paths(&d), ["lat"]);
}

#[test]
fn rule2_integer_and_float_of_equal_value_are_equal() {
    assert!(
        compare(
            &json!({"tone": 800}),
            &json!({"tone": 800.0}),
            &Rules::default()
        )
        .is_empty()
    );
}

#[test]
fn rule2_rel_tolerance() {
    let r = Rules {
        rel_tol: Some(1e-9),
        ..Rules::default()
    };
    assert!(compare(&json!({"km": 10000.0}), &json!({"km": 10000.000001}), &r).is_empty());
    assert_eq!(
        paths(&compare(
            &json!({"km": 10000.0}),
            &json!({"km": 10000.0001}),
            &r
        )),
        ["km"]
    );
}

#[test]
fn rule2_override_by_dotted_path() {
    let r = Rules {
        abs_overrides: vec![("pos.lat".into(), 1e-6)],
        ..abs(1e-9)
    };
    let e = json!({"pos": {"lat": 54.583333, "lon": 54.583333}});
    let a = json!({"pos": {"lat": 54.5833333, "lon": 54.5833333}});
    assert_eq!(paths(&compare(&e, &a, &r)), ["pos.lon"]);
}

// Rule 5: informative suites, cases or fields: differences are reported,
// marked informative, and do not fail.
#[test]
fn rule5_informative_field_and_its_children() {
    let r = Rules {
        informative: vec!["summary".into(), "expansion".into()],
        ..Rules::default()
    };
    let d = compare(
        &json!({"alert": true, "summary": "DISTRESS", "expansion": {"speed_kn": 12.4}}),
        &json!({"alert": true, "summary": "Distress", "expansion": {"speed_kn": 1.0}}),
        &r,
    );
    assert_eq!(paths(&d), ["expansion.speed_kn", "summary"]); // keys in sorted order
    assert!(d.iter().all(|d| d.informative));
}

#[test]
fn rule5_informative_prefix_is_a_whole_key() {
    // "summary" must not make "summary_x" informative.
    let r = Rules {
        informative: vec!["summary".into()],
        ..Rules::default()
    };
    let d = compare(&json!({"summary_x": 1}), &json!({"summary_x": 2}), &r);
    assert!(!d[0].informative);
}

#[test]
fn rule5_informative_case_marks_every_difference() {
    let r = Rules {
        all_informative: true,
        ..Rules::default()
    };
    let d = compare(
        &json!({"a": 1, "b": {"c": 2}}),
        &json!({"a": 2, "b": {"c": 3}}),
        &r,
    );
    assert_eq!(d.len(), 2);
    assert!(d.iter().all(|d| d.informative));
}

// ndjson-write: "keys absent in expected MUST be absent".
#[test]
fn strict_keys_extra_key_is_a_difference() {
    let r = Rules {
        strict_keys: true,
        ..Rules::default()
    };
    let d = compare(
        &json!({"v": 2, "pos": {"lat": 1.0}}),
        &json!({"v": 2, "x": 0, "pos": {"lat": 1.0, "lq": "N"}}),
        &r,
    );
    assert_eq!(paths(&d), ["pos.lq", "x"]);
}

#[test]
fn extra_keys_ignored_when_not_strict() {
    assert!(
        compare(
            &json!({"v": 2}),
            &json!({"v": 2, "length": 23}),
            &Rules::default()
        )
        .is_empty()
    );
}

// AGENTS.md M0: the copied files still match manifest.json; vectors are frozen.
#[path = "suites.rs"]
mod suites;

#[test]
fn manifest_case_counts_match_the_suite_files() {
    let s = suites::load_suites();
    assert_eq!(s.len(), 17, "suites in manifest.json");
    for suite in &s {
        assert_eq!(suite.cases().len(), suite.manifest_cases, "{}", suite.file);
        assert_eq!(
            suite.json["suite"].as_str(),
            Some(suite.name.as_str()),
            "{}",
            suite.file
        );
    }
}

#[test]
fn vectors_are_unchanged_since_the_copy() {
    // tests/dsc/conformance.sha256 was taken from the seed's files at the copy.
    let root = env!("CARGO_MANIFEST_DIR");
    let out = std::process::Command::new("sha256sum")
        .args(["--check", "--quiet", "--strict", "conformance.sha256"])
        .current_dir(format!("{root}/tests/dsc"))
        .output()
        .expect("sha256sum");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[path = "rules.rs"]
mod rules;

fn suite(name: &str, normative: bool, compare: serde_json::Value) -> serde_json::Value {
    json!({"suite": name, "normative": normative, "compare": compare, "cases": []})
}

#[test]
fn rules_tolerances_come_from_the_suite_compare_block() {
    let s = suite(
        "ndjson-write",
        true,
        json!({"method": "…", "float_abs_tolerance": 1e-6,
        "float_abs_tolerance_overrides": {"pos.lat": 1e-7, "note": "text is not a tolerance"}}),
    );
    let r = rules::rules_for(&s, &json!({"id": "x"}));
    assert_eq!(r.abs_tol, Some(1e-6));
    assert_eq!(r.abs_overrides, vec![("pos.lat".to_string(), 1e-7)]);
    let r = rules::rules_for(
        &suite("distance", true, json!({"float_rel_tolerance": 1e-9})),
        &json!({}),
    );
    assert_eq!(r.rel_tol, Some(1e-9));
}

#[test]
fn rules_ndjson_write_is_strict_and_others_are_not() {
    assert!(rules::rules_for(&suite("ndjson-write", true, json!({})), &json!({})).strict_keys);
    assert!(!rules::rules_for(&suite("parse", true, json!({})), &json!({})).strict_keys);
}

#[test]
fn rules_informative_suite_case_and_fields() {
    assert!(rules::rules_for(&suite("detail", false, json!({})), &json!({})).all_informative);
    let s = suite("audio-decode", true, json!({"utc_epoch_tolerance_s": 1}));
    assert!(rules::rules_for(&s, &json!({"normative": false})).all_informative);
    assert!(!rules::rules_for(&s, &json!({})).all_informative);
    let r = rules::rules_for(
        &suite("parse", true, json!({})),
        &json!({"non_normative_fields": ["expansion"]}),
    );
    assert_eq!(r.informative, ["expansion"]);
    // alerts: "expected.alert is normative and exact; summary/body are informative."
    let r = rules::rules_for(&suite("alerts", true, json!({})), &json!({}));
    assert_eq!(r.informative, ["summary", "body"]);
}

#[test]
fn rules_audio_time_tolerance_applies_to_integer_epochs() {
    let s = suite("audio-decode", true, json!({"utc_epoch_tolerance_s": 1}));
    let r = rules::rules_for(&s, &json!({}));
    let e = json!({"messages": [{"utc_epoch": 1792498971, "utc_offset": 10800}]});
    assert!(
        compare(
            &e,
            &json!({"messages": [{"utc_epoch": 1792498972, "utc_offset": 10800}]}),
            &r
        )
        .is_empty()
    );
    assert_eq!(
        paths(&compare(
            &e,
            &json!({"messages": [{"utc_epoch": 1792498973, "utc_offset": 10800}]}),
            &r
        )),
        ["messages[0].utc_epoch"]
    );
    // The offset is still exact.
    assert_eq!(
        paths(&compare(
            &e,
            &json!({"messages": [{"utc_epoch": 1792498971, "utc_offset": 10801}]}),
            &r
        )),
        ["messages[0].utc_offset"]
    );
}

#[path = "report.rs"]
mod report;

#[test]
fn outcome_pass_fail_informative() {
    use report::{Outcome, classify};
    assert_eq!(classify(vec![]), Outcome::Pass);
    let d = |informative| compare::Diff {
        path: "x".into(),
        expected: json!(1),
        actual: None,
        informative,
    };
    assert_eq!(classify(vec![d(true)]), Outcome::Informative(vec![d(true)]));
    assert_eq!(
        classify(vec![d(true), d(false)]),
        Outcome::Fail(vec![d(true), d(false)])
    );
}

#[test]
fn search_terms_order_is_free_and_duplicates_count_once() {
    let mut e = json!({"terms": ["OK", "INDV", "2026-10-20"]});
    let mut a = json!({"terms": ["2026-10-20", "INDV", "OK", "INDV"]});
    report::prepare("search-terms", &mut e, &mut a);
    assert!(compare(&e, &a, &Rules::default()).is_empty());
    // A missing term is still a difference.
    let mut e = json!({"terms": ["OK", "INDV"]});
    let mut a = json!({"terms": ["OK"]});
    report::prepare("search-terms", &mut e, &mut a);
    assert!(!compare(&e, &a, &Rules::default()).is_empty());
}

#[test]
fn expected_failure_cites_its_finding_and_a_pass_is_stale() {
    use report::{Outcome, settle};
    let d = compare::Diff {
        path: "snr".into(),
        expected: json!(12.5),
        actual: None,
        informative: false,
    };
    assert_eq!(
        settle(Outcome::Fail(vec![d.clone()]), Some("B-02")),
        Outcome::ExpectedFailure("B-02", vec![d.clone()])
    );
    assert!(matches!(
        settle(Outcome::Pass, Some("B-02")),
        Outcome::Fail(_)
    ));
    assert_eq!(
        settle(Outcome::Fail(vec![d.clone()]), None),
        Outcome::Fail(vec![d])
    );
    assert_eq!(settle(Outcome::Pass, None), Outcome::Pass);
}
