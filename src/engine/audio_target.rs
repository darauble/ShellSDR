//! Audio output target: how to address a chosen output (pure).
//!
//! cpal's ALSA backend enumerates ALSA PCM names (`hw:...`, `plughw:...`, the
//! generic `pipewire` PCM, etc.) but does NOT expose individual PipeWire sinks
//! such as a user's `VirtualSink`. To reach a specific PipeWire sink we open
//! the generic `pipewire` ALSA PCM with the `PIPEWIRE_NODE` environment
//! variable set to the sink's node name, which pins the stream to that node
//! (a documented pipewire-alsa feature).
//!
//! `AudioTarget` distinguishes these cases. It round-trips through a single
//! canonical string so it can persist in the existing `Option<String>` config
//! field: a bare name is a cpal device (backward-compatible with older
//! configs), and a `pw:` prefix marks a PipeWire sink by node name.

/// Prefix marking a PipeWire-sink target in the canonical string form.
const PW_PREFIX: &str = "pw:";

/// A resolved audio output selection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AudioTarget {
    /// The system default output device.
    #[default]
    Default,
    /// A cpal/ALSA device addressed by its enumerated name.
    CpalDevice(String),
    /// A PipeWire sink addressed by its node name (opened via the `pipewire`
    /// PCM + `PIPEWIRE_NODE`).
    PipeWireSink(String),
}

impl AudioTarget {
    /// Parse the canonical persisted form. `None` is `Default`; a `pw:`-prefixed
    /// string is a PipeWire sink; anything else is a cpal device name.
    pub fn from_stored(stored: Option<&str>) -> Self {
        match stored {
            None => AudioTarget::Default,
            Some(s) => {
                if let Some(node) = s.strip_prefix(PW_PREFIX) {
                    AudioTarget::PipeWireSink(node.to_string())
                } else {
                    AudioTarget::CpalDevice(s.to_string())
                }
            }
        }
    }

    /// The canonical persisted form. `Default` stores as `None` so the config
    /// field stays empty for the default case.
    pub fn to_stored(&self) -> Option<String> {
        match self {
            AudioTarget::Default => None,
            AudioTarget::CpalDevice(name) => Some(name.clone()),
            AudioTarget::PipeWireSink(node) => Some(format!("{PW_PREFIX}{node}")),
        }
    }

    /// A short human label for the picker/line display.
    pub fn label(&self) -> String {
        match self {
            AudioTarget::Default => "Default".to_string(),
            AudioTarget::CpalDevice(name) => name.clone(),
            AudioTarget::PipeWireSink(node) => node.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_stored_is_default() {
        assert_eq!(AudioTarget::from_stored(None), AudioTarget::Default);
        assert_eq!(AudioTarget::Default.to_stored(), None);
    }

    #[test]
    fn bare_name_is_cpal_device_backward_compatible() {
        // An older config holding a plain device name loads as a cpal device.
        let t = AudioTarget::from_stored(Some("hw:CARD=sofhdadsp,DEV=0"));
        assert_eq!(
            t,
            AudioTarget::CpalDevice("hw:CARD=sofhdadsp,DEV=0".to_string())
        );
        assert_eq!(t.to_stored().as_deref(), Some("hw:CARD=sofhdadsp,DEV=0"));
    }

    #[test]
    fn pw_prefixed_is_pipewire_sink() {
        let t = AudioTarget::from_stored(Some("pw:VirtualSink"));
        assert_eq!(t, AudioTarget::PipeWireSink("VirtualSink".to_string()));
        assert_eq!(t.to_stored().as_deref(), Some("pw:VirtualSink"));
    }

    #[test]
    fn round_trip_all_variants() {
        for t in [
            AudioTarget::Default,
            AudioTarget::CpalDevice("pipewire".into()),
            AudioTarget::PipeWireSink("VirtualSink".into()),
        ] {
            let stored = t.to_stored();
            assert_eq!(AudioTarget::from_stored(stored.as_deref()), t);
        }
    }

    #[test]
    fn label_is_human_readable() {
        assert_eq!(AudioTarget::Default.label(), "Default");
        assert_eq!(
            AudioTarget::PipeWireSink("VirtualSink".into()).label(),
            "VirtualSink"
        );
    }
}
