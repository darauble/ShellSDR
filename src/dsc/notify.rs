//! Desktop notifications (group `notify`, ALR-04): through `notify-send`,
//! which talks to the desktop's notification daemon over D-Bus.

/// The `notify-send` arguments: critical urgency for distress.
pub fn args(summary: &str, body: &str, critical: bool) -> Vec<String> {
    vec![
        "--app-name=dsqrust".into(),
        format!("--urgency={}", if critical { "critical" } else { "normal" }),
        // Texts from the air never become options.
        "--".into(),
        summary.into(),
        body.into(),
    ]
}

/// Sends a notification; does not wait for it to be shown.
pub fn desktop(summary: &str, body: &str, critical: bool) -> Result<(), String> {
    std::process::Command::new("notify-send")
        .args(args(summary, body, critical))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("notify-send: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distress_is_critical_and_texts_are_plain_arguments() {
        assert_eq!(
            args("DSC DISTRESS", "From 258999119 — --fire", true),
            [
                "--app-name=dsqrust",
                "--urgency=critical",
                "--",
                "DSC DISTRESS",
                "From 258999119 — --fire"
            ]
        );
        assert_eq!(args("DSC URGENCY", "b", false)[1], "--urgency=normal");
    }
}
