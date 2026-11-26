//! The settings schema: the JSON API between a VFO type, the VFO panel and
//! the bookmark editor (spec 13.3).
//!
//! A type describes its settings as a list of [`Field`]s; a settings object
//! is a JSON map keyed by `Field::key`. One generic form (the VFO panel rows
//! and the bookmark editor's type rows) is built from the schema, so a new
//! type needs no UI code of its own for its settings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A type's settings: a JSON object keyed by field key.
pub type Settings = serde_json::Map<String, Value>;

/// One option of a `choice` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub value: Value,
    pub label: String,
}

/// What kind of value a field holds, with its kind-specific limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    Text {
        max_len: usize,
    },
    Integer {
        min: i64,
        max: i64,
        step: i64,
        #[serde(default)]
        unit: String,
    },
    Number {
        min: f64,
        max: f64,
        step: f64,
        #[serde(default)]
        unit: String,
    },
    FrequencyHz,
    /// A folder path, edited in a text prompt (empty = the type's default).
    Path,
    Choice {
        options: Vec<ChoiceOption>,
    },
    Bool,
}

/// One setting of a VFO type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: FieldKind,
    pub default: Value,
    /// Stored in (and applied from) bookmarks.
    #[serde(default)]
    pub bookmark: bool,
    /// Shown only while each named field holds one of the listed values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_if: Option<BTreeMap<String, Vec<Value>>>,
}

/// A type's settings schema, computed for its current settings (ranges and
/// steps may depend on other fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub vfo_type: String,
    pub fields: Vec<Field>,
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// Format a number without a needless `.0` ("2800", "12.5").
pub fn fmt_num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// A frequency span as "200 Hz" / "2.3 kHz" / "12.5 kHz".
pub fn hz_label(hz: f64) -> String {
    if hz < 1000.0 {
        format!("{hz:.0} Hz")
    } else {
        let s = format!("{:.1}", hz / 1e3);
        format!("{} kHz", s.trim_end_matches(".0"))
    }
}

impl Field {
    /// Whether the field is shown for `settings` (its `visible_if`).
    pub fn visible(&self, settings: &Settings) -> bool {
        self.visible_if.as_ref().is_none_or(|conds| {
            conds
                .iter()
                .all(|(k, vals)| settings.get(k).is_some_and(|v| vals.contains(v)))
        })
    }

    /// The value in `settings`, or the default.
    pub fn value<'a>(&'a self, settings: &'a Settings) -> &'a Value {
        settings.get(&self.key).unwrap_or(&self.default)
    }

    /// The value as shown in a form (choice label, number with unit).
    pub fn display(&self, v: &Value) -> String {
        match &self.kind {
            FieldKind::Text { .. } => v.as_str().unwrap_or("").to_string(),
            FieldKind::Path => match v.as_str().unwrap_or("") {
                "" => "(default)".to_string(),
                p => p.to_string(),
            },
            FieldKind::Integer { unit, .. } | FieldKind::Number { unit, .. } => {
                let n = as_f64(v).map(fmt_num).unwrap_or_else(|| "?".into());
                if unit.is_empty() {
                    n
                } else {
                    format!("{n} {unit}")
                }
            }
            FieldKind::FrequencyHz => {
                format!("{} Hz", as_f64(v).map(fmt_num).unwrap_or_default())
            }
            FieldKind::Choice { options } => options
                .iter()
                .find(|o| &o.value == v)
                .map(|o| o.label.clone())
                .unwrap_or_else(|| format!("{v}")),
            FieldKind::Bool => {
                if v.as_bool().unwrap_or(false) {
                    "[x]".into()
                } else {
                    "[ ]".into()
                }
            }
        }
    }

    /// Step the value by `dir` (+1 / -1): choices cycle, numbers move by
    /// their step within their range, a bool toggles. Text is unchanged.
    pub fn step(&self, v: &Value, dir: i32) -> Value {
        match &self.kind {
            FieldKind::Choice { options } if !options.is_empty() => {
                let n = options.len() as i32;
                let i = options.iter().position(|o| &o.value == v).unwrap_or(0) as i32;
                options[((i + dir) % n + n) as usize % options.len()]
                    .value
                    .clone()
            }
            FieldKind::Integer { min, max, step, .. } => {
                let cur = v.as_i64().unwrap_or(*min);
                Value::from((cur + step * dir as i64).clamp(*min, *max))
            }
            FieldKind::Number { min, max, step, .. } => {
                let cur = as_f64(v).unwrap_or(*min);
                Value::from((cur + step * dir as f64).clamp(*min, *max))
            }
            FieldKind::Bool => Value::from(!v.as_bool().unwrap_or(false)),
            _ => v.clone(),
        }
    }

