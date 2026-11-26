//! The UI side of the engine seam (0.4.0, spec 21.1): what to start the
//! engine with, and which [`Command`]s bring a running engine in line with
//! the application state. The same commands go to the local DSP thread or,
//! for the Network source, over the wire to a server.

use crate::app::AppState;
use crate::engine::{Command, EngineConfig, EngineHandle, Input};
use crate::source::model::SourceKind;
use crate::vfo::types::Settings;

/// The active VFO's type and settings as the DSP sees them: only what the
/// processor depends on (DSC: nothing, so its Tools settings never
/// interrupt a call being read), and never the tuning snap, which only
/// affects the UI.
pub fn vfo_type_settings(s: &AppState) -> (String, Settings) {
    let v = s.radio.active();
    let mut settings = match v.type_def() {
        Some(t) => t.dsp_settings(&v.type_settings()),
        None => v.type_settings(),
    };
    settings.remove("snap_hz");
    (v.vfo_type.clone(), settings)
}

/// Where the engine's IQ comes from for the current source (None: nothing
/// to start, e.g. no device or no file loaded).
pub fn engine_input(s: &AppState) -> Option<Input> {
    match s.source.kind {
        SourceKind::SoapySdr => s
            .source
            .selected_device_args()
            .map(|device_args| Input::Soapy { device_args }),
        SourceKind::File => s
            .source
            .file
            .path
            .clone()
            .filter(|_| s.source.file.meta.is_some())
            .map(|path| Input::File {
                path,
                start: s.source.file.position,
            }),
        SourceKind::Network => s.source.network_input(),
    }
}

/// The engine configuration for the current state (None: no input).
pub fn engine_config(s: &AppState) -> Option<EngineConfig> {
    let input = engine_input(s)?;
    Some(EngineConfig {
        input,
        sample_rate: s.source.selected_sample_rate().unwrap_or(2_000_000.0),
        decimation: s.source.decimation(),
        center_hz: s.center_hz() as f64,
        vfo_hz: s.radio.active().frequency_hz as f64,
        vfo_type: s.radio.active().vfo_type.clone(),
        settings: s.radio.active().type_settings(),
        audio_target: crate::engine::audio_target::AudioTarget::from_stored(
            s.radio.active().audio_device.as_deref(),
        ),
        audio_tap: false,
        gains: s.source.gain_settings(),
        agc: s.source.agc_on(),
        antenna: s.source.selected_antenna(),
        hw_bandwidth_hz: s.source.selected_hw_bandwidth(),
        fft_enabled: s.fft_wanted(),
    })
}

/// Start the engine for `cfg`: the local DSP thread, or a server's for the
/// Network source.
pub fn start_engine(cfg: EngineConfig) -> Result<Box<dyn EngineHandle>, String> {
    match &cfg.input {
        Input::Network {
            addr,
            token,
            args,
            bitrate,
        } => {
            let target = crate::remote::client::Target {
                addr: addr.clone(),
                token: token.clone(),
                args: args.clone(),
                bitrate: *bitrate,
            };
            Ok(Box::new(crate::remote::client::RemoteEngine::start(
                &target, &cfg,
            )?))
        }
        _ => Ok(Box::new(crate::engine::start(cfg)?)),
    }
}

/// What the UI last sent to the running engine: a change in the state
/// becomes a command.
#[derive(Debug, Clone, PartialEq)]
pub struct EnginePush {
    center: u64,
    vfo: u64,
    type_settings: (String, Settings),
    gains: Vec<(String, f64)>,
    agc: bool,
    antenna: Option<String>,
    hw_bw: Option<f64>,
    decimation: u32,
    volume: f32,
    fft: bool,
    view: bool,
    file_fast: bool,
    file_hold: bool,
}

impl EnginePush {
    /// What an engine just started from `engine_config(s)` has: the state's
    /// values, and the engine's own defaults for the rest (volume 1, view
    /// off, real-time file playback).
    pub fn started(s: &AppState) -> Self {
        Self {
            center: s.center_hz(),
            vfo: s.radio.active().frequency_hz,
            type_settings: vfo_type_settings(s),
            gains: s.source.gain_settings(),
            agc: s.source.agc_on(),
            antenna: s.source.selected_antenna(),
            hw_bw: s.source.selected_hw_bandwidth(),
            decimation: s.source.decimation(),
            volume: 1.0,
            fft: s.fft_wanted(),
            view: false,
            file_fast: false,
            file_hold: false,
        }
    }

