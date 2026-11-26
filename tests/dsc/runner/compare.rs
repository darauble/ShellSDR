//! Comparison of an actual result with a case's `expected`, by the rules of
//! tests/conformance/README.md ("Comparison rules").
#![allow(dead_code)]

use serde_json::Value;

/// How one case is compared.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    /// `float_abs_tolerance`.
    pub abs_tol: Option<f64>,
    /// `float_rel_tolerance`.
    pub rel_tol: Option<f64>,
    /// `float_abs_tolerance_overrides`: dotted path -> tolerance.
    pub abs_overrides: Vec<(String, f64)>,
    /// Dotted paths of informative fields (`non_normative_fields` and
    /// suite-level informative keys).
    pub informative: Vec<String>,
    /// The whole case (or suite) is informative.
    pub all_informative: bool,
    /// Keys of `actual` that `expected` does not name are differences.
    pub strict_keys: bool,
}

/// One difference between expected and actual.
#[derive(Debug, Clone, PartialEq)]
pub struct Diff {
    /// Dotted path of object keys; array items as `[i]`.
    pub path: String,
    pub expected: Value,
    /// `None`: absent in actual.
    pub actual: Option<Value>,
    /// The difference is in an informative field.
    pub informative: bool,
}

pub fn compare(expected: &Value, actual: &Value, rules: &Rules) -> Vec<Diff> {
    let mut diffs = Vec::new();
    walk("", expected, Some(actual), rules, &mut diffs);
    diffs
}

fn walk(path: &str, expected: &Value, actual: Option<&Value>, rules: &Rules, out: &mut Vec<Diff>) {
    // Rule 1: JSON null in `actual` is the same as a missing key.
    let actual = actual.filter(|a| !a.is_null());
    match (expected, actual) {
        (Value::Null, None) => {}
        (Value::Object(e), Some(Value::Object(a))) => {
            for (k, ev) in e {
                walk(&join(path, k), ev, a.get(k), rules, out);
            }
            if rules.strict_keys {
                for (k, av) in a
                    .iter()
                    .filter(|(k, v)| !e.contains_key(*k) && !v.is_null())
                {
                    let p = join(path, k);
                    let informative = is_informative(&p, rules);
                    out.push(Diff {
                        path: p,
                        expected: Value::Null,
                        actual: Some(av.clone()),
                        informative,
                    });
                }
            }
        }
        (Value::Array(e), Some(Value::Array(a))) if e.len() == a.len() => {
            for (i, (ev, av)) in e.iter().zip(a).enumerate() {
                walk(&format!("{path}[{i}]"), ev, Some(av), rules, out);
            }
        }
        // Rule 3: strings, integers and booleans exactly.
        (e, Some(a)) if e == a => {}
        // Rule 2: numbers of which one is a float, or with a per-key
        // tolerance (audio utc_epoch), with the tolerance.
        (Value::Number(e), Some(Value::Number(a)))
            if (e.is_f64() || a.is_f64() || override_for(path, rules).is_some())
                && floats_match(path, e.as_f64(), a.as_f64(), rules) => {}
        _ => out.push(Diff {
            path: path.to_string(),
            expected: expected.clone(),
            actual: actual.cloned(),
            informative: is_informative(path, rules),
        }),
    }
}

/// Rule 5: an informative case, or a path at or below an informative key.
fn is_informative(path: &str, rules: &Rules) -> bool {
    rules.all_informative
        || rules.informative.iter().any(|p| {
            path.strip_prefix(p.as_str()).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with('.') || rest.starts_with('[')
            })
        })
}

fn floats_match(path: &str, e: Option<f64>, a: Option<f64>, rules: &Rules) -> bool {
    let (Some(e), Some(a)) = (e, a) else {
        return false;
    };
    let diff = (a - e).abs();
    if let Some(t) = override_for(path, rules) {
        return diff <= t;
    }
    if let Some(t) = rules.abs_tol {
        return diff <= t;
    }
    if let Some(t) = rules.rel_tol {
        return diff <= t * e.abs();
    }
    diff == 0.0
}

/// The per-key tolerance of a path; array indices are not part of the key
/// (`messages[0].utc_epoch` has the key `messages.utc_epoch`).
fn override_for(path: &str, rules: &Rules) -> Option<f64> {
    let mut key = String::with_capacity(path.len());
    let mut in_index = false;
    for c in path.chars() {
        match c {
            '[' => in_index = true,
            ']' => in_index = false,
            c if !in_index => key.push(c),
            _ => {}
        }
    }
    rules
        .abs_overrides
        .iter()
        .find(|(p, _)| *p == key)
        .map(|(_, t)| *t)
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}