    /// Parse typed text into a value of this field (numbers checked against
    /// the range). Errors name the field.
    pub fn parse(&self, text: &str) -> Result<Value, String> {
        let t = text.trim();
        match &self.kind {
            FieldKind::Text { max_len } => {
                if t.chars().count() > *max_len {
                    Err(format!("{}: at most {max_len} characters", self.label))
                } else {
                    Ok(Value::from(t))
                }
            }
            FieldKind::Path => Ok(Value::from(t)),
            FieldKind::Integer { min, max, .. } => match t.parse::<i64>() {
                Ok(n) if (*min..=*max).contains(&n) => Ok(Value::from(n)),
                _ => Err(format!("{}: a whole number {min}..{max}", self.label)),
            },
            FieldKind::Number { min, max, .. } => match t.parse::<f64>() {
                Ok(n) if n.is_finite() && n >= *min && n <= *max => Ok(Value::from(n)),
                _ => Err(format!(
                    "{}: a number {}..{}",
                    self.label,
                    fmt_num(*min),
                    fmt_num(*max)
                )),
            },
            FieldKind::FrequencyHz => t
                .parse::<u64>()
                .map(Value::from)
                .map_err(|_| format!("{}: a frequency in Hz", self.label)),
            FieldKind::Choice { options } => options
                .iter()
                .find(|o| o.label.eq_ignore_ascii_case(t))
                .map(|o| o.value.clone())
                .ok_or_else(|| format!("{}: one of the listed options", self.label)),
            FieldKind::Bool => match t {
                "1" | "true" | "yes" | "on" => Ok(Value::from(true)),
                "0" | "false" | "no" | "off" => Ok(Value::from(false)),
                _ => Err(format!("{}: yes or no", self.label)),
            },
        }
    }

    /// Whether `v` is acceptable for this field (type and range).
    pub fn accepts(&self, v: &Value) -> bool {
        match &self.kind {
            FieldKind::Text { max_len } => {
                v.as_str().is_some_and(|s| s.chars().count() <= *max_len)
            }
            FieldKind::Integer { min, max, .. } => {
                v.as_i64().is_some_and(|n| (*min..=*max).contains(&n))
            }
            FieldKind::Number { min, max, .. } => {
                as_f64(v).is_some_and(|n| n.is_finite() && n >= *min && n <= *max)
            }
            FieldKind::FrequencyHz => v.as_u64().is_some(),
            FieldKind::Path => v.is_string(),
            FieldKind::Choice { options } => options.iter().any(|o| &o.value == v),
            FieldKind::Bool => v.is_boolean(),
        }
    }
}

impl Schema {
    /// The field with `key`.
    pub fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// The fields shown for `settings`; with `bookmark_only`, only those a
    /// bookmark stores.
    pub fn visible_fields(&self, settings: &Settings, bookmark_only: bool) -> Vec<&Field> {
        self.fields
            .iter()
            .filter(|f| (!bookmark_only || f.bookmark) && f.visible(settings))
            .collect()
    }

    /// The defaults of every field.
    pub fn defaults(&self) -> Settings {
        self.fields
            .iter()
            .map(|f| (f.key.clone(), f.default.clone()))
            .collect()
    }

    /// The bookmark part of `settings` (the `bookmark: true` fields).
    pub fn bookmark_part(&self, settings: &Settings) -> Settings {
        self.fields
            .iter()
            .filter(|f| f.bookmark)
            .map(|f| (f.key.clone(), f.value(settings).clone()))
            .collect()
    }

