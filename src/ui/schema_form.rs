//! Rows of a schema-driven form (spec 13.3): the shared text of a VFO type's
//! setting rows in the VFO panel and the bookmark editor.

use crate::vfo::types::{Field, FieldKind};
use serde_json::Value;

/// One row: `Label: value`; a checkbox reads `[x] Label`.
pub fn row_text(f: &Field, v: &Value) -> String {
    match f.kind {
        FieldKind::Bool => format!("{} {}", f.display(v), f.label),
        _ => format!("{}: {}", f.label, f.display(v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfo::types::{VfoType, voice::VoiceSettings, voice::VoiceType};

    #[test]
    fn voice_rows_read_like_the_old_panel() {
        let s = VoiceSettings::for_mode(crate::vfo::model::Mode::Usb).to_settings();
        let schema = VoiceType.schema(&s);
        let rows: Vec<String> = schema
            .visible_fields(&s, false)
            .iter()
            .map(|f| row_text(f, f.value(&s)))
            .collect();
        assert_eq!(rows, ["Mode: USB", "Bandwidth: 2800 Hz", "Snap: 100 Hz"]);
        let b: Field = serde_json::from_value(serde_json::json!(
            {"key": "x", "label": "Notch", "kind": "bool", "default": false}))
        .unwrap();
        assert_eq!(row_text(&b, &Value::from(true)), "[x] Notch");
    }
}
