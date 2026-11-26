//! cpal audio output (hardware).
//!
//! Piece B2. Opens the default output device at 48 kHz and pulls mono f32
//! samples from a bounded channel in the audio callback, duplicating to all
//! output channels. Underruns (consumer ahead of producer) emit silence.
//! Volume is applied here, at the output stage, reading a shared atomic so a
//! change takes effect immediately (no producer-side buffering lag).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Receiver;

use super::audio_target::AudioTarget;

/// cpal device name for the generic PipeWire ALSA PCM. Opening this with the
/// `PIPEWIRE_NODE` env var set pins the stream to a specific PipeWire sink.
const PIPEWIRE_PCM: &str = "pipewire";

/// Enumerate output device names on the default host (cpal/ALSA). Best effort:
/// returns an empty list if the host cannot enumerate.
pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    match host.output_devices() {
        Ok(devs) => devs.filter_map(|d| d.name().ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// A selectable audio output: a display label plus the target used to open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputOption {
    pub label: String,
    pub target: AudioTarget,
}

/// Enumerate all selectable outputs: the cpal/ALSA PCMs plus any PipeWire
/// sinks (which the ALSA layer does not expose individually). PipeWire sinks
/// are listed with their friendly description and routed via `PIPEWIRE_NODE`.
/// The system default is not included here; callers prepend it.
pub fn output_options() -> Vec<OutputOption> {
    let mut out = Vec::new();
    // PipeWire sinks first (friendly names, and what the user most likely
    // wants), then the raw ALSA PCMs.
    for (node, desc) in pipewire_sinks() {
        let label = if desc.is_empty() {
            format!("{node} (PipeWire)")
        } else {
            format!("{desc} (PipeWire)")
        };
        out.push(OutputOption {
            label,
            target: AudioTarget::PipeWireSink(node),
        });
    }
    for name in output_device_names() {
        out.push(OutputOption {
            label: name.clone(),
            target: AudioTarget::CpalDevice(name),
        });
    }
    out
}

/// Query PipeWire sinks via `pactl list sinks`. Returns `(node_name,
/// description)` pairs. Empty when PipeWire/pactl is unavailable (e.g. a bare
/// Raspberry Pi), so the caller transparently falls back to ALSA-only.
fn pipewire_sinks() -> Vec<(String, String)> {
    let output = std::process::Command::new("pactl")
        .arg("list")
        .arg("sinks")
        .output();
    match output {
        Ok(o) if o.status.success() => parse_pactl_sinks(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Parse `pactl list sinks` output into `(node_name, description)` pairs. Pure,
/// so it is unit-tested against captured output. Each sink block has a
/// `Name:` line (the node name = `PIPEWIRE_NODE` value) followed by a
/// `Description:` line.
fn parse_pactl_sinks(text: &str) -> Vec<(String, String)> {
    let mut sinks = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Name:") {
            pending_name = Some(rest.trim().to_string());
        } else if let Some(rest) = t.strip_prefix("Description:")
            && let Some(name) = pending_name.take()
        {
            sinks.push((name, rest.trim().to_string()));
        }
    }
    sinks
}

/// One output sample: scale the (unclamped) demod output by `volume`, then
/// limit to full scale. Mirrors SDR++, where the AGC may produce peaks above
/// 1.0 and only what still exceeds full scale after the volume is clipped (by
/// the sound card there; explicitly here). Non-finite input becomes silence.
pub fn output_sample(s: f32, volume: f32) -> f32 {
    let v = s * volume;
    if v.is_finite() {
        v.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Owns the cpal stream; dropping it stops playback.
pub struct AudioOutput {
    _stream: cpal::Stream,
}

impl AudioOutput {
    /// Open an output and start playing samples pulled from `rx` (mono, at
    /// `sample_rate` Hz), scaled by the shared `volume`. `target` selects the
    /// output: the system default, a named cpal/ALSA device, or a PipeWire
    /// sink (opened via the `pipewire` PCM with `PIPEWIRE_NODE` pinned to the
    /// sink's node name).
    pub fn new(
        sample_rate: u32,
        rx: Receiver<f32>,
        volume: Arc<AtomicU32>,
        target: &AudioTarget,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        // For a PipeWire sink, pin the generic `pipewire` PCM to the node for
        // the duration of the open via `PIPEWIRE_NODE`. The var is read by
        // pipewire-alsa at PCM-open time; we set it just around opening and
        // restore it afterwards so it does not leak to the rest of the process.
        let (device, _env_guard) = match target {
            AudioTarget::PipeWireSink(node) => {
                let guard = PipeWireNodeEnv::set(node);
                let dev = Self::device_by_name(&host, PIPEWIRE_PCM)
                    .or_else(|| host.default_output_device());
                (dev, Some(guard))
            }
            AudioTarget::CpalDevice(name) => (Self::pick_device(&host, Some(name)), None),
            AudioTarget::Default => (host.default_output_device(), None),
        };
        let device = device.ok_or_else(|| "no output device available".to_string())?;

        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let channels = supported.channels() as usize;

        let config = cpal::StreamConfig {
            channels: supported.channels(),
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let err_fn = |e| eprintln!("audio stream error: {e}");
        let stream = device
            .build_output_stream(
                &config,
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                    // Fill frame by frame; one mono sample fans out to all
                    // channels. Silence on underrun.
                    for frame in out.chunks_mut(channels) {
                        let s = output_sample(rx.try_recv().unwrap_or(0.0), vol);
                        for ch in frame.iter_mut() {
                            *ch = s;
                        }
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| e.to_string())?;

        stream.play().map_err(|e| e.to_string())?;
        Ok(Self { _stream: stream })
    }

    /// Resolve the output device by name, falling back to the host default
    /// when `name` is `None` or does not match any enumerated device.
    fn pick_device(host: &cpal::Host, name: Option<&str>) -> Option<cpal::Device> {
        if let Some(want) = name
            && let Some(dev) = Self::device_by_name(host, want)
        {
            return Some(dev);
        }
        host.default_output_device()
    }

    /// Find an output device by exact name, if present.
    fn device_by_name(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
        host.output_devices()
            .ok()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
    }
}

/// Scoped setter for the `PIPEWIRE_NODE` environment variable. Sets it on
/// construction and restores the previous value (or removes it) on drop, so
/// pinning the `pipewire` PCM to a sink does not leak to the rest of the
/// process. Only meaningful around a stream open on the same thread.
struct PipeWireNodeEnv {
    previous: Option<std::ffi::OsString>,
}

impl PipeWireNodeEnv {
    fn set(node: &str) -> Self {
        let previous = std::env::var_os("PIPEWIRE_NODE");
        // SAFETY: set before opening the stream on this thread; no other
        // thread touches PIPEWIRE_NODE. Restored on drop.
        unsafe {
            std::env::set_var("PIPEWIRE_NODE", node);
        }
        Self { previous }
    }
}

impl Drop for PipeWireNodeEnv {
    fn drop(&mut self) {
        // SAFETY: restore/remove the variable we set.
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var("PIPEWIRE_NODE", v),
                None => std::env::remove_var("PIPEWIRE_NODE"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_is_applied_before_the_full_scale_limit() {
        // SDR++ order: the unclamped demod output is scaled by the volume and
        // only then limited to full scale. A 1.8 peak at volume 0.45 must come
        // out at 0.81, not be flattened at 1.0 first and then scaled to 0.45
        // (the "rectangular at mid level" bug).
        assert!((output_sample(1.8, 0.45) - 0.81).abs() < 1e-6);
        assert!((output_sample(-1.8, 0.45) + 0.81).abs() < 1e-6);
        // Anything still beyond full scale after volume is limited.
        assert_eq!(output_sample(3.0, 1.0), 1.0);
        assert_eq!(output_sample(-3.0, 1.0), -1.0);
        // Non-finite input never reaches the sound card.
        assert_eq!(output_sample(f32::NAN, 0.5), 0.0);
    }

    #[test]
    fn parses_pactl_sink_name_description_pairs() {
        let sample = "\
Sink #56
\tName: alsa_output.usb-DragonFly.analog-stereo
\tDescription: DragonFly Red Analog Stereo
Sink #60
\tName: alsa_output.pci.HiFi__Speaker__sink
\tDescription: Speaker
Sink #4968
\tName: VirtualSink
\tDescription: VirtualSink Audio/Sink sink
";
        let sinks = parse_pactl_sinks(sample);
        assert_eq!(
            sinks,
            vec![
                (
                    "alsa_output.usb-DragonFly.analog-stereo".to_string(),
                    "DragonFly Red Analog Stereo".to_string()
                ),
                (
                    "alsa_output.pci.HiFi__Speaker__sink".to_string(),
                    "Speaker".to_string()
                ),
                (
                    "VirtualSink".to_string(),
                    "VirtualSink Audio/Sink sink".to_string()
                ),
            ]
        );
    }

    #[test]
    fn parse_pactl_sinks_handles_empty_and_garbage() {
        assert!(parse_pactl_sinks("").is_empty());
        assert!(parse_pactl_sinks("no sinks here\njust text").is_empty());
    }

    #[test]
    fn a_name_without_description_is_skipped() {
        // A dangling Name: with no following Description: is not emitted.
        let sample = "\tName: Orphan\nSink #2\n\tName: Good\n\tDescription: Good Sink\n";
        let sinks = parse_pactl_sinks(sample);
        assert_eq!(sinks, vec![("Good".to_string(), "Good Sink".to_string())]);
    }
}
