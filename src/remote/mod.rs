//! The network remote (0.4.0, spec 21.2): a server ShellSDR runs the SDR
//! and the DSP; a client ShellSDR's Network source shows and controls it.
//!
//! The seam is the engine's (spec 21.1): the client's run loop drives a
//! [`client::RemoteEngine`] through the same `EngineHandle` as the local
//! engine; its commands go over the wire, and the server answers with the
//! engine's events and the audio (Opus, or 16-bit PCM without libopus).

pub mod cells;
pub mod client;
pub mod opus;
pub mod picture;
pub mod server;
pub mod share;
pub mod viewer;
pub mod wire;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::client::{RemoteEngine, Target, query};
    use super::server::{ServeOptions, Server};
    use crate::engine::{Command, EngineConfig, EngineHandle, Event, Input};
    use crate::vfo::model::Mode;
    use crate::vfo::types::voice::VoiceSettings;

    const RATE: u32 = 48_000;
    const CENTER: u64 = 7_100_000;

    /// A 16-bit I/Q WAV with a carrier `offset` Hz above the center.
    fn baseband(path: &std::path::Path, secs: f64, offset: f64) {
        let n = (f64::from(RATE) * secs) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n * 4).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&RATE.to_le_bytes());
        b.extend_from_slice(&(RATE * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(n * 4).to_le_bytes());
        for i in 0..n {
            let p = std::f64::consts::TAU * offset * f64::from(i) / f64::from(RATE);
            b.extend_from_slice(&((p.cos() * 8000.0) as i16).to_le_bytes());
            b.extend_from_slice(&((p.sin() * 8000.0) as i16).to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    /// Power of `x` at `f` Hz (48 kHz).
    fn at(x: &[f32], f: f64) -> f64 {
        let (mut re, mut im) = (0f64, 0f64);
        for (i, v) in x.iter().enumerate() {
            let p = std::f64::consts::TAU * f * i as f64 / 48_000.0;
            re += f64::from(*v) * p.cos();
            im += f64::from(*v) * p.sin();
        }
        (re * re + im * im).sqrt() / x.len().max(1) as f64
    }

    #[test]
    fn a_client_hears_a_served_file_through_the_server() {
        let dir = std::env::temp_dir().join(format!("shellsdr-remote-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join(format!("t_{CENTER}Hz.wav"));
        baseband(&wav, 3.0, 5_000.0);
        let server = Server::bind(&ServeOptions {
            addr: "127.0.0.1:0".into(),
            token: Some("s3cret".into()),
            files: vec![wav],
        })
        .unwrap();
        let addr = server.local_addr();
        let quit = Arc::new(AtomicBool::new(false));
        let q = Arc::clone(&quit);
        let h = std::thread::spawn(move || server.run(q));

        assert!(
            query(&addr, Some("wrong"), None)
                .unwrap_err()
                .contains("wrong token")
        );
        let q = query(&addr, Some("s3cret"), None).unwrap();
        let input = q
            .inputs
            .iter()
            .find(|i| i.label.starts_with("File: "))
            .unwrap()
            .clone();
        assert_eq!(input.fixed_center_hz, Some(CENTER as f64));
        let caps = q.caps.unwrap().unwrap();
        assert_eq!(caps.sample_rates, vec![f64::from(RATE)]);

        // USB with the dial 4 kHz above the center: the carrier at +5 kHz
        // is a 1 kHz tone.
        let cfg = EngineConfig {
            input: Input::Soapy {
                device_args: String::new(),
            },
            sample_rate: f64::from(RATE),
            decimation: 1,
            center_hz: CENTER as f64,
            vfo_hz: (CENTER + 4_000) as f64,
            vfo_type: crate::vfo::types::VOICE.into(),
            settings: VoiceSettings::for_mode(Mode::Usb).to_settings(),
            audio_target: Default::default(),
            audio_tap: true,
            gains: Vec::new(),
            agc: false,
            antenna: None,
            hw_bandwidth_hz: None,
            fft_enabled: true,
        };
        let target = Target {
            addr: addr.clone(),
            token: Some("s3cret".into()),
            args: input.args.clone(),
            bitrate: 32_000,
        };
        let e = RemoteEngine::start(&target, &cfg).unwrap();
        e.command(Command::SetView(true));
        let (mut audio, mut spectrum, mut view, mut samples) = (Vec::new(), None, false, 0);
        let t0 = Instant::now();
        // until enough came (a loaded machine is slower), at most 15 s
        while t0.elapsed() < Duration::from_secs(15)
            && !(spectrum.is_some() && view && samples > 24_000 && audio.len() > 48_000)
        {
            std::thread::sleep(Duration::from_millis(50));
            audio.extend(e.take_audio());
            for ev in e.poll() {
                match ev {
                    Event::Spectrum(s) => spectrum = Some(s),
                    Event::View(_) => view = true,
                    Event::Stats(st) => samples = st.samples,
                    Event::Failed(m) => panic!("{m}"),
                    _ => {}
                }
            }
        }
        let s = spectrum.expect("a spectrum row");
        assert_eq!(s.len(), super::client::SPECTRUM_BINS as usize);
        let peak = s
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        // +5 kHz of a 48 kHz span, DC in the middle
        let want = (0.5 + 5_000.0 / 48_000.0) * s.len() as f64;
        assert!(
            (peak as f64 - want).abs() < 4.0,
            "peak bin {peak}, want {want}"
        );
        assert!(view, "VFO view frames come when asked for");
        assert!(samples > 24_000, "the server reads the file: {samples}");
        assert!(audio.len() > 24_000, "audio arrives: {}", audio.len());
        let tail = &audio[audio.len() / 2..];
        assert!(
            at(tail, 1_000.0) > 10.0 * at(tail, 2_000.0),
            "a 1 kHz tone: {} vs {}",
            at(tail, 1_000.0),
            at(tail, 2_000.0)
        );
        // the compression changes while connected (0.4.3), the tone stays
        let opus = super::opus::available().is_ok();
        if opus {
            assert_eq!(e.net_audio().as_deref(), Some("Opus 32 kb/s"));
        }
        e.set_net_audio(0).unwrap();
        let t0 = Instant::now();
        while e.net_audio().as_deref() != Some("PCM 768 kb/s")
            && t0.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(e.net_audio().as_deref(), Some("PCM 768 kb/s"));
        let _ = e.take_audio();
        let mut after = Vec::new();
        let t0 = Instant::now();
        while after.len() < 24_000 && t0.elapsed() < Duration::from_secs(15) {
            std::thread::sleep(Duration::from_millis(50));
            after.extend(e.take_audio());
            let _ = e.poll();
        }
        assert!(
            at(&after, 1_000.0) > 10.0 * at(&after, 2_000.0),
            "still the tone after the change"
        );
        let left = Box::new(e).stop();
        assert!(left.is_empty());

        // the server is free for the next client
        let q2 = query(&addr, Some("s3cret"), Some(&input.args)).unwrap();
        assert!(q2.caps.unwrap().is_ok());
        quit.store(true, Ordering::Relaxed);
        h.join().unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_viewer_sees_the_shared_screen_types_into_it_and_hears_it() {
        use super::cells::Grid;
        use super::share::{ShareEvent, ShareServer};
        use super::viewer::{Viewer, WindowReq};
        let share = ShareServer::start("127.0.0.1:0", Some("tk".into())).unwrap();
        let addr = share.local_addr().to_string();
        let req = WindowReq {
            addr: addr.clone(),
            token: Some("tk".into()),
            cols: 40,
            rows: 10,
            overlays: false,
            bins: 0,
            bitrate: 32_000,
            audio_target: Default::default(),
            audio_tap: true,
        };
        let bad = WindowReq {
            token: None,
            ..req.clone()
        };
        assert!(Viewer::open(&bad).err().unwrap().contains("wrong token"));
        // a Network source's query learns it is a window
        let q = query(&addr, Some("tk"), None).unwrap();
        assert!(q.info.window && q.inputs.is_empty());

        let v = Viewer::open(&req).unwrap();
        let wait = |f: &mut dyn FnMut() -> bool| {
            let t0 = Instant::now();
            while !f() && t0.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let mut evs = Vec::new();
        wait(&mut || {
            evs.extend(share.events());
            evs.iter()
                .any(|e| matches!(e, ShareEvent::Connected { .. }))
        });
        assert!(
            evs.iter().any(|e| matches!(
                e,
                ShareEvent::Connected {
                    cols: 40,
                    rows: 10,
                    ..
                }
            )),
            "{evs:?}"
        );
        let connected = evs.clone();
        let evs_pictures = |evs: &[ShareEvent]| {
            evs.iter()
                .any(|e| matches!(e, ShareEvent::Connected { pictures: true, .. }))
        };
        assert!(Viewer::open(&req).err().unwrap().contains("busy"));

        // the screen
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 10));
        buf.set_string(2, 3, "Freq: 7100000", ratatui::style::Style::default());
        let g = Grid::from_buffer(&buf);
        share.send_cells(g.diff(None));
        let mut got = None;
        wait(&mut || {
            got = v.take_frame();
            got.is_some()
        });
        assert_eq!(got.unwrap(), g);
        // input and size
        let key = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ));
        v.input(key.clone());
        v.resize(80, 24);
        let mut evs = Vec::new();
        wait(&mut || {
            evs.extend(
                share
                    .events()
                    .into_iter()
                    .filter(|e| !matches!(e, ShareEvent::Refused { .. })),
            );
            evs.len() >= 2
        });
        assert_eq!(
            evs,
            vec![
                ShareEvent::Input(key),
                ShareEvent::Resize { cols: 80, rows: 24 }
            ]
        );
        // audio: a tone in, a tone out
        let tx = share.audio_sender();
        for b in 0..50 {
            let block: Vec<f32> = (0..960)
                .map(|n| {
                    let t = (b * 960 + n) as f32 / 48_000.0;
                    0.5 * (std::f32::consts::TAU * 1_000.0 * t).sin()
                })
                .collect();
            let _ = tx.try_send(block);
        }
        let mut audio = Vec::new();
        wait(&mut || {
            audio.extend(v.take_audio());
            audio.len() >= 24_000
        });
        assert!(audio.len() >= 24_000, "{}", audio.len());
        assert!(at(&audio[audio.len() / 2..], 1_000.0) > 0.1);
        // the compression changes while open (0.4.3): both sides know
        if super::opus::available().is_ok() {
            assert_eq!(v.audio(), "Opus 32 kb/s");
        }
        v.set_audio(0).unwrap();
        let mut evs = Vec::new();
        wait(&mut || {
            evs.extend(share.events());
            v.audio() == "PCM 768 kb/s" && evs.contains(&ShareEvent::Audio("PCM 768 kb/s".into()))
        });
        assert_eq!(v.audio(), "PCM 768 kb/s");
        assert!(
            evs.contains(&ShareEvent::Audio("PCM 768 kb/s".into())),
            "{evs:?}"
        );
        let _ = v.take_audio();
        for b in 0..50 {
            let block: Vec<f32> = (0..960)
                .map(|n| {
                    let t = (b * 960 + n) as f32 / 48_000.0;
                    0.5 * (std::f32::consts::TAU * 1_000.0 * t).sin()
                })
                .collect();
            let _ = tx.try_send(block);
        }
        let mut audio = Vec::new();
        wait(&mut || {
            audio.extend(v.take_audio());
            audio.len() >= 24_000
        });
        assert!(at(&audio[audio.len() / 2..], 1_000.0) > 0.1, "PCM decodes");
        // overlays and spectrum rows (asked for or not, they are passed)
        let ov = vec![super::wire::Overlay::Waterfall {
            rect: [5, 20, 100, 10],
            center_hz: 7e6,
            span_hz: 2e5,
            db_min: -120.0,
            db_max: -40.0,
            gradient: vec![[0, 0, 0], [255, 255, 0]],
            stream: 0,
            edges_hz: 0.0,
        }];
        share.send_overlays(ov.clone());
        share.send_rows(super::wire::STREAM_CHANNEL, &vec![-80.0f32; 4096], 2048);
        let mut rows = Vec::new();
        wait(&mut || {
            rows.extend(v.take_rows());
            !rows.is_empty() && v.overlays() == ov
        });
        assert_eq!(v.overlays(), ov);
        assert_eq!(rows[0].0, super::wire::STREAM_CHANNEL);
        assert_eq!(rows[0].1.len(), 2048, "at the bins asked for");
        // a picture's pixels (0.4.8): this viewer is new enough, but draws
        // no overlays, so the loop would send it none; passed all the same
        assert!(!evs_pictures(&connected));
        let px: Vec<u8> = (0..640 * 480).map(|i| (i % 256) as u8).collect();
        let pic = std::sync::Arc::new(super::picture::Picture::grey(
            super::picture::SLOT_FAX,
            9,
            640,
            480,
            px,
        ));
        wait(&mut || share.send_picture(std::sync::Arc::clone(&pic)));
        wait(&mut || v.pictures().first().is_some_and(|p| p.key == 9));
        assert_eq!(*v.pictures()[0], *pic);
        // the sharing side disconnects
        share.disconnect();
        wait(&mut || v.closed().is_some());
        assert!(v.closed().is_some());
        let mut evs = Vec::new();
        wait(&mut || {
            evs.extend(share.events());
            evs.iter()
                .any(|e| matches!(e, ShareEvent::Disconnected { .. }))
        });
        assert!(!share.connected());
    }
}
