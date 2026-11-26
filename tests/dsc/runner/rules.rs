//! The comparison rules of one case, from its suite file (conformance/README.md).
#![allow(dead_code)]

use crate::compare::Rules;
use serde_json::Value;

pub fn rules_for(suite: &Value, case: &Value) -> Rules {
    let name = suite["suite"].as_str().unwrap_or("");
    let cmp = &suite["compare"];
    let mut abs_overrides: Vec<(String, f64)> = cmp["float_abs_tolerance_overrides"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_f64()?)))
                .collect()
        })
        .unwrap_or_default();
    if let Some(t) = cmp["utc_epoch_tolerance_s"].as_f64() {
        abs_overrides.push(("messages.utc_epoch".into(), t));
    }
    let mut informative: Vec<String> = case["non_normative_fields"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if name == "alerts" {
        // "expected.alert is normative and exact; summary/body are informative."
        informative.extend(["summary".to_string(), "body".to_string()]);
    }
    Rules {
        abs_tol: cmp["float_abs_tolerance"].as_f64(),
        rel_tol: cmp["float_rel_tolerance"].as_f64(),
        abs_overrides,
        informative,
        all_informative: suite["normative"] == Value::Bool(false)
            || case["normative"] == Value::Bool(false),
        // "keys absent in expected MUST be absent"
        strict_keys: name == "ndjson-write",
    }
}