    /// Check every visible field's value: the first problem, by label.
    pub fn validate(&self, settings: &Settings) -> Result<(), String> {
        for f in self.fields.iter().filter(|f| f.visible(settings)) {
            if !f.accepts(f.value(settings)) {
                return Err(format!("{}: invalid value {}", f.label, f.value(settings)));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Schema {
        serde_json::from_value(json!({
            "vfo_type": "Demo",
            "fields": [
                {"key": "mode", "label": "Mode", "kind": "choice", "default": "A",
                 "bookmark": true,
                 "options": [{"value": "A", "label": "Alpha"}, {"value": "B", "label": "Beta"}]},
                {"key": "bw", "label": "Bandwidth", "kind": "number", "default": 1000.0,
                 "min": 100.0, "max": 2000.0, "step": 500.0, "unit": "Hz", "bookmark": true},
                {"key": "tone", "label": "Tone", "kind": "integer", "default": 800,
                 "min": 250, "max": 3000, "step": 10, "visible_if": {"mode": ["B"]}},
                {"key": "on", "label": "Enabled", "kind": "bool", "default": false},
                {"key": "tag", "label": "Tag", "kind": "text", "default": "", "max_len": 4}
            ]
        }))
        .expect("schema JSON parses")
    }

    #[test]
    fn schema_round_trips_as_json() {
        let s = schema();
        let back: Schema = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert_eq!(
            serde_json::to_value(&s).unwrap()["fields"][0]["kind"],
            "choice"
        );
    }

    #[test]
    fn visibility_follows_other_fields() {
        let s = schema();
        let mut st = s.defaults();
        let keys = |st: &Settings, b| -> Vec<String> {
            s.visible_fields(st, b)
                .iter()
                .map(|f| f.key.clone())
                .collect()
        };
        assert_eq!(keys(&st, false), ["mode", "bw", "on", "tag"]);
        st.insert("mode".into(), json!("B"));
        assert_eq!(keys(&st, false), ["mode", "bw", "tone", "on", "tag"]);
        assert_eq!(keys(&st, true), ["mode", "bw"], "bookmark fields only");
    }

    #[test]
    fn step_cycles_choices_clamps_numbers_and_toggles_bools() {
        let s = schema();
        let mode = s.field("mode").unwrap();
        assert_eq!(mode.step(&json!("B"), 1), json!("A"), "wraps");
        assert_eq!(mode.step(&json!("A"), -1), json!("B"));
        let bw = s.field("bw").unwrap();
        assert_eq!(bw.step(&json!(1000.0), 1), json!(1500.0));
        assert_eq!(bw.step(&json!(1900.0), 1), json!(2000.0), "clamped");
        assert_eq!(bw.step(&json!(300.0), -1), json!(100.0));
        let tone = s.field("tone").unwrap();
        assert_eq!(tone.step(&json!(800), -1), json!(790));
        assert_eq!(s.field("on").unwrap().step(&json!(false), 1), json!(true));
    }

    #[test]
    fn display_and_parse() {
        let s = schema();
        assert_eq!(s.field("mode").unwrap().display(&json!("B")), "Beta");
        assert_eq!(s.field("bw").unwrap().display(&json!(2800.0)), "2800 Hz");
        assert_eq!(s.field("on").unwrap().display(&json!(true)), "[x]");
        assert_eq!(s.field("bw").unwrap().parse("1500"), Ok(json!(1500.0)));
        assert!(
            s.field("bw").unwrap().parse("5000").is_err(),
            "out of range"
        );
        assert_eq!(s.field("mode").unwrap().parse("beta"), Ok(json!("B")));
        assert!(s.field("tag").unwrap().parse("toolong").is_err());
        assert_eq!(s.field("tone").unwrap().parse("900"), Ok(json!(900)));
    }

    #[test]
    fn a_path_shows_default_when_empty_and_takes_any_text() {
        let f: Field = serde_json::from_value(json!(
            {"key": "dir", "label": "Messages", "kind": "path", "default": ""}))
        .unwrap();
        assert_eq!(f.display(&json!("")), "(default)");
        assert_eq!(f.display(&json!("~/dsc")), "~/dsc");
        assert_eq!(f.parse("  /tmp/x "), Ok(json!("/tmp/x")));
        assert!(f.accepts(&json!("")) && !f.accepts(&json!(3)));
    }

    #[test]
    fn validate_and_bookmark_part() {
        let s = schema();
        let mut st = s.defaults();
        assert!(s.validate(&st).is_ok());
        st.insert("bw".into(), json!(9999.0));
        assert!(s.validate(&st).unwrap_err().contains("Bandwidth"));
        let part = s.bookmark_part(&st);
        assert_eq!(part.keys().collect::<Vec<_>>(), ["bw", "mode"]);
    }
}
