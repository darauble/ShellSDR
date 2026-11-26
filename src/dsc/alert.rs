//! The alert decision (seed spec/04-algorithms/04-support.md §4.7, ALR-01).

use crate::dsc::message::{Category, Format, Message, Severity};

/// Whether a newly decoded message raises an alert.
pub fn decide(m: &Message, alert_on_urgency: bool) -> bool {
    if m.format_unconfirmed {
        return false; // §4.2: one copy of 112/116 is not enough
    }
    if m.format == Format::Vts || m.category == Some(Category::Vts) {
        return false; // Table A1-3 note 3: no alarms
    }
    if m.eos.is_none() {
        return false; // a cut call: fields unreliable (F-27)
    }
    m.severity == Severity::Distress || (m.severity == Severity::Urgency && alert_on_urgency)
}