    /// The commands that bring the engine in line with `s`; `file_hold`:
    /// a decoder downstream is behind (file playback at Max waits).
    pub fn commands(&mut self, s: &AppState, file_hold: bool) -> Vec<Command> {
        let mut out = Vec::new();
        let c = s.center_hz();
        if c != self.center {
            out.push(Command::SetCenter(c as f64));
            self.center = c;
        }
        let v = s.radio.active().frequency_hz;
        if v != self.vfo {
            out.push(Command::SetVfo(v as f64));
            self.vfo = v;
        }
        // Type or settings changed (mode, bandwidth, ...): the processor is
        // rebuilt.
        let ts = vfo_type_settings(s);
        if ts != self.type_settings {
            out.push(Command::SetVfoSettings(ts.0.clone(), ts.1.clone()));
            self.type_settings = ts;
        }
        // Source hardware: gains (only the elements that changed), AGC,
        // antenna and analog bandwidth are applied live, like SDR++.
        let gains = s.source.gain_settings();
        if gains != self.gains {
            for (name, db) in &gains {
                let changed = self
                    .gains
                    .iter()
                    .find(|(n, _)| n == name)
                    .is_none_or(|(_, old)| (old - db).abs() > f64::EPSILON);
                if changed {
                    out.push(Command::SetGain(name.clone(), *db));
                }
            }
            self.gains = gains;
        }
        let agc = s.source.agc_on();
        if agc != self.agc {
            out.push(Command::SetAgc(agc));
            self.agc = agc;
        }
        let ant = s.source.selected_antenna();
        if ant != self.antenna {
            if let Some(a) = &ant {
                out.push(Command::SetAntenna(a.clone()));
            }
            self.antenna = ant;
        }
        let hw_bw = s.source.selected_hw_bandwidth();
        if hw_bw != self.hw_bw {
            if let Some(bw) = hw_bw {
                out.push(Command::SetHwBandwidth(bw));
            }
            self.hw_bw = hw_bw;
        }
        let decim = s.source.decimation();
        if decim != self.decimation {
            out.push(Command::SetDecimation(decim));
            self.decimation = decim;
        }
        let vol = s.effective_volume();
        if (vol - self.volume).abs() > f32::EPSILON {
            out.push(Command::SetVolume(vol));
            self.volume = vol;
        }
        // The VFO view's spectra run only while it is on screen; the
        // wideband FFT only when a display wants it (low-fi).
        let view = s.vfo_view_visible || s.remote_shows.1;
        if view != self.view {
            out.push(Command::SetView(view));
            self.view = view;
        }
        let fft = s.fft_wanted();
        if fft != self.fft {
            out.push(Command::SetFft(fft));
            self.fft = fft;
        }
        if s.source.kind == SourceKind::File {
            if s.source.file.fast != self.file_fast {
                out.push(Command::SetFileFast(s.source.file.fast));
                self.file_fast = s.source.file.fast;
            }
            if file_hold != self.file_hold {
                out.push(Command::SetFileHold(file_hold));
                self.file_hold = file_hold;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_started_engine_gets_only_what_differs_from_its_defaults() {
        let mut s = AppState::default();
        let mut p = EnginePush::started(&s);
        let first = p.commands(&s, false);
        // the engine starts at volume 1; the state's volume may differ
        assert!(
            first
                .iter()
                .all(|c| matches!(c, Command::SetVolume(_) | Command::SetView(_))),
            "{first:?}"
        );
        assert!(p.commands(&s, false).is_empty(), "nothing changed");
        let f = s.radio.active().frequency_hz + 1_000;
        s.radio.active_mut_freq(f);
        let got = p.commands(&s, false);
        assert_eq!(
            got,
            vec![Command::SetVfo(s.radio.active().frequency_hz as f64)]
        );
        assert!(p.commands(&s, false).is_empty());
    }

    #[test]
    fn file_controls_are_sent_only_for_the_file_source() {
        let mut s = AppState::default();
        let mut p = EnginePush::started(&s);
        let _ = p.commands(&s, false);
        s.source.file.fast = true;
        assert!(p.commands(&s, true).is_empty(), "not a file source");
        s.source.kind = SourceKind::File;
        let got = p.commands(&s, true);
        assert!(got.contains(&Command::SetFileFast(true)));
        assert!(got.contains(&Command::SetFileHold(true)));
    }

    #[test]
    fn commands_survive_the_wire_format() {
        let mut settings = Settings::new();
        settings.insert("mode".into(), serde_json::json!("USB"));
        for c in [
            Command::SetCenter(7_100_000.0),
            Command::SetVfoSettings("Voice".into(), settings),
            Command::SetGain("IFGR".into(), 40.0),
            Command::SetVolume(0.5),
        ] {
            let j = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<Command>(&j).unwrap(), c);
        }
    }
}
