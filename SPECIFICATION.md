y# ShellSDR Specification

A terminal (TUI) SDR receiver and recorder for Linux, optimized for Raspberry Pi (including Pi 3, 1 GB RAM). Inspired by SDR++ (feature model and DSP ideas) and tuiner (Rust/Ratatui reference), with a multi-VFO, recording-focused design and a strict TDD workflow.

Status: decisions locked from the first review round. Remaining defaults are marked DEFAULT and can be revisited; they are not blockers.

Progress (2026-10): Steps 0a-5 are done and user-confirmed on hardware (RSPduo): crate skeleton, SoapySDR spike, TUI layout/nav, Source module, Master VFO with the full SDR++ DSP chain + real source + cpal audio, themes (external JSON) + persistence, scrollable panel, editable center frequency, the Step 3.7 usability batch (reusable selection popup, Source device dropdown with re-probe, VFO mode dropdown, per-VFO audio output incl. PipeWire-sink routing), persistence (center/VFOs/theme/toggles/volume/selected device + per-device source settings), the Power Spectrum (Step 4), and the Waterfall (Step 5). 291 unit tests, clippy clean. Next: Step 6 (secondary VFOs). See HANDOVER.md for the full orientation and the exact resume point.

Version 0.2.0 (2026-11-21, DONE, AWAITING REVIEW): VFO types, the Work Area, native/baseband recording with minimizing, schema-driven bookmarks v2, the scheduler's Tune only / Record, versioned files with migration from 0.1.x, `lib/shellsdr` in the packages. See section 13 (as built). 658 tests, clippy clean.

## 1. Goals and non-goals

### Goals
- A fast, low-footprint TUI SDR receiver and recorder that runs comfortably on a Raspberry Pi 3 (1 GB RAM).
- Device access through SoapySDR, so any SoapySDR-supported device works. Primary target: SDRplay RSPduo.
- Multiple VFOs (radios) sitting at offsets within one source's band, in a single DSP graph. Each VFO has its own processor (audio demod or, later, a decoder), audio output device, and recorder.
- Per-VFO audio and (Master-only) baseband recording, with a bookmark-driven recording scheduler (one Schedule module per instance) and WAV metadata (RIFF comments).
- Baseband file playback as an alternative source, reading center frequency from the RIFF comment.
- Keyboard-first operation with full mouse support, under a strict "no hidden shortcuts" rule.
- Selectable low-fi mode: power spectrum and waterfall can each be turned off to save CPU.
- Extensible toward decoder plugins (DSC, Navtex, WeFax) modeled as VFO processors.

### Non-goals (v1)
- No RSPduo dual-tuner / master-slave within a single process. Two tuners are used by running two ShellSDR instances, one per tuner (mirrors the user's existing SDR++ workflow).
- No digital-mode decoders shipped in v1 (but the architecture reserves the VFO-processor seam for them).
- No network streaming / server mode.
- No online frequency database / bookmark sync. Bookmarks are local.

## 2. Target platform and performance budget

- OS: Linux (Raspberry Pi OS and desktop Linux).
- Hardware floor: Raspberry Pi 3, 1 GB RAM, quad Cortex-A53.
- Delivered builds (`tools/dev build release <arch>`, `docker/Dockerfile.build`, output `dist/shellsdr-<version>-<arch>.tar.gz`):
  - `x64` (x86_64): oldest supported system Ubuntu 24.04 (also runs on Debian 13+); built natively on `ubuntu:24.04`.
  - `arm64` (aarch64): oldest supported system Debian 13 Trixie / Raspberry Pi OS 13 64-bit; cross-built on `debian:trixie`.
  - 0.2.0: the tarball also carries `lib/shellsdr/` for VFO-type libraries (13.10).
  - No 32-bit armhf build. The packages are compiled only, not run under emulation.
  - Runtime dependencies on the target: `libsoapysdr0.8`, `libasound2t64`, glibc 2.39 or newer, plus the SoapySDR module for the device.
- Performance principles:
  - A small fixed set of long-lived threads: UI thread, DSP thread, plus audio-output (cpal) and recorder-writer worker threads. No thread-per-block.
  - Single DSP graph. VFOs are consumers that sit at an offset in the band; adding/removing/enabling a VFO adds/removes a consumer, it does not spawn a separate graph.
  - FFT runs at a fixed, modest frame rate (DEFAULT 15 fps), decoupled from the sample rate (SDR++ reshaper approach).
  - Low-fi mode disables the FFT branch entirely (not merely hidden) when both spectrum and waterfall are off: zero FFT CPU cost.
  - Each VFO decimates the wideband IQ to its channel rate as early as possible.
  - Disabled VFOs consume no CPU (their channelizer + processor are torn down).
  - Bounded channels with backpressure; spectrum frames drop-oldest when the UI is behind.

## 3. Technology choices

- Language: Rust (edition 2024). Toolchain present on dev machine: cargo 1.99.
- TUI: `ratatui` + `crossterm` with mouse capture enabled.
- Theming: a `Theme` maps semantic UI roles (idle/selected/editing field states, border/border-focused, text/dim/accent, OFF note) and a waterfall gradient (RGB color stops) + spectrum line color to concrete colors. Themes are selectable and persisted. Built-ins: Default, and "Retro terminal" (black / dark-green #005000 / bright-green #00FF00 / yellow #FFFF00), whose waterfall gradient reuses SDR++'s Zalias colormap stops. The renderer references theme roles, never hardcoded colors. Field states: idle = gray/dark-green, selected = white/bright-green, editing = yellow background + black text; the highlighted frequency digit is black-on-white.
- Inter-thread messaging: `crossbeam-channel`.
- SDR device I/O: SoapySDR. Binding choice to be finalized by a throwaway spike (see 3.1 and step 0b). Leaning toward the lower-level `soapysdr` crate because it exposes named gain elements (IFGR/RFGR) and bandwidth lists directly.
- DSP: custom, modeled on SDR++'s block/stream idea, NOT FutureSDR. Single graph with a wideband fan-out (splitter) feeding: the FFT branch and each VFO's channelizer. Behind a trait boundary (`SdrSource`, `Vfo`, `VfoProcessor`, `SpectrumSink`, `Recorder`) so components are unit-testable and a decoder can be slotted in later.
- FFT: `rustfft` (pure Rust, NEON-friendly on aarch64).
- Resampling/filters: hand-rolled DSP ported from SDR++: polyphase rational resampler (`dsp::resampler`, deep stopband), cascaded decimate-by-2 channelizer (`dsp::channelizer`), FIR low-pass (`dsp::fir`), NCO (`dsp::nco`), AGC (`dsp::agc`), DC blocker. (An earlier `rubato` dependency was removed: its linear-interpolation stopband was too shallow versus SDR++.)
- Audio out: `cpal` (per-VFO output device selection). On Linux/PipeWire, individual PipeWire sinks (which the ALSA layer does not expose by name) are listed via `pactl` and opened through the generic `pipewire` PCM with the `PIPEWIRE_NODE` environment variable pinned to the sink's node name (`engine::audio_target::AudioTarget`).
- Config: `serde` + TOML. `--config <name>` (→ `~/.config/ShellSDR/<name>.toml`) or `--config-path <path>` flag; default directory `~/.config/ShellSDR` on Linux.
- WAV/RIFF: custom writer and reader. Metadata via RIFF `LIST`/`INFO` `ICMT` as `key=value` lines. Reader needed for baseband playback (SDR++ has only a writer).

### 3.1 SoapySDR binding decision (LOCKED, verified by step-0b spike)
Decision: use the `soapysdr` crate (0.5.x, which targets SoapySDR 0.8). Not `seify`, not raw `sdrplay_api`.

Verified on the dev machine (SoapySDR 0.8.1, SoapySDRPlay module 0.5.1, RSPduo serial 22340A6E34):
- Enumeration exposes the RSPduo mode variants as distinct devices via the `mode=` arg: `ST` (Single Tuner), `DT` (Dual Tuner), `MA` / `MA8` (Master), and Slave when a master is active. Instance separation and master/slave selection is therefore just choosing the right device args, e.g. `driver=sdrplay,mode=ST` or `mode=MA` / slave. This is richer than SDR++'s hardwired single-tuner path.
- IFGR and RFGR are independently controllable named gain elements (`list_gains` -> `["IFGR","RFGR"]`), ranges `[20,59]` dB and `[0,9]` respectively; overall gain range `[0,48]`. So separate IF/RF gain control needs no raw-API fallback.
- Sample-rate ranges, filter-bandwidth list (0.2/0.3/0.6/1.536/5/6/7/8 MHz), antenna list (Tuner 1 50 ohm / Tuner 1 Hi-Z / Tuner 2 50 ohm), and frequency range are all queryable.
- AGC supported (`has_gain_mode` = true); device settings readable/writable: `rfgain_sel`, `agc_setpoint`, `biasT_ctrl`, `rfnotch_ctrl`, `dabnotch_ctrl`, `iqcorr_ctrl`.

SDR++ is not a reference for the binding (it links `sdrplay_api` directly), only for which controls matter. The device layer will wrap `soapysdr::Device` behind the `SdrSource` trait.

## 4. RSPduo dual-instance behavior (verified)

SDR++'s SDRplay module simply calls `sdrplay_api_GetDevices()` and lists whatever it returns; it always opens in `RspDuoMode_Single_Tuner`, tuner A, one instance (`Max instances = 1`). The "second instance only sees Slave" behavior is produced by the sdrplay_api service, not by SDR++ logic: once the first process selects the RSPduo in a mode, the service constrains what the next process may enumerate/open. SoapySDRPlay sits on the same `sdrplay_api`, so ShellSDR inherits the same behavior for free.

The step-0b spike confirmed SoapySDRPlay surfaces the modes as separate enumerable devices via the `mode=` arg (ST / DT / MA / MA8, and Slave once a master runs). ShellSDR's rule: enumerate exactly what SoapySDR returns and present that, label and all; do not special-case master/slave. A user picks the Master device in instance 1 and the Slave in instance 2 (each with its own `--config`), reproducing the existing SDR++ workflow, and gaining explicit master/slave selection that SDR++ lacked.

## 5. Screen layout

```
+----------------------------------------------------------------------------+
| Global: [Start/Stop] Vol[####----] [x] Mute [Spectrum:ON] [Waterfall:ON]   |
|  Freq: 1 4 5.5 0 0 0 0 0 Hz [Center|Free] Theme Panel:[◂ 32 ▸] [?] Help   |
+------------------+---------------------------------------------------------+
| LEFT PANEL       | RIGHT PANEL                                             |
| (TAB-cycled      |  +---------------------------------------------------+  |
|  modules;        |  | Bookmarks row(s): centered text labels            |  |
|  each            |  +---------------------------------------------------+  |
|  collapsible)    |  | Power spectrum (tunable freq range on X)          |  |
|                  |  | VFO markers overlaid; active VFO framed            |  |
| [-] Source       |  +---------------------------------------------------+  |
| [-] Radio        |  | Waterfall                                         |  |
| [-] Bookmarks    |  +---------------------------------------------------+  |
+------------------+---------------------------------------------------------+
| Status bar: device, sample rate, VFO summary, record state, messages       |
+----------------------------------------------------------------------------+
```

- Left panel width: 32 columns by default (also the minimum: the module rows are laid out for it), up to 96, in steps of 2, set with the Global bar's `Panel:` stepper (6.3). The right side always keeps at least 40 columns: on a narrower terminal the panel shrinks to fit, but the requested width is kept (and persisted as `left_panel_width`), so widening the terminal brings it back.
- Spectrum/Waterfall OFF: area stays visible, drawn black with a centered `OFF` note; the FFT branch is disabled when both are off.
- Bookmark strip: centered text only, no background fill, not drawn inside the spectrum. Row count user-selectable (1..10). Details in section 9.
- The active VFO is indicated by a highlighted frame, like any other active/focused area.
- 0.2.0: the right side becomes the Work Area, tabbed or split with the active VFO's type view (13.5).

## 6. Interaction model

### 6.1 Control visibility invariant (hard rule)
Every action has a visible, focusable, and mouse-clickable control. Keyboard shortcuts are accelerators for those visible controls; no action is reachable only by an unlabeled shortcut. Each review step verifies this for any UI it adds.

### 6.2 Navigation
- `TAB` / `Shift+TAB`: cycle focus across focusable regions: Global Controls bar, then left-panel modules (Source, Radio, Bookmarks, Schedule; a folded module's title shows a summary in brackets: source kind, active VFO, selected bookmark list, scheduler On/Off). Global Controls is a TAB stop so it is operable without a mouse.
- Arrows: move within the focused region/module. Within Global Controls, arrows move between buttons; `ENTER` activates the focused button.
- `ENTER` + arrows: list-selection mode inside a module (devices, VFOs, bookmarks). Arrows move selection, `ENTER` commits, `Esc` cancels.
- Mouse: every actionable element is clickable; spectrum is click-to-tune; scroll wheel adjusts the highlighted frequency digit and steppers. Rules (Step 3.13): left press acts (not release); a click focuses the region, selects the field and activates it like `ENTER` (toggles flip, list fields open their popup, editable fields enter edit mode); a click on a frequency digit edits that digit; popup items apply on click, a click outside a popup dismisses it, the wheel moves its highlight; clicking elsewhere is a focus-out that commits an in-progress edit; the wheel changes a value only on the already-selected control (no scroll hijacking), otherwise it scrolls the left panel; wheel on a frequency digit acts like Up/Down (pending until `ENTER`/focus-out). Only click/wheel/drag reporting is requested from the terminal (no any-motion tracking). Hold `Shift` while dragging to use the terminal's own text selection.
- Click-to-tune (Step 3.16): a left click anywhere in the spectrum plot (trace area and its frequency ruler) or the waterfall tunes the active VFO to the frequency under the clicked column, per the Center/Free mode (6.3). The clicked frequency is snapped to the active VFO's `Snap` interval (SDR++ snaps the same way). The dB label gutter left of the plot is not a target, and a display that is OFF is not clickable. The spectrum, the waterfall and the bookmark strip share one plot geometry: the same `PLOT_LEFT_COLS` gutter (fixed-width dB labels) and the same column-to-frequency mapping (`spectrum::freq_to_col` / `col_to_offset`), so a signal, its trace, the VFO marker and its bookmark label are in the same column. A pending center or VFO frequency edit is committed first (focus-out), but the frequency is read from the frame the user clicked on. One terminal column is `span / plot columns` wide (about 25 kHz for 2 MHz on 80 columns): use the frequency digits or the wheel for finer steps. Not yet: dragging the VFO, the wheel over the plot.
- Collapse: the focused module responds to `+`/`-` and a clickable `[+]`/`[-]` glyph in its header, collapsing to header-only or expanding. Applies to Source, each VFO, Bookmarks. Collapsed state persists.

### 6.3 Global controls
- Start/Stop: start/stop the source stream and all DSP. (`Space` accelerator.)
- Global volume: a master gain applied on top of each VFO's own volume.
- `[x] Mute` (right after `Vol[...]`, its own cursor stop; ENTER, Left/Right or a click toggles it; `m` accelerator): the engine gets volume 0 while muted, the volume level itself is kept. Persisted (`muted` in the TOML).
- `Panel:[◂ 32 ▸]` (second line, after Theme): the left panel width in columns (section 5). A stepper like Volume: Left/Right or the wheel change it while it is selected, a click on `◂` / `▸` narrows / widens it by one step, and `Ctrl+Shift+Left` / `Ctrl+Shift+Right` are its accelerators from any region (focus does not move). Persisted.
- `[?] Help` (last control on the second line; `?` accelerator): opens the key-binding help (6.4).
- Spectrum on/off and Waterfall on/off: independent toggles; OFF = black + `OFF` note, FFT branch disabled when both off.
- Center frequency entry (top panel): sets the SOURCE CENTER frequency (where the SDR is tuned). The VFO's own frequency is edited in the Radio module (section 8).
  - Type digits directly, or select a digit and increment/decrement with Up/Down or scroll wheel.
  - Selected digit highlighted; Left/Right move the selected digit; `z` zeros digits to the right of the cursor.
  - Editing does NOT retune live: while digits are being modified the field shows the editing state. The new center is applied only on `ENTER` (or focus-out); `Esc` cancels.
  - Changing the center frequency preserves every VFO's OFFSET from center (the channels move with the window). A VFO already at offset 0 stays centered.
- Center/Free toggle (`c` accelerator), affects spectrum clicks:
  - Center: clicking in the spectrum re-centers the source on the clicked frequency AND moves the active VFO there; all VFO relative offsets are preserved (see 8.3). Secondary VFOs that would fall outside the new span are pulled back into the visible span.
  - Free: clicking in the spectrum moves only the active VFO's offset; source center unchanged. If the VFO's channel (frequency ± bandwidth/2) would not fit inside the span, the source retunes per SDR++ `tuner::normalTuning` (the same rule as tuning a bookmark, section 9): the VFO lands `span/2 - bw/2 - span/10` from the center, on the side it came from.

### 6.4 Help popup
- `?` (or ENTER/click on `[?] Help`) opens a modal, centered popup listing every key binding, grouped in sections: Global, Navigation, Frequency editing, Spectrum/Waterfall, Bookmarks list, Schedule, Editor forms and prompts, Mouse. Keys in a left column, descriptions (dim) beside them; section headings in `accent`.
- Scrolling when it is taller than the screen: Up/Down, PgUp/PgDn, Home/End, mouse wheel; the bottom border shows `↑/↓ more` while lines are hidden. Esc, ENTER or `?` closes it, a click outside closes it, a click inside is inert. It sits above every other dialog and captures the keyboard (no accelerators leak to the app behind it); `q` still quits (subject to 6.5).
- While a text field captures the keyboard (prompt, editor form, Find), `?` is text, not help.
- The content is a static table in `ui::help` kept beside `ui::run::translate_key`; a test checks that every global accelerator is listed. Any new or changed binding must update it.

### 6.5 Quitting
- `q` quits only while the source is stopped. While reception or file playback runs, `q` (from anywhere, including the help popup and other dialogs) does not quit: a modal dialog "Stop reception before quitting" / "Stop file playback before quitting" with `[OK]` opens instead (ENTER, Esc or a click closes it). Nothing is stopped for the user; Space or the Start/Stop control stops the source, then `q` quits.

## 7. Source module

Two source types, selectable; the module's UI changes to match. Default: SoapySDR.

### 7.1 SoapySDR source
- Device selection: enumerate SoapySDR devices; pick one (list-selection). One tuner per instance (one source). Two instances select the tuners the sdrplay_api service offers (section 4).
- Controls, populated from device capabilities where possible:
  - Sample rate / capture width (e.g. RSP 250 kHz ... 2000 kHz ...): from `getSampleRateRange`, else per-device fallback list.
  - Bandwidth (analog filter): from `listBandwidths` / `getBandwidthRange`.
  - Decimation (software, right after capture, SDR++ `IQFrontEnd` + `PowerDecimator`): None, 2x, 4x, 8x, 16x, 32x, 64x with SDR++'s precomputed multi-stage FIR plans and taps. Everything downstream (spectrum span, VFOs, baseband recording) runs at the effective rate `sample_rate / decimation`, shown next to the ratio. Applied live; persisted globally (`decimation` in the TOML, like SDR++'s `conf["decimation"]`).
  - Gains: IFGR and RFGR as separate named gain elements for RSP devices (exact element names confirmed by the step-0b spike); generic devices show their named elements and/or overall gain.
  - AGC toggle where supported.

### 7.2 Baseband file source
- Select a WAV baseband (IQ) file: ENTER (or a click) on the `File:` row opens a modal file dialog (`ui::file_browser`) in the current file's folder (cursor on it), else the recordings folder. It lists `../`, folders, then `*.wav` files with their size (dot entries hidden), shows the selected file's rate, center and length (or why it is not playable), and has the buttons `[Open] [Up] [Home] [Path…] [WAV only]/[All files] [Cancel]`. Keys: Up/Down/PgUp/PgDn/Home/End move, Enter or Right opens a folder, Enter chooses a file, Left/Backspace goes up, a letter jumps to the next name starting with it, `~` goes home, Tab switches between the list and the buttons, Esc closes. Mouse: a click selects a row, a click on the selected row opens it, the wheel scrolls, a click outside closes. [Path…] opens a typed-path prompt (`~` = home); a folder typed there reopens the dialog in it. An unplayable file keeps the dialog open with the error. Plays I/Q stereo 16-bit PCM (SDR++'s default, read as `/32768` like SDR++) and 32-bit float (SDR++ "Float32 mode"); mono or other formats are rejected with the reason shown in the module.
- Rows: `Kind`, `File: <name>`, an info line (`250 kHz  13.264000 MHz`, or the load error), the position `m:ss.t / m:ss.t` with a progress bar, `[Start]`/`[Stop] [Rewind]`, `Back: [5s] [10s] [30s] [1m]`, `Fwd:  [5s] [10s] [30s] [1m]`. Each button is a click target; on a button row Left/Right selects the button and ENTER presses it. [Start]/[Stop] is the same switch as the global Start/Stop; Rewind goes to the beginning; the skip rows are the backward and forward directions. Skips while stopped move the start position; while playing they seek at once.
- On load: read the center frequency from the RIFF `ICMT` (`center_freq`), else from the file name like SDR++'s file source (the first `[0-9]+Hz`, e.g. `20261104T100029_baseband_13264000Hz.wav`), and tune the center to it (VFOs keep their offsets). While the File source is selected the center is locked to the file's (as SDR++ locks its waterfall center); the VFOs tune freely within the span. The span is the file's sample rate (Source decimation still applies).
- Playback runs in real time in SDR++'s block size (sample rate / 200, 5 ms), feeding the same decimator, spectrum and VFO chain as a device.
- At end-of-file: stop and rewind (DEFAULT; looping may be added later as a visible toggle). A user Stop keeps the position (pause).
- Persisted: `source_kind` and `file_path`; a file that is gone at startup keeps its path and shows the error.

## 8. Radio / VFO module

0.2.0: each VFO gets a `Type:` (only `Voice` at first), settings become per VFO per type, and recording becomes Native / Baseband / both with optional minimizing (13.2 to 13.6).

- At least one VFO always exists: "Master" (fixed name, cannot be removed).
- Additional VFOs can be added/removed, each with a user-given name.
- Every VFO (including Master) has:
  - enabled flag: disabled = channelizer + processor torn down (no CPU), row collapses to header. Master may be disabled but not removed.
  - collapsed flag: header-only display, independent of enabled.
- Per-VFO settings:
  - Processor / mode: audio demod NFM, WFM, AM, DSB, USB, LSB, CW (SDR++ set minus RAW). The mode list is the extension seam for future decoders (DSC/Navtex/WeFax) as additional processor types; a decoder VFO emits text/data to a panel and binds no sound device.
  - Bandwidth: edited in per-mode steps down to a per-mode minimum (`Mode::bandwidth_step` / `Mode::min_bandwidth`). CW: 100 Hz steps, minimum 100 Hz (default 200 Hz; SDR++ `cw.h` allows 50..500 Hz). All other modes: 500 Hz steps, minimum 500 Hz.
  - Snap interval.
  - CW tone (CW only): 250..3000 Hz in 10 Hz steps (SDR++ allows 250..1250). Up to 1250 Hz the CW chain is exactly SDR++'s (3 kHz IF); above it the IF rate is raised to `2.2 * (tone + bw/2)` (`dsp::chain::cw_if_rate`) so the pitch stays below Nyquist. CPU note: a 3000 Hz pitch from 2 MHz costs about 4x the default narrow CW channel filter.
  - Audio output device + audio sample rate (per VFO, via cpal). (Not shown for decoder processors.)
  - Per-VFO volume / mute (global volume is applied on top). Mute is done (Step 8c): a `[ ] Mute VFO` row right after `Audio:` (ENTER, Left/Right, wheel or a click toggles it), persisted as `muted` in the VFO's TOML entry. It silences only the VFO's audio output: recording taps the audio before the volume, so a muted VFO still records. With one audio chain (until Step 6) the active VFO's mute zeroes the engine gain (`AppState::effective_volume`); per-VFO chains will carry their own gain. Per-VFO volume is still to do.
  - Squelch (SDR++-style power squelch).
  - Recorder: Audio, or Baseband (Baseband only on Master).
  - Filename pattern (SDR++-style template, e.g. `$t_$f_$h-$m-$s_$d-$M-$y`) with a VFO-name token.
  - Scheduling is not per VFO: see the Schedule module (section 9b). (Changed in Step 8b after a design review: per-VFO intervals scatter jobs, cannot start/stop the source, and record whatever the VFO happens to be tuned to.)
- Visible controls: a `[+ Add VFO]` action at the bottom of the VFO list; each VFO header shows `[+/-]` collapse, an enable checkbox, and (secondary only) a `[Remove]` action. All focusable and clickable; accelerators provided.

### 8.1 Active VFO
- Exactly one VFO is active, shown with a highlighted frame in the list and marked in the spectrum overlay. The active VFO is the one whose frame is focused in the Radio module.
- Click-to-tune in the spectrum affects the active VFO (Free) or re-centers the source (Center).

### 8.2 Decoder-as-VFO (future-proofing, no decoders in v1)
Superseded by the `VfoType` seam of 0.2.0 (13.2); the text below is the original intent.
The `VfoProcessor` trait is the plugin seam: it consumes a VFO's channelized IQ and produces either audio (demodulators) or decoded output (decoders). Picking the processor type is part of the VFO "mode" selection. Master can be disabled so it does not demodulate when only a decoder VFO (or baseband recording) is wanted.

### 8.3 Keep current distance between VFOs
When a Center-mode click (or an out-of-bounds Free entry) changes the source center, the inter-VFO offsets captured at the moment of the click are preserved; all VFOs shift together with the new center.

### 8.4 WAV metadata (RIFF ICMT, key=value)
Every recorded WAV embeds a `LIST`/`INFO` `ICMT` comment with at least: `center_freq`, `vfo_freq`, `start_time`, `end_time`, `timezone`, `vfo_name`, `mode`, `bandwidth`, `sample_rate`, `rec_type` (audio|baseband), `format_version`. Baseband playback parses `center_freq`. Exact key list is frozen in step 8 and documented alongside the WAV module. 0.2.0: `format_version` 2 (13.6).

## 9. Bookmarks module

0.2.0: bookmarks carry a VFO type and its settings, the editor is built from the type's schema, unknown types are inert, and the file becomes `bookmarks.v2.json` (13.7, 13.9).

- Modeled on SDR++'s Bookmark Manager plugin (`misc_modules/bookmark_manager`), not the Frequency Manager. No import of SDR++ bookmark files (user decision, Step 7).
- Bookmarks live in named lists. Each list can be shown on or hidden from the strip. A fresh store starts with one empty "General" list (SDR++ default).
- A bookmark stores: name (unique within its list), frequency, mode, bandwidth, CW tone, AM AGC (every per-mode setting needed to reproduce the tuning), UTC start/end time (HHMM; 0000/0000 = all day; a window may wrap past midnight), the weekdays it airs (Su..Sa), geo info and notes.
- On air (SDR++ `bookmarkOnline`): off on an unchecked weekday; all day when start = end = 0000; otherwise inside `[start, end)`, wrapping when start > end; never when start = end != 0000. The end minute is exclusive in both cases (SDR++ makes it inclusive in the wrapping case only).
- Strip (above the spectrum, text only, no fill, not inside the spectrum graph): labels centered over their frequency column using the spectrum's plot geometry, so a label sits exactly over its trace column and the VFO marker. Labels are packed into the user-selected 1..10 rows with SDR++'s greedy rule (by frequency, first row without overlap, one blank column between labels); labels that fit nowhere are skipped. The selected bookmark is placed first. Every bookmark in view gets a `┬` tick on the strip's bottom border. Names longer than 20 characters are cut with `…`. Title shows the UTC clock. Theme roles: on air `text`, off air `dim`, the selected bookmark reversed + bold in its on/off-air color. Clicking a label tunes it at once and selects it in the module.
- Left-panel Bookmarks module, one row per control: `List:` (Left/Right cycle, ENTER picker), list buttons `[New] [Rename] [Del] [Hide]/[Show]`, bookmark buttons `[Add] [Edit] [Remove] [Tune]`, `Strip rows:` (Left/Right or wheel 1..10, ENTER steps and wraps), `Sort:` (Frequency/Name), `Find(/):` (case-insensitive filter over name, notes and geo info), a 10-row scrolling bookmark window (kHz frequency + name, on/off-air colors), and a details line for the selected bookmark (mode, bandwidth, schedule).
- Keyboard on the bookmark rows: Up/Down move (Up from the first row leaves to Find; Down past the last wraps to List), Left/Right switch to the previous/next list, Home/End first/last, PgUp/PgDown page, ENTER tunes, Delete removes (with confirmation). `/` anywhere focuses the module and types into Find (ENTER keeps the filter and returns to the rows, Esc clears it).
- Mouse on the bookmark rows: the first click selects, a click on the already-selected row tunes (SDR++'s double-click without a timing window); the wheel scrolls the window without moving the selection.
- [Add] captures the active VFO (frequency, mode, all mode settings) as "New Bookmark" (or "New Bookmark (n)") into the selected list and opens the editor. [Edit] opens the editor on the selected bookmark. Deleting a list or removing a bookmark asks for confirmation (Cancel is the default). New/Rename list ask for a name in a prompt (non-empty, unique).
- Editor form (modal): Name, List (move to another list), Frequency (the shared digit editor), Mode (changing it resets the bandwidth to the mode default), Bandwidth (Hz), CW tone (CW only, 250..3000 Hz), Start/End UTC (HHMM), Days (seven toggles), Geo info, Notes, [Apply] [Cancel]. While the form is open every key is text (no accelerators). Entering a text field selects its content; typing replaces it, arrows/Home/End keep it and move the cursor. Typing on a selected text field starts editing it. Esc reverts the field being edited, a second Esc cancels the form. Apply validates (name, unique within the target list, times, bandwidth, tone) and keeps the form open with the error otherwise. Clicks outside the form are ignored.
- Tuning a bookmark applies its mode and settings to the active VFO, then the frequency: Center mode re-centers the source on it; Free mode moves only the VFO while its channel fits the span, else retunes the source per SDR++ `tuner::normalTuning` (VFO placed `span/2 - bw/2 - span/10` from center, near the top edge when the target is below the span, near the bottom when above). Other VFOs keep their offsets.

## 9b. Schedule module (scheduler: record or tune)

0.2.0: Action becomes Tune only / Record (Native, Baseband or both) and a job may switch the VFO type for its run (13.8, 13.9).

Decided in Step 8b after a three-viewpoint design review (operator UX, architecture/cost, ops/minimalism). A global list of bookmark-driven jobs; the scheduler can start and stop the source to save power on the Pi.

- Left-panel module after Bookmarks, one row per control: a summary line (`REC <job>`, or `Next <day> <time> <job>`, or `Scheduler is off` / `Clock not set: waiting`), `Scheduler: [On]/[Off]` (master switch; Off is drawn in `accent`), `Auto start/stop: [x]`, `Local time: [ ]`, buttons `[Add] [Edit] [Remove] [Off]/[On]` (the last switches the selected job), a 6-row scrolling job window, and two details lines for the selected job (window, days, mode, kHz; then its state: `REC until ...`, `Next ...`, `Waits: manual recording / source stopped / another job`, `Skipped to ...`, `Source failed: retry Ns`, `Off`).
- Job row: `›x 0600-0630 Am! NAVTEX`: selection marker, state (`●` running in `accent`, `x` on, `-` off in `dim`), the window, the action `A`/`B`/`T` (record Audio / record Baseband / Tune only), `m` when the job mutes the VFO, `!` when it overlaps another enabled Record job, and the name. A Tune only job shows only its start: `›x 0600      T  DSC`.
- Job order: always sorted by start time as shown (local time when `Local time` is on), then end time, then the order the jobs were added. Adding or editing a job, switching `Local time` and a change of the UTC offset (DST) re-sort the list; the selection stays on its job.
- Every job is bookmark-driven (user decision; no frequency-only jobs). [Add] opens the editor and at once a picker of all bookmarks (`List: Name (kHz)`, the Bookmarks module's selection highlighted). In a new job, choosing a bookmark fills Start, End and Days from the bookmark's broadcast schedule; the user may override them. Choosing another bookmark while editing a job changes only the bookmark (name and tuning): the job keeps its Start, End and Days. The job's name is the bookmark's name. The job stores the bookmark's list + name and a snapshot of its tuning: the live bookmark's tuning is used when it still exists, the snapshot otherwise (`(bookmark gone)` in the details).
- Editor form (modal): Bookmark `[..]` (ENTER/click opens the picker) with its tuning shown below it, Start and End (HHMM; `0000-0000` = all day; start = end otherwise is rejected), Days (seven toggles, at least one), Repeat (Weekly/Once: a Once job switches itself off after its occurrence), Action (`Record audio` / `Record baseband` / `Tune only`; Baseband only on Master), Mute (`[x] while it runs`), [Apply] [Cancel]. With `Tune only`, End and Mute are disabled (dim, skipped by navigation, clicks ignored; End reads `none: tunes at Start only`) and End is not validated; the previous End comes back when switching to a Record action. A Tune only job is stored with its start minute as window (`0600-0601`). Old schedule files without `action`/`mute` load as Record, unmuted. Same key model as the bookmark editor; every key is text while it is open.
- Times are stored in UTC. With `Local time` checked, the module and the editor show and accept local time (the current UTC offset from the C library); weekdays rotate when the start crosses midnight. A DST change moves a job by an hour in local terms (it stays fixed in UTC, like bookmarks).
- Window rule: SDR++'s time-of-day rule (wrap past midnight, end exclusive), but the weekdays are those of the occurrence's *start* (a Monday 22:00-02:00 job runs until Tuesday 02:00). Bookmarks keep SDR++'s rule (weekday of now). Shared core: `schedule::window`.
- When a job starts it tunes the Master VFO to its bookmark: the source center and the VFO both go to the bookmark frequency, with its mode, bandwidth, CW tone and AM AGC (user decision), and a Record job starts recording as the job's type (the VFO's own recorder type is restored afterwards); a Tune only job only tunes. Name template and folder are the VFO's. A job with Mute mutes the VFO while it runs (the recording is unaffected) and gives the VFO its own mute state back when it ends; back-to-back jobs restore the state from before the first one; a job without Mute leaves the VFO's mute alone.
- Rules (level-triggered: re-evaluated every UI tick, nothing armed, so a restart or crash inside a window resumes in a new file and a clock jump just changes the answer):
  - One Record job runs at a time (one VFO until Step 6). Among open occurrences the earliest start wins, list order breaks ties; the others wait and start when it ends if still inside their window.
  - Auto start/stop (default on): a job that finds the source stopped starts it; the scheduler stops only a source it started, once no job needs it (back-to-back jobs keep it running). A source the user started is never stopped. With Auto off, a due job waits for the user to press Start.
  - The user wins: pressing Stop on the source or the recorder during a job skips that occurrence until its window ends. A manual recording is never interrupted; a due job waits for it to end. A user retune during a job is not undone.
  - Master switch Off: nothing fires (e.g. while listening). Switching it off during a job stops the recording, skips that occurrence, and leaves the source running for the user.
  - Switching it On again restores the current job (0.3.0, user request 2026-11-24): every skipped occurrence is forgotten, so a Record job whose window is open starts again at once (in a new file), also one the user had stopped; when no Record job is open, the Tune only job that fired last (within a week; the last in list order on a tie) is applied again (type, settings, frequency) and, with Auto start/stop, a stopped source is started. It happens once, at the switch: retuning afterwards is left alone. A Tune job is not re-applied over a running recording. Activating a schedule list does the same (19.8). The Report logs `Resumed <job> (Tune only, from <time>)` or the Record job's start.
  - Source start failure (the engine stops while the job still records): retried every 10 s while the window is open. A recording write error ends the occurrence (no retry loop).
  - Clock sanity: nothing fires before 2025-01-01 (a Pi without RTC before NTP sync).
  - Tune only jobs have no end (user decision): a Tune job fires once, in its start minute, then holds nothing (no slot, no overlap flag, no source ownership), so a Record job may start in the same minute (it starts after the tune). With Auto start/stop it starts a stopped source and leaves it running for the user; without, it only tunes. It never retunes a running recording (a job's or the user's): then that occurrence is missed, as is one whose minute passed without a tick. Several Tune jobs in one minute: the last in list order tunes. A Once Tune job switches itself off after firing. Its status is always `Next ...`. (Changed after review: Tune jobs used to hold their window like Record jobs.)
- Persistence: per instance (two RSPduo instances must not both record a job), `<config stem>.schedule.v3.json` (0.3.0, with lists: 19.8; `.v2.json` in 0.2.x, 13.9) next to the instance TOML, saved atomically at every change (master switch, Auto, Local time, jobs, a Once job switching off). Skipped occurrences are not persisted.
- The status bar marks a scheduled recording `SCHED REC <file>`.
- Deferred: `--headless` (run the schedule without a TUI, for systemd on a Pi), jobs on secondary VFOs and several simultaneous jobs (Step 6), a `[Schedule]` button in the bookmark editor, a free-disk-space check before a job starts, power measurements (Step 10).

## 10. Configuration and persistence

0.2.0: `min_version` in every file, migration from 0.1.0, VFO -> Type -> settings in the profile TOML, Work Area layout persisted (13.4, 13.9).

- One TOML file per instance. `--config <name>` selects `~/.config/ShellSDR/<name>.toml` (schedule `<name>.schedule.v2.json` beside it, 0.2.0); `--config-path <path>` takes an explicit file path; default `~/.config/ShellSDR/default.toml`. Two instances use different `--config` files for independent bookmarks/settings.
- Persistence model matches SDR++: settings persist from the last-closed instance.
- Persisted: source type + settings, all VFOs (incl. enabled/collapsed/active), bookmark view (selected list, strip rows, sort), UI toggles (spectrum/waterfall, center/free), collapse states, global volume, last frequency.
- Bookmarks themselves are NOT per instance: one JSON file shared by all instances, `<config dir>/ShellSDR/bookmarks.v2.json` (0.2.0; 13.9) (independent of `--config`), saved immediately on every change (atomic write: temp file + rename). Each instance checks the file's modification time once a second and reloads it when another instance saved it (keeping the selected list and bookmark by name).
- `--test` mock mode (deterministic synthetic IQ) for running and testing without hardware.

## 11. Architecture

### 11.1 Threads
- UI thread: ratatui draw loop, keyboard + mouse input, UI state. Never blocks on DSP.
- DSP thread: owns the source and the single processing graph (fan-out -> FFT branch + per-VFO channelizers -> processors). Never blocks on UI.
- Channels: FFT frames DSP->UI (bounded, drop-oldest); commands UI->DSP; cpal callback threads per active audio VFO; recorder writer threads.
- Runtime-adjustable params (tune, squelch, mode, per-VFO + global volume, deemphasis) via shared atomics read per-sample, avoiding graph rebuilds. Structural changes (sample rate, decimation, add/remove/enable VFO) rebuild only the affected consumer(s).

### 11.2 DSP graph (single graph, SDR++ model)
`source -> preproc (optional decim, DC block) -> splitter`, where the splitter feeds:
- FFT branch (reshaper throttle -> rustfft -> dB magnitude -> spectrum channel), disabled in low-fi.
- Master baseband recorder tap (raw wideband IQ) when active.
- Each enabled VFO: NCO frequency-translate by the VFO offset -> rational resample to channel rate -> bandwidth FIR -> `VfoProcessor` (demod/decoder) -> AF chain (resample to audio rate, HPF, deemphasis) -> cpal sink and/or recorder.

### 11.3 Testability (TDD)
- Pure logic unit-tested directly: frequency digit editor (incl. pending-edit + apply-on-enter + out-of-bounds recenter), VFO offset / keep-distance math, center/free retune math, filename templating, RIFF ICMT encode/decode, scheduler time logic, bookmark model, config serialization.
- DSP blocks tested with synthetic signals and known-output assertions (NCO, decimator, FM/AM/SSB/CW demod).
- Mock source enables end-to-end tests without hardware: spectrum-frame production, record to temp WAV then re-read and assert (including ICMT round-trip).
- UI as pure state + render function tested via ratatui `TestBackend` buffer assertions; input handlers tested as pure state transitions.

## 12. Implementation plan and review checklist

Strict TDD: for each step, write failing tests first, implement to green, refactor. Each step ends with the app running and the new feature demonstrably working, and MUST be reviewed and confirmed by the user before starting the next step. "No hidden shortcuts" (6.1) is checked on every UI-bearing step.

Legend: [ ] not started, [~] in progress, [x] done + user-confirmed.

- [x] Step 0a: Repo + crate skeleton. DONE + confirmed. `cargo` project (edition 2024, `shellsdr`), modules `cli` + `config`, `--config`/`--test` parsing, config path resolution + TOML round-trip, 8 unit tests green, clippy clean, save-triggered `cargo test`+`clippy` hook added.
- [x] Step 0b: SoapySDR spike (throwaway). DONE. Verified on RSPduo via the `soapysdr` crate against SoapySDR 0.8.1: IFGR/RFGR are separate named gain elements, sample-rate/bandwidth/antenna ranges queryable, mode variants (ST/DT/MA/MA8) enumerable via `mode=` args, AGC + settings available. Binding locked to `soapysdr` crate (spec 3.1). Spike deleted.
- [x] Step 1: TUI layout + navigation. DONE + confirmed. Seams implemented test-first: `ui::nav` focus/collapse state machine (TAB/Shift+TAB cycle Global→Source→Radio→Bookmarks, +/- collapse focused module); `ui::layout` pure region split (global bar / left panel / bookmarks strip / spectrum / waterfall / status bar, bookmark-row-count driven); `ui::render` draws all controls with the focused region framed and OFF notes on disabled displays (TestBackend assertions); `app::AppState` holds nav + toggles (spectrum/waterfall/center-free/run); `ui::run` event loop with keyboard + mouse capture, testable key translation. 31 tests, clippy clean, fmt.
- [x] Step 2: Source module. DONE + confirmed. `source` module added: `SdrSource` trait seam (capabilities, set rate/freq/gain, start/stop with `IqSink`), `MockSource` with RSPduo-like caps from the spike + deterministic tone streaming, `source::model` selection state (SoapySDR/File kind, capability-to-UI mapping exposing IFGR/RFGR as separate controls, sample-rate/bandwidth/antenna/AGC options). Source module renders real contents; default SoapySDR; Left/Right toggles kind when Source focused. 43 tests, clippy clean. Per option (b): real SoapySDR streaming thread deferred to step 3.
- [x] Step 3: Master VFO. DONE + CONFIRMED (works end-to-end on hardware). PIECE A (hardware-free), STEP 3.5 (theme system + field navigation), and PIECE B (hardware bring-up) all complete and user-confirmed. Piece A implemented test-first: `freq` digit editor (cursor, inc/dec by power of ten, zero-right, type-digit, pending-until-commit, out-of-bounds flag); `vfo::model` (Mode NFM/WFM/AM/DSB/USB/LSB/CW with defaults + IF rates, Vfo with enable/collapse/CW-tone, snap/within-band math); `vfo::radio` RadioModule (Master VFO, active selection, mode cycling, frequency commit); `dsp::nco` (NCO translate + decimator, verified tone-to-DC) and `dsp::demod` (FM/AM/SSB/DSB/CW, verified by tone recovery on synthetic signals); AppState holds radio; Radio module renders frequency (highlighted digit + pending-edit color), mode/bandwidth/snap/CW-tone/audio.
- [x] Step 3.5: Theme system + Radio field navigation. DONE + CONFIRMED (theme works, persists; accepted). `ui::theme` with role colors + waterfall gradient sampler (Default + Retro-terminal). Retro UI greens lightened for readability (#30C030 idle / #66FF66 text / #FFFF33 accent) while the waterfall keeps authentic SDR++ Zalias stops. Theme applied across ALL regions (global bar, borders, Source/Radio/Bookmarks, spectrum/waterfall OFF, status bar) via theme roles, verified by tests (status text + borders green under retro). `ThemeKind` cycle via `t` + visible "Theme:[..]" control. Radio field cursor (`RadioField`): Up/Down move field (idle gray / selected white), ENTER edit (yellow-bg/black), in-field arrows, ENTER commit+exit, Esc cancel; freq digit highlight black-on-white only while editing; CW-tone field only in CW. 101 tests, clippy clean.
- [x] Step 3.6a: External JSON themes. DONE + confirmed (themes load and switch; user verified). Themes are JSON files (`name` + hex role colors + variable-length `waterfall` hex array). Search-path loader (env `SHELLSDR_RESOURCE_DIR`, user `~/.config/ShellSDR/themes`, dev `assets/themes`, system dirs) with embedded `include_str!` fallback; disk overrides built-ins by name. Shipped `assets/themes/{default,retro}.json`. 113 tests, clippy clean.
- [x] Step 3.6b: Scrollable left panel. DONE + CONFIRMED (scrolls to the expected field, scrollbar shows). NOTE: the scrollbar is display-only; dragging it with the mouse is not yet wired (deferred to the mouse-usability item). Left panel is a vertical scroll viewport: `ui::scroll` (pure geometry: content height, offset clamp, needs-scrollbar, ensure-visible, page step) + `ui::panel` (per-module natural heights, focused-field virtual span). Modules render into an off-screen buffer at natural height; the visible slice is blitted; a ratatui `Scrollbar` shows when content overflows. Focus changes (TAB) and field navigation auto-scroll the focused field into view (field granularity, so a module taller than the viewport still scrolls its fields in); PgUp/PgDown + mouse wheel scroll; resize reconciled each frame. Content no longer clips behind frames (modules are 1-line-per-field, no wrap; File transport restacked to fit the 32-col panel). Superseded fixed-split `left_panel_rows` removed. 127 tests, clippy clean.
- [x] Step 3.6c: Theme persistence. DONE + CONFIRMED (persisting works). `Config` gains a `theme: Option<String>` (the last-used theme name); `config::{load_or_default, save}` added (save creates parent dirs). Main restores the saved theme on startup via `AppState::select_theme` and persists `current_theme_name()` on exit. Verified end-to-end: selecting a theme and quitting writes it to the `--config` TOML, and relaunching restores it. 134 tests, clippy clean.
- [x] Step 3 Piece B1: DSP chain (hardware-free). DONE + CONFIRMED. `dsp::chain::VfoChain` assembles the per-VFO receive chain: wideband IQ -> NCO translate -> filtered anti-alias `dsp::channelizer` (cascaded decimate-by-2 FIR to the IF rate) -> channel low-pass FIR (cutoff = bandwidth/2) -> demod -> audio AGC (AM/SSB/CW) -> audio-band FIR -> ported SDR++ polyphase `dsp::resampler` to 48 kHz -> clamp. NOTE: the original B1 used a `dsp::plan` integer-decimation planner and a `rubato` FastFixedIn resampler; both were replaced during Step 3.8 (the planner by the filtered channelizer, rubato by the SDR++ polyphase resampler for a deep stopband) and `rubato` was removed as a dependency. Verified end-to-end: a narrow-FM signal 200 kHz off center is recovered as a 48 kHz tone that dominates. soapysdr + cpal build/link on this machine.
- [x] Step 3.6d: Editable center frequency (top panel). DONE + CONFIRMED (works). The Global Controls bar is now a navigable focus region (`ui::global::GlobalField`: Start/Stop, Volume, Spectrum, Waterfall, Frequency, Center/Free, Theme) with the same idle/selected/editing model as the VFO. The center frequency uses the shared digit editor (highlighted digit while editing); ENTER edits/commits, Esc cancels. Committing the center reprojects all VFOs preserving their offset from center (`RadioModule::reproject_to_center`). Keyboard: Up/Down move the global cursor (or change the digit while editing Frequency), Left/Right act in-field, ENTER/Esc, 0-9 type, z zero-right. 150 tests, clippy clean.
- [x] Step 3 Piece B2: Real source + audio. DONE + CONFIRMED (works across modes). Carrier AGC: `AmAgcMode` (Off/Audio/Carrier, default Audio) per VFO, selectable via an AM-only "AM AGC" field, applied on the complex IF before detection (SDR++ `process_complex`). Volume applied in the cpal callback from a shared atomic. Real `soapysdr` source + DSP thread + cpal output live-verified on the RSPduo, including running a second instance (Slave).
- [~] Step 3.7: Usability batch (from hardware review). IN PROGRESS. Reusable selection popup (`ui::popup::ListPopup` + centered overlay) built and wired to the VFO Mode picker, the Source device/sample-rate/bandwidth pickers, and the per-VFO audio-output picker. Native-driver print corruption fixed (SoapySDR `log` no-op handler + scoped stdout/stderr fd silencer around device open/stream). Status bar now shows the real device / run state / mode.
  - [x] AM Carrier AGC (Off/Audio/Carrier, default Audio, per-VFO, selectable).
  - [x] Volume bug fixed (applied in audio callback).
  - [x] Persistence: last center frequency, full VFO settings (freq/mode/bandwidth/snap/CW tone/AM AGC/audio device/enabled/collapsed via `VfoConfig`), selected source device (args, re-resolved on startup) plus per-device Source settings (sample rate, bandwidth, antenna, named gains, AGC) stored in a `source_devices` map keyed by device args (by value/name so they survive capability reordering, SDR++-style; other devices' entries preserved across saves), Center/Free, spectrum/waterfall on/off, volume. Config extended; `AppState::to_config`/`apply_config`; verified via config round-trip + apply/restore tests. TODO: persist the spectrum dB window.
  - [x] Source device dropdown: field-nav Source module (Kind/Device/SampleRate/Bandwidth/gains/AGC), re-probe-on-select (SDR++ open-probe-close pattern), popups for device/rate/bandwidth, in-place gain/AGC, device persistence.
  - [x] VFO mode dropdown (reusable popup; Left/Right quick-cycle, ENTER popup).
  - [x] Per-VFO audio output device selection: `Vfo.audio_device` (persisted), cpal + PipeWire-sink enumeration (`AudioTarget` Default/CpalDevice/PipeWireSink; PipeWire sinks routed via the `pipewire` PCM + `PIPEWIRE_NODE`), picker on the Audio field (Left/Right quick-cycle, ENTER popup), engine restart on live change.
  - [~] Mouse usability: click-to-focus/edit with hit-testing DONE in Step 3.13 (awaiting review). Click-to-tune in the spectrum/waterfall DONE in Step 3.16. Still open: drag the left-panel scrollbar; mouse-draggable dB slider; drag the VFO / wheel over the plot.
- [x] Step 3.8: Port SDR++'s full DSP chain + fix AM clipping/chirp. DONE + CONFIRMED by user (audio listenable across modes; AM with Carrier AGC very similar to SDR++). Fixes: filtered `dsp::channelizer` (cascaded /2 FIR, no aliasing), ported SDR++ polyphase `dsp::resampler` (deep stopband), audio-AGC headroom (set point 0.3 / ceiling 0.9), exact SDR++ per-mode IF rates. Verified offline via `examples/am_chain_dump` + FFT/PSD (0% clip, ~5 kHz band, -60 dB stopband matching SDR++) then confirmed on hardware. Binary+lib split added.
- [~] Step 3.9: Full demodulator audit against SDR++ (triggered by unintelligible USB, A/B `wav/2026-11-04-*`). DONE, AWAITING HARDWARE RE-LISTEN. SSB/DSB rewritten as SDR++ `dsp::demod::SSB` (channel extracted centered at carrier +/- bw/2 per the USB REF_LOWER / LSB REF_UPPER VFO reference, symmetric channel filter selects the sideband, `FrequencyXlator` shift back by +/- bw/2, `Re()`, AGC setPoint 1.0 / maxOut 10, no post-demod filter). CW rewritten as SDR++ `dsp::demod::CW` (complex shift by +tone, `Re()`, AGC 100/5), tone now taken from the VFO (default 800, clamp 250..1250) and pushed live (`Command::SetCwTone`). NFM/WFM deviation = bandwidth/2; NFM default-on audio low-pass bw/2; WFM 15 kHz/4 kHz audio low-pass + 50 us de-emphasis at 48 kHz (`dsp::deemphasis`); DSB default bandwidth 4600. AM unchanged (already matched SDR++'s structure; keeps the confirmed 0.3 headroom AGC). Verified by new sideband-rejection / full-passband / pitch / deviation / de-emphasis tests and `examples/ssb_chain_dump` (37 dB image rejection, 0.8 dB ripple). 257 tests, clippy clean.
- [~] Step 3.10: Source hardware settings actually reach the device (triggered by the Fix01 A/B: ShellSDR's speech band is flat/noise-like with an ~8 dB higher relative floor, i.e. an RF SNR gap, not a demod gap). DONE, AWAITING HARDWARE RE-LISTEN. Gains, hardware AGC, antenna and analog bandwidth are now applied at Start AND live (`Command::SetGain/SetAgc/SetAntenna/SetHwBandwidth`; previously gains/AGC applied only at Start and antenna/bandwidth never). Changing the source device or sample rate while running restarts the engine. New Antenna field in the Source module (shown when the device has more than one port, SDR++-style; popup + Left/Right), persisted. Gain defaults for a device with no saved settings follow SDR++ (range minimum; for RSP reductions = maximum gain) instead of the range midpoint; gain adjust snaps to whole values; IFGR/RFGR labeled "(red.)" as they are reductions. 261 tests, clippy clean.
- [~] Step 3.11: Clip-before-volume fix (triggered by `*-Fix02.wav`: ShellSDR waveform rectangular at ~0.45, 14.4% of samples on the rail vs SDR++ 1.1%). DONE, AWAITING HARDWARE RE-LISTEN. `VfoChain::process` no longer clamps (SDR++ does not; its AGC overshoots 1.0); the full-scale limit now happens after the volume in `engine::audio::output_sample`. `examples/ssb_chain_dump` now reports peak and the clip-free volume (peaks ~2.2x full scale on loud SSB, so volume <= ~0.46 never clips); with the distortion gone it measures 66 dB image rejection, 0.0 dB ripple. 263 tests, clippy clean.
- [~] Step 3.12: Identical-input A/B against SDR++ + channelizer block-phase fix. DONE, AWAITING HARDWARE RE-LISTEN. New `examples/iq_wav_demod` runs an SDR++ baseband WAV through the real `VfoChain` (SDR++ block size sr/200). On `wav/20261104T100029_baseband_13264000Hz.wav` it exposed a real bug: `Decim2` restarted its decimation phase at every block, so blocks whose length is not a multiple of the total decimation emitted extra samples (157 instead of 156.25 per 1250-sample block: +0.48% time base, a phase slip every block, also in the live engine since SoapySDR block sizes vary). Fixed by carrying the phase; new tests require the channelizer and the whole chain to be independent of block boundaries. After the fix, with bandwidth matched to SDR++'s (2300 Hz), ShellSDR vs SDR++ on the same baseband: median phase-invariant similarity 0.999 (10th pct 0.997), AGC level tracking within -0.3..+0.1 dB, all bands within ~1 dB, frequency offset 0.000 Hz. Analysis scripts saved in `tools/analysis/`. 266 tests, clippy clean.
- [~] Step 3.13: Mouse control (re-prioritized ahead of Step 6 for usability). DONE, AWAITING REVIEW. `render_with_hits` records a `ui::mouse::HitMap` while drawing (ratatui is immediate-mode, so the last drawn frame is the source of truth; no duplicated layout math): Global bar controls per span (each volume segment and each center-frequency digit separately), left-panel module header / `[+]`/`[-]` glyph / body, Source and Radio field rows, each VFO frequency digit, and popup frame + items (with their real indices when the list is windowed). Left-panel rects are mapped through the scroll offset and clipped to the viewport. `ui::mouse::apply_mouse` implements the rules in 6.2 by calling the existing keyboard actions, so mouse and keyboard cannot diverge. The terminal is asked for click/wheel/drag reporting only (DECSET 1000/1002/1006/1015, no 1003 any-motion), the event loop drains all pending input per redraw, and an applied mouse event forces a redraw so the next click hits a fresh map. 287 tests, clippy clean.
- [~] Step 3.14: Small polish batch. DONE, AWAITING REVIEW. (1) CW bandwidth now steps by 100 Hz with a 100 Hz minimum (was 500 Hz steps and a 500 Hz floor, so the 200 Hz CW default jumped up to 500 Hz on the first decrement); step and minimum are per-mode (`Mode::bandwidth_step`, `Mode::min_bandwidth`), other modes unchanged at 500 Hz. The narrower CW channel filter costs more taps (about 3,000 at 100 Hz vs about 1,500 at 200 Hz, at the ~3.9 kHz CW channel rate): check CPU in Step 10. (2) Status bar: the run state is drawn in the theme `text` color when Running and `dim` when Stopped; the rest of the status bar stays `dim`. 291 tests, clippy clean.
- [~] Step 4: Power spectrum (RAISED PRIORITY). DONE, AWAITING HARDWARE REVIEW. `dsp::fft::SpectrumAnalyzer` ports SDR++'s exact recipe (reshaper throttle keep/skip, Nuttall window × alternating ±1 for a DC-centered time-domain fftshift, `rustfft` forward, `dB = 10·log10(|X|²/N²)`, one-pole IIR time average). Engine taps the wideband IQ before per-VFO translation, feeds the analyzer only when a display wants it (low-fi: the FFT is skipped entirely when both spectrum and waterfall are off) and publishes the latest frame via a drop-oldest shared slot. `ui::spectrum` renders a Braille dB trace (ratatui `Chart`) with a dB Y-axis, a center-anchored absolute-frequency ruler (tuned center labeled at the middle column, nice ±1/2/5·10^k offset ticks either side, auto-scaled Hz/kHz/MHz units), a center line, and the active-VFO marker; the trace is themed from the top stop of the JSON `waterfall` gradient. The dB window auto-ranges by default (slowly follows recent frames' min/max with 5 dB headroom, SDR++-style) and is manually adjustable (ceiling `,`/`.`, floor `<`/`>`, `a` toggles auto), mirroring SDR++'s Max/Min vertical sliders; the current window + mode is shown in the Spectrum title. The same `DbRange` will drive the waterfall color mapping. Defaults: FFT 4096, 20 fps, avg α 0.5. Mapping helpers (peak-decimate, freq↔col, ruler labels, DbRange) unit-tested; analyzer verified by synthetic tones (DC centered, ±sr/4 peaks, ~20 frames/s). 233 tests, clippy clean. Click-to-tune and a mouse-draggable dB slider deferred to the mouse-usability item. TODO(persistence): the spectrum dB window is not yet persisted (source device settings now are).
- [~] Step 5: Waterfall (RAISED PRIORITY). DONE, AWAITING HARDWARE REVIEW. `ui::waterfall` adds a bounded newest-first `WaterfallHistory` (ring of raw dB frames, decimated at render time so resize is handled) and a `Waterfall` widget that stacks frames newest-at-top, two time-steps per character row via the upper-half-block glyph ("▀", fg = newer, bg = older) for double vertical resolution. Columns reuse the spectrum's `peak_decimate` so the waterfall lines up with the trace above it; color comes from `db_to_color` = the shared `DbRange` normalization into the theme's full JSON `waterfall` gradient (`Theme::waterfall_color`), so spectrum and waterfall share one palette. The engine's FFT frames feed both the spectrum and the history; waterfall on/off keeps the area visible (black + `OFF`) and shares the low-fi FFT gating with the spectrum (FFT skipped only when both are off). History is cleared when the engine stops. Pure history + color mapping unit-tested (newest-first + capacity, gradient-ends mapping, NaN/out-of-range clamp, newest-row-on-top render). 245 tests, clippy clean.
- [~] Step 3.15: Global Mute + Help popup. DONE, AWAITING REVIEW. `GlobalField::Mute` (after Volume) and `GlobalField::Help` (after Theme); `AppState::muted` + `effective_volume()` (what the engine gets), persisted as `muted`; `m` accelerator (the earlier spec's `d` was never implemented). `ui::help` (sections table, `HelpView` scroll model reconciled with the screen size each frame, centered renderer) per 6.4; `?` accelerator, modal keyboard + mouse handling. 504 tests, clippy clean.
- [~] Step 3.16: Click-to-tune in the spectrum and waterfall (6.2, 6.3). DONE, AWAITING REVIEW. `HitTarget::Plot { x, cols }` recorded over the spectrum plot (incl. its ruler) and the waterfall whenever the display is ON; `spectrum::col_to_offset` is the exact inverse of `freq_to_col` (middle column = DC). `AppState::click_tune(hz)` snaps to the active VFO's snap interval and reuses `tune_active_to` (Center: re-center the source, other VFOs keep their offsets; Free: move the VFO, SDR++ normal tuning when its channel leaves the span). Alignment fixes found on the way: the waterfall now draws in the spectrum's plot columns (it used the full width, so it was stretched against the trace and the bookmark strip), and the spectrum's dB labels are fixed-width so ratatui's Chart always keeps the `PLOT_LEFT_COLS` gutter (3-character labels like `-70` had shifted the trace one column left of the markers). Verified live in tmux with SGR mouse clicks. 514 tests, clippy clean.
- [~] Step 3.17: Resizable left panel (5, 6.3). DONE, AWAITING REVIEW. `GlobalField::Panel` stepper `Panel:[◂ N ▸]` between Theme and Help (`HitTarget::PanelStep` for the arrows); `Ctrl+Shift+Left/Right` accelerators (`Action::PanelNarrower/PanelWider`). `ui::layout::left_panel_width` clamps the requested width to 32..96 and to the screen (right side keeps 40 columns); `compute_layout` takes the requested width. `AppState::resize_left_panel` steps from the width actually shown (`screen_width` recorded each frame), so a narrow screen never needs dead key presses. Persisted as `left_panel_width` (clamped on load; old configs get 32). Verified live in tmux (Ctrl+Shift+Right x5 -> 42, saved and restored). Note: the Global bar's second line is now 80 columns with borders under the Default theme (exactly fits an 80-column terminal); a longer theme name such as `Retro terminal` pushes `[?] Help` off the edge there (its `?` accelerator still works). 520 tests, clippy clean.
- [ ] Step 6: Secondary VFOs. Add/remove via visible `[+ Add VFO]`/`[Remove]`, naming, active-VFO frame, keep-distance retune (center/free), per-VFO enable/collapse. Review: VFO collection ops, offset preservation.
- [~] Step 7: Bookmarks (done before Step 6 at the user's request). DONE, AWAITING REVIEW. Implemented per section 9. `bookmarks::model` (Bookmark/BookmarkList/BookmarkStore, UTC on-air logic ported from SDR++ `bookmarkOnline`, `UtcNow` from the system clock without a date crate, JSON store with atomic save, `StoreFile` mtime tracking for multi-instance reload); `bookmarks::manager` (lists, field/button cursors, sort, find filter, selection, scroll window, view settings; dialogs and tuning returned as `Request`s); `bookmarks::editor` (the modal form); `ui::text_input` (shared single-line input: char cursor, digits-only/limit, select-all-on-focus); `ui::prompt` (name prompt); `ui::bookmark_strip` (SDR++ row packing + render, ticks, hits); `ui::bookmark_view` (module body + editor render); `vfo::model::normal_tuning_center` (SDR++ normalTuning) and `RadioModule::apply_settings`. Keyboard: a text-capture mode (`AppState::text_capture`, `run::translate_text_key`) so letters typed into names/filters are text, not accelerators. Mouse: rows, buttons, strip labels, prompt and every editor field/digit/weekday/button are click targets. 391 tests, clippy clean.
- [~] Step 7.5: Source decimation (requested before recording, to save baseband bandwidth). DONE, AWAITING HARDWARE REVIEW. `dsp::power_decimator` ports SDR++'s `PowerDecimator`/`DecimatingFIR` with its 2x..64x plans and the verbatim taps (`dsp::decim_taps`, generated from `decim/taps/*.h`); block-boundary invariant, exact `len/ratio` output count, >60 dB alias rejection. Source module `Decimation:` field (popup, Left/Right, wheel, click) shows the effective rate. Engine decimates right after the stream read and feeds the FFT and the VFO chain at the effective rate; `Command::SetDecimation` rebuilds decimator, analyzer and chain live. 401 tests, clippy clean.
- [~] Step 8a: Per-VFO recording (Audio, Master Baseband). DONE, AWAITING HARDWARE REVIEW. `record::wav` (16-bit PCM writer, SDR++ Int16 scaling with saturation; ICMT written at close, after `data`, so `end_time` is real; reader for playback/tests), `record::naming` (SDR++ template tokens + `$n`, UTC clock, unique names, part names), `record::recorder` (writer thread fed by a bounded channel, never blocks the DSP thread, counts drops; splits at the 4 GiB RIFF limit and on a sample-rate change into `_partN.wav`). Taps as in SDR++: Baseband = the decimated IQ, Audio = the VFO's audio before the volume. Radio module rows `Rec:` (Audio/Baseband, Baseband only on Master, locked while recording), `Name:` (template prompt), `Dir:` (folder prompt, default `~/ShellSDR/recordings`), `[Record]`/`[Stop]` with elapsed time, size, drop warning or error. Recording runs while the source runs; Stop ends it; an engine restart continues in a new file; the switch is never persisted. ICMT key list frozen in `src/record/mod.rs`. 427 tests, clippy clean. Remaining in Step 8: scheduler, baseband file source.
- [~] Step 8a.1: CW tone up to 3000 Hz (user request, before the scheduler). DONE, AWAITING HARDWARE CHECK. Range 250..3000 Hz in the Radio field, bookmarks and the bookmark editor (shared `vfo::model::CW_TONE_MIN_HZ`/`CW_TONE_MAX_HZ`). The CW IF rate follows the pitch above SDR++'s 1250 Hz (`dsp::chain::cw_if_rate`), so a 3000 Hz pitch is not aliased (it would come out at ~906 Hz on the 3906.25 Hz IF). Tests: a 3000 Hz pitch at 100 and 500 Hz bandwidth from 2 MHz, and an unchanged IF for 250..1250 Hz.
- [~] Step 8b: Recording scheduler, Phase 0 + 1. DONE, AWAITING REVIEW. Phase 0: `schedule::window` (weekly UTC windows: occurrence identity, next start, overlap, local-time shift with weekday rotation, clock sanity; `Bookmark::is_online` now uses its time rule) and `bookmarks::model::atomic_write`. Phase 1: the Schedule module per section 9b: `schedule::job` (bookmark-driven job, tuning snapshot), `schedule::planner` (pure level-triggered state machine: source ownership, skip on user Stop, manual recording priority, retry backoff, Once, master switch), `schedule::module` (view state), `schedule::editor` (form, bookmark picker, local time), `schedule::store` (per-instance JSON), `schedule::glue` (AppState effects: tune Master to the bookmark, center + VFO, record), `ui::schedule_view`, mouse/keyboard wiring. No controller extraction was needed: the scheduler drives the existing desired-state switches. 491 tests, clippy clean.
- [~] Step 8c: Scheduler actions + VFO mute. DONE, AWAITING REVIEW. Per-VFO mute (`Vfo::muted`, `RadioField::Mute`, `[ ] Mute VFO`, persisted; zeroes the engine gain while one chain exists). Jobs gain `action` (`JobAction::Record`/`Tune`) and `mute`; the editor's Action field cycles Record audio / Record baseband / Tune only, plus a Mute checkbox; the planner's `Effect::StopJob` carries the action, `JobState::Recording` became `Active`; the glue mutes/restores and records only for Record jobs.
- [~] Step 8c.1: Clean terminal start/exit (user request). DONE, AWAITING CHECK. On start the alternate screen is explicitly cleared and ratatui's buffers reset, so no shell output shows through the first frame; on exit (and on a panic) the screen is cleared before leaving the alternate screen, so no UI is left in the terminal or its scrollback even where the alternate screen is unsupported. Native prints *during* a session are still only silenced around device open/stream activate. 538 tests, clippy clean.
- [~] Step 8d: Baseband file source (section 7.2). DONE, AWAITING REVIEW. `source::file` (`describe`/`probe`: I/Q 16-bit PCM or 32-bit float, center from ICMT then the SDR++ file-name rule; `IqReader` seekable frame reader; `Pacer` real-time pacing), `record::wav::WavInfo::format`. `SourceModule` File fields (`FilePath`, `FileTransport`, `FileBack`, `FileFwd`, `field_row` for the read-only rows between them), `FileView` (meta, error, position, pending seek, button cursors), kind-aware `selected_sample_rate`/`stream_key`/`source_label`. `AppState::load_file`/`press_file_button`/`file_finished`/`center_band` (center locked to the file). Engine `Input::{Soapy, File}` with `Command::Seek` and shared position/EOF; the event loop forwards seeks, mirrors the position, and stops + rewinds at the end. Config `source_kind` + `file_path`. File-open dialog `ui::file_browser` (folder navigation, WAV filter, header details, type-ahead, buttons, mouse), user request after review. Verified on `wav/20261104T100029_baseband_13264000Hz.wav`: 3.010 s played in 3.01 s wall, 20 spectrum fps, seek, start at an offset, EOF stop. 575 tests, clippy clean.
- [ ] Step 8: Recording + scheduler + RIFF metadata. Per-VFO Audio record, Master Baseband record, filename templates, scheduler intervals, WAV ICMT write; baseband file source reads center freq. Freeze ICMT key list. Review: template, ICMT round-trip, scheduler firing, WAV write+reread.
- [ ] Step 9 (superseded by 0.2-2, section 13.12): Decoder seam validation (no shipped decoders). Confirm `VfoProcessor` trait cleanly supports a stub text decoder VFO (disabled Master, collapsed rows). Review: seam works end-to-end with a dummy decoder.
- [ ] Step 10: Pi hardening. Footprint + CPU measurement on Pi 3, low-fi validation, RSPduo IFGR/RFGR verified on hardware. Review: measured budgets acceptable.

Version 0.2.0 steps: section 13.12.

(Steps are reorderable on request. Power spectrum and waterfall were raised ahead of secondary VFOs / bookmarks at the user's request, and Bookmarks (Step 7) was done before Secondary VFOs (Step 6). Recording is grouped after VFOs exist.)

## 13. Version 0.2.0: VFO types, Work Area, versioned files (DONE, AWAITING REVIEW)

Decided in the 0.2.0 design Q&A (2026-11-21) and implemented the same day (steps in 13.12); the sections it changes carry a "0.2.0" pointer here. Where the build differs from the first plan, the text below says so ("as built"). The version was bumped to `0.2.0` in step 0.2-1, since the files a 0.1.x build reads as "newer" are written from then on. Roadmap after it: 13.11.

### 13.1 Scope
- One VFO (Master) whose type can be changed in its own panel. Secondary VFOs (Step 6) stay out of 0.2.0, but every new structure is keyed by VFO name so Step 6 needs no schema change.
- The only type in 0.2.0 is `Voice`: today's demodulators (NFM/WFM/AM/DSB/USB/LSB/CW), unchanged in sound (the `examples/iq_wav_demod` A/B must give the same output before and after).
- Keep in mind (not designed here): 0.2.1 adds a `DSC` type whose core is an external `.so` and whose native output is decoded messages, not audio.

### 13.2 VFO type seam
- A `VfoType` trait (registry of the types compiled in) replaces the planned `VfoProcessor` (8.2, Step 9). Per type:
  - `id()`: the persisted type name (`"Voice"`).
  - `availability()`: `Ok`, or `Unavailable(reason)` when its library could not be loaded (13.9). An unavailable type is listed but cannot be selected; a VFO, bookmark or job using it is inert.
  - `defaults()` and `schema(&settings)`: the type's settings as a JSON object and its field descriptors (13.3). The schema is computed for the current settings, so ranges and steps may depend on other fields (Voice: bandwidth step and minimum depend on the mode).
  - `normalize(&mut settings, changed_key)`: applies dependent rules and validates (Voice: a mode change resets bandwidth and snap to the mode defaults; CW tone 250..3000 Hz).
  - `native_output()`: `Audio` (48 kHz mono WAV) or `Data` (format decided by the type, 0.2.1).
  - `native_token()`: the `$t` value of its native recording (`audio` for Voice).
  - `min_baseband_rate(&settings)`: the lowest IQ rate it decodes from (Voice: 1.25 x bandwidth); see 13.6.
  - `build_processor(&settings, source_rate, offset)`: its DSP (`VfoProcessor`: wideband IQ in, native output out, plus the channel IQ and meters for its view). As built, Voice's processor is the whole `VfoChain` (NCO, channelizer, demod, AF), unchanged: a settings change rebuilds it, as a mode change did before. Audio through the seam is sample-identical to the old path in all seven modes (test).
  - Also `bandwidth_hz`, `snap_hz`, `mode_label` (`$r`, status bar) and `summary` (lists: `USB 2.3 kHz`).
  - Code: `vfo::types` (`mod.rs` trait + registry, `schema.rs`, `voice.rs`, `libs.rs`). Unit tests compile a second, data-output type (`Test`) into the registry, so type switching is tested; the shipped build has only Voice.
- The Work Area view of a type (13.5) lives in `ui`, looked up by type id, so the DSP seam does not depend on ratatui.
- Changing a VFO's type rebuilds only that VFO's processor (as a mode change does today); its frequency stays.

### 13.3 Settings schema (JSON API between VFO types, the VFO panel and bookmarks)
- A schema is a list of fields: `{"key", "label", "kind", "default", "bookmark": bool, "visible_if": {"key": [values]}}` plus per-kind data. Kinds: `text` (`max_len`), `integer` and `number` (`min`, `max`, `step`, `unit`), `frequency_hz`, `choice` (`options`: list of `{value, label}`), `bool`.
- `bookmark: true` marks the settings a bookmark stores and applies (Voice: mode, bandwidth, CW tone, AM AGC). The rest are VFO-only (Voice: snap, squelch, recording choices).
- One schema-driven form renderer builds both the VFO panel's type rows and the bookmark editor's type rows, keyboard and mouse included (same field states and hit targets as today), so the two cannot diverge. After each change the form calls `normalize` and re-reads the schema.
- Unknown keys in a settings object are kept untouched on save (forward compatibility).

### 13.4 Per-VFO settings hierarchy (config)
- `VFO -> Type -> settings`. Common to all types of a VFO: name, frequency, enabled, collapsed, active type, audio device, mute, name template. Per type: everything else (Voice: mode, bandwidth, snap, CW tone, AM AGC, squelch, the recording checks of 13.6).
- TOML shape (per profile, `--config`):
  ```toml
  min_version = "0.2.0"
  [work_area]
  layout = "Tabs"        # Tabs | Split
  tab = "Spectrum"       # Spectrum | Vfo
  [vfo.Master]
  frequency_hz = 14200000
  vfo_type = "Voice"
  audio_device = "default"
  muted = false
  rec_template = "$t_$f_$h-$m-$s_$d-$M-$y"
  [vfo.Master.types.Voice]
  mode = "Usb"
  bandwidth_hz = 2800.0
  rec_native = true
  rec_baseband = false
  rec_minimize = false
  ```
- Each type the VFO has used keeps its table, so switching back restores it. A table of a type this build lacks is kept verbatim.
- Recording choices (Native, Baseband, Minimize) are therefore persisted per VFO per type.

### 13.5 Work Area
- The right side (bookmark strip + spectrum + waterfall) becomes the Work Area, with two layouts, persisted as `work_area.layout`:
  - Tabs: a one-line tab row on top, `[Spectrum] [Master: Voice]`, both clickable; the selected tab is persisted (`work_area.tab`). The Spectrum tab is today's view.
  - Split: two equal halves, the VFO view on the left (next to the left panel), today's view on the right; the tab row turns into the halves' titles.
- Global bar: a `[Tabs]`/`[Split]` control at the end of the first line (as built: the second line was already 80 columns; with both displays OFF the first line is 72). Accelerators: `w` switches the tab, `l` toggles the layout; both are in the help table (6.4). The tab row shows the same hint (`w: switch, l: Split`).
- The VFO view shows the active VFO's type view. Hidden views cost no CPU (their FFTs are not fed), like the spectrum OFF rule.
- Voice view (0.2.0), stacked top to bottom:
  - Zoomed channel: spectrum + waterfall of about +/-1.5 x bandwidth around the channel center (for USB/LSB the channel sits bw/2 beside the dial), limited to what the channel IQ holds (AM: about +/-7.8 kHz), filter edges drawn dim (`┊`), the dial marked. A click in it moves only the VFO (the source stays), snapped to 10 Hz (for CW/SSB, where one wide-view column is about 25 kHz). Fed by a small FFT of the VFO's channel IQ before the channel filter.
  - Audio waterfall: the demodulated audio 0..4 kHz (CW pitch, SSB voice band). Fed by a small FFT of the 48 kHz audio.
  - Meters line: `Level -42 dBFS  SNR 18 dB  AGC +23 dB` (level after the channel filter; SNR = passband mean over the median of the bins outside it; `AGC off` when the mode has none). Squelch is not implemented in ShellSDR yet, so it has no meter.
  - Recordings list: this VFO's recordings of the session (name, length, size, drops).
- Pi 3 budget: both view FFTs small (DEFAULT 512 points, 10 fps), measured in Step 10.

### 13.6 Recording: native, baseband, or both
- VFO panel rows (as built: one row per checkbox, each a cursor stop): `Rec: [x] Native (audio)`, `     [ ] Baseband`, `     [ ] Minimize baseband`, then today's `Name:`, `Dir:` (the recordings folder of the VFO's type: each type has its own, `recordings_dirs` in the profile keyed by the type id; a type without one uses the common `recordings_dir`, default `~/ShellSDR/recordings`) and one `[Record]`/`[Stop]` that starts and stops every checked output together. At least one check is required to record (otherwise the reason shows next to [Record]); the checks are locked while recording.
- Native: the type's output (Voice: audio before the volume, as today).
- Baseband: the IQ after the Source decimation (as today; no longer Master-only, though Master is the only VFO in 0.2.0).
- Minimize baseband: done after the Source decimation and independent of it: NCO shift to the VFO frequency, then the existing power-of-two channelizer to `effective_rate / 2^k`, the smallest that is at least `min_baseband_rate`. As built, Voice's minimum depends on the mode, because SSB sits beside the dial and CW is shifted up by its pitch: AM/NFM/WFM 1.25 x bandwidth, DSB/USB/LSB 2.2 x bandwidth, CW 2.2 x (pitch + bandwidth/2). Examples at 2 MHz: AM 10 kHz -> 15.625 kHz; USB 2.8 kHz -> 7.8125 kHz; CW 200 Hz at 800 Hz -> 3.906 kHz. The rate is fixed when the recording starts; the minimizer stays on its frequency when the source retunes. ICMT `center_freq` is then the VFO frequency, so the file plays back with the VFO at offset 0.
- Playback: a minimized file plays and demodulates in the same mode (the chain runs at the file's rate when it is below the mode's IF rate; tests for USB, AM and CW check the recovered tone). Opening a minimized file also puts the VFO back on the recorded frequency, type and settings (ICMT `vfo_freq`, `vfo_type`, `vfo_settings`), since the file is only the VFO's channel.
- File names: same template. `$t` expands to the native token (`audio`) or `baseband`. When the template has no `$t`, `_n` (native) or `_b` (baseband) is added before `.wav` (and before `_partN`). Both outputs of one recording share the same timestamp.
- ICMT `format_version` 2: adds `vfo_type`, `vfo_settings` (the type settings as compact JSON), `minimized` (0/1); `rec_type` becomes the native token or `baseband`; `center_freq` is the IQ's center for a baseband (the source center, or the VFO for a minimized one) and the source center for a native file, as before. Playback keeps reading version 1 files. The session's finished recordings are listed in the Voice view.

### 13.7 Bookmarks v2
- A bookmark is: common fields (name, frequency, UTC window, days, geo info, notes) + `vfo_type` + `settings` (that type's `bookmark: true` fields). [Add] captures the active VFO's type and bookmark settings.
- Editor: the common fields, then a `Type:` choice of the available types; switching it rebuilds the type rows from that type's schema with its defaults (as Mode resets Bandwidth today). The type rows come from the schema renderer (13.3).
- Unknown type (a borrowed bookmark from a build with other types, or a type whose library failed to load): listed and drawn on the strip in `dim` as `?DSC`; the common fields stay editable; its `settings` are saved verbatim; Tune and new jobs are refused with a message. Its `Type:` row is read-only.
- Tuning applies the type first, then its settings, then the frequency (same Center/Free rules as 9).

### 13.8 Scheduler
- Editor: `Action` becomes two radio rows, `( ) Tune only` and `( ) Record`, with `[x] Native [ ] Baseband` under Record (at least one; disabled under Tune only). Minimize comes from the VFO's own setting for the bookmark's type at run time.
- The job's tuning snapshot becomes `vfo_type` + `settings` (from the bookmark).
- When a job starts and the bookmark's type differs from Master's, the job switches Master to that type and its settings for the run, then restores the previous type (with its settings) afterwards, like the mute restore. Back-to-back jobs restore the state from before the first one.
- A job whose type is unknown or unavailable never fires; its details line says why.
- Job row action letters: `T` tune, `N` native, `B` baseband, `NB` both (two columns, then `m` and `!`): `›x 0600-0630 NBm NAVTEX`.
- As built: the editor rows are `( ) Tune only`, `( ) Record`, `    [x] Native`, `    [ ] Baseband`, `Mute`; under Tune only the output and Mute rows are disabled. A Record job with neither output is refused ("Choose Native or Baseband to record"). A bookmark of a type this build lacks cannot be picked for a job.

### 13.9 Versioned files and migration
- Every persisted file gets `min_version`: the lowest ShellSDR version that can read it (semver string). Missing means `0.1.0`.
- Profile TOML: a 0.1.0 file is copied to `<name>.toml.0.1.0.bak`, then migrated in memory (`vfos[]` -> `[vfo.<name>]` with type Voice; `rec_type` Audio/Baseband -> `rec_native`/`rec_baseband`) and written as 0.2.0 on the next save.
- Bookmarks: 0.2.0 uses `bookmarks.v2.json`. If it is missing, `bookmarks.json` is read once and migrated into it (every bookmark becomes Voice, its mode settings become `settings`); the old file is never written again, so a 0.1.x instance keeps working on it. The mtime reload watches the v2 file.
- Schedule: likewise `<profile>.schedule.v2.json`, migrated once from `<profile>.schedule.json` (`rec_type` -> the Native/Baseband checks, tuning -> type Voice + settings).
- A file whose `min_version` is newer than the running build is loaded best effort and read-only: it is never saved, and the status bar warns.
- Migration is a pure function per file and version step (`0.1.0 -> 0.2.0`), unit-tested with real 0.1.x samples.

### 13.10 Libraries and delivery
- Types with an external core library are compiled in (cargo feature per type) and `dlopen` their `.so` at runtime (`libloading`), so a missing library only makes that type unavailable (13.2); the binary always starts.
- Search order: `$SHELLSDR_LIB_DIR`, `<exe dir>/../lib/shellsdr`, `/usr/local/lib/shellsdr`, `/usr/lib/shellsdr`.
- `tools/dev` and the CI tarball gain `lib/shellsdr/` (empty in 0.2.0; a type's `assets/lib/<arch>/*.so` is copied there), installed by the same `sudo tar -C /usr/local -xzf ...`. README updated. Verified: the x64 and arm64 packages build in Docker, glibc floor still 2.39, no new runtime dependency (`libloading` uses glibc's `dlopen`).

### 13.11 Roadmap after 0.2.0
- 0.2.1: `DSC` VFO type (section 14). As decided in 14.1, its core is compiled in (ported from DSQrust), not an external `.so`.
- 0.3.0: Work Area pages, simpler side modules and the Report module (sections 19, 20); later the network remote and a graphical window (21).
- Later: audio notches for Voice (user-placed notch filters on the demodulated audio, as in SDRuno / SDRconnect, editable from the audio waterfall); secondary VFOs (Step 6); `--headless`.

### 13.12 Steps (strict TDD, each reviewed before the next)
- [~] 0.2-1: Versioning + migration framework (`version.rs`): `min_version` in the TOML and both JSON files, `<file>.0.1.0.bak` for a legacy profile, read-only newer files (status bar `READ-ONLY: ...`), `bookmarks.v2.json` / `<profile>.schedule.v2.json` migrated once from the 0.1.x files, which are never written again. DONE, AWAITING REVIEW.
- [~] 0.2-2: `VfoType` seam + registry; `Voice` wraps the chain; settings as JSON; the engine builds the processor through the registry (`Command::SetVfoSettings`). Sample-identical audio (test). DONE, AWAITING REVIEW.
- [~] 0.2-3: Schema (`vfo::types::schema`) + `ui::schema_form`; hierarchical config (13.4, migration from `[[vfos]]`); `Type:` row (picker, Left/Right); the Voice rows are `RadioField::Setting(i)` from the schema. DONE, AWAITING REVIEW.
- [~] 0.2-4: `RecChoice` per type, `RecKind`, `recording_plans` (one writer per output), the engine's `RecTaps` + `Minimizer`, `_n`/`_b` suffixes, ICMT v2, minimized-file playback and VFO restore. DONE, AWAITING REVIEW.
- [~] 0.2-5: Work Area frame: `compute_layout_with`, tab row, Split, `[Tabs]`/`[Split]`, `w`/`l`, persistence, FFT off while the spectrum tab is hidden. DONE, AWAITING REVIEW.
- [~] 0.2-6: Voice view (`ui::vfo_view`): zoomed channel (fine click-to-tune), audio waterfall, meters, recordings list; engine `VfoViewFrame` (512-point FFTs at 10 fps, only while the view is on screen). DONE, AWAITING REVIEW.
- [~] 0.2-7: Bookmarks v2: `vfo_type` + `settings`, 0.1.x fields migrated on load, schema-driven editor with `Type:`, unknown types inert (`?Name` dim on the strip, `?DSC` details, Tune refused with a notice, settings kept). DONE, AWAITING REVIEW.
- [~] 0.2-8: Scheduler: Tune only / Record + Native/Baseband, typed tuning snapshot, type switch + restore, jobs of an unknown type never fire (`Never: DSC not in this build`), migrated jobs. DONE, AWAITING REVIEW.
- [~] 0.2-9: Library loader (`vfo::types::libs`, search order 13.10), `lib/shellsdr` in packages and CI, version `0.2.0`, README / HANDOVER. DONE, AWAITING REVIEW.

## 14. Version 0.2.1: the DSC VFO type (DONE, AWAITING REVIEW)

Decided 2026-11-21 (Q&A after the 0.2.0 build). Sources: the DSC seed (`DSC-Decoder-Seed`: requirements, algorithms, conformance vectors), the C++ ncurses decoder (`/home/sp/Programs/dsc`: the interface copied for the Work Area) and DSQrust (Rust + Qt 6: the decoding and data library ported here).

### 14.1 Core
- Compiled in, not a `.so` (this replaces 13.11's plan): DSQrust's library is ported to `src/dsc/` (decoder, parser, domain tables, record model, NDJSON store, enrichment, stations, online lookup, alerts, presentation). The `lib/shellsdr` loader stays for future C/C++ cores.
- The seed's conformance suite is copied to `tests/dsc/conformance/` and run by DSQrust's runner, ported as the integration tests `dsc_conformance` and `dsc_runner_selftest`.
- New dependencies: `jiff` (time zones, the day file's local date) and `ureq` (aprs.fi lookup, as DSQrust; TLS by `rustls` + `ring`, compiled in). Verified: the x64 and arm64 packages build in Docker, glibc floor still 2.39, no new runtime library (the binary needs the same `libSoapySDR`, `libasound`, `libc`, `libm`, `libgcc_s`).

### 14.2 Signal path
- The VFO frequency is the DSC frequency itself (e.g. 8414.5 kHz), not a USB dial. There is no audio: the processor decodes the IQ directly.
- `NCO(-offset)` -> the power-of-two channelizer to the smallest `rate / 2^k` >= 8 kHz (as built: the seed's filter divides by `sinc(2 i fc)`, which turns negative below about 8 kHz, so the demodulator needs at least that rate; at 2 MHz the channel runs at 15.625 kHz) -> a slow AGC (DSQrust's discriminator takes the log of integer levels, so the input is normalized to a fixed RMS) -> the seed's demodulator with complex input: the mark tone at +85 Hz, the space tone at -85 Hz -> the receiver -> calls.
- The demodulator is DSQrust's, generalized: the audio path (`process(&[i16])`, used by the conformance suite) feeds `(x, x)` into the same complex core, so both paths share one implementation.
- `bandwidth_hz` 400 (markers), `snap_hz` 500 (DSC frequencies end in .0 or .5 kHz), `min_baseband_rate` 8000 (a minimized file must still decode), `mode_label`/`summary` `DSC`, native token `dsc`.
- Meters for the view: the tone level (0..1, seed 06 2) and the state: SILENT, ACTIVITY (level > 0.6), DECODING (the receiver is reading a call).

### 14.3 Settings (`[vfo.Master.types.DSC]`)
- Schema (VFO panel): `message_dir`, the directory of the day files (picked with the directory browser, typed, or empty = the DSC type's recordings folder, `Dir:`; until 0.2.2 `$XDG_DATA_HOME/ShellSDR/dsc`).
- Kept in the same table but set from the Work Area's Tools menu (not in the schema, so not in the panel or bookmarks): `location` (`lat, lon` or empty), `lookup` (aprs.fi on/off), `api_key`, `endpoint`, `alert_sound`, `alert_urgency`, `desktop_notify`, `save_dir`, `detail_layout`.
- Only the processor-relevant part of the settings rebuilds the processor (`VfoType::dsp_settings`); DSC has none, so a Tools change never interrupts a call being read.

### 14.4 Native output: the messages
- The native recording of DSC is always on: while the radio runs and the VFO is DSC, every call is recorded (seed STO-01), even duplicates and garbled ones. It cannot be disabled.
- VFO panel: `Rec: [x] Native (dsc, always)` is shown checked and locked (as built: the panel is 30 columns wide); `[ ] Baseband`, `[ ] Minimize baseband`, `Name:`, `Dir:` and `[Record]` work as for Voice but `[Record]` records the baseband only (without Baseband checked it says `Check Baseband to record`). Audio and Mute rows are hidden (no audio).
- Records: DSQrust's schema (v 2), one line per call appended to `<message_dir>/YYYY-MM-DD.ndjson` by local date (seed STO-02, STO-03, STO-08), on a writer thread. Source label = the VFO frequency (`8414.5 kHz`). Time: the wall clock at the call's phasing (time source `wallclock`); during file playback, the file's recorded start time plus the play position (`filename`).
- Enrichment as DSQrust: station CSVs in `<message_dir>/stations/`, the built-in coast stations, the position cache, the user's location and distances; aprs.fi lookup on a worker when enabled with a key; alerts (distress always, urgency if enabled).

### 14.5 Only today's messages
- The view shows the current local day only: today's day file at start, then every line appended to it, by this VFO or by another ShellSDR instance writing to the same directory (re-read about every second; this VFO's own records are shown at once and not twice). At local midnight it switches to the new day. No other day file can be opened (the standalone `Open`, `WAV`, `Live`, `Stop` and `Quit` commands do not exist here).
- Changing `message_dir` reloads the view from the new directory.

### 14.6 Work Area view (the ncurses decoder's interface)
- Status box: `LIVE*` (radio running, VFO is DSC) or `Stopped`, `msgs: n`; one row for this VFO: Channel (VFO frequency), Source (device or file), Status (`idle`, `silent`, `activity`, `DECODING...`, `API_REQ...`) with a level bar. The alert banner sits on top while an alert is active.
- Messages box (`Messages: x of n  Pages: p of q  File: YYYY-MM-DD`): TIME, severity letter, SRC, FMT, CATEGORY, FROM, TO; severity shown by letter and colour; garbled (`!!`) rows dim.
- Detail box (on demand): the DSQrust detail text (identification, parties with country, name, position and distance, distress, messages 2/3, expansion, reception, raw symbols), scrollable. Beside the list on a wide view, below it on a tall one; F3 cycles `Auto View` / `Horizontal` / `Vertical`.
- Function-key bar at the bottom of the view (Midnight Commander style, clickable): `1Help 2Save 3Layout 5Copy 6Detail 7Search 8Follow 9Menu`. F-keys are reserved for Work Area views: ShellSDR itself binds none. They act on the active VFO's view whenever it is on screen (Tabs or Split), whatever has the focus.
- F9 menu bar (keyboard and mouse): View (Detail, Detail layout, Follow/Hold), Filter (Search, Distress only, Clear filters), Tools (Set my location, Online ship lookup, Set aprs.fi API key, Alert sound, Alert on urgency, Desktop notifications, Reload stations, Acknowledge alert), Help (Keys, About).
- Focus: when the view is visible, the Work Area joins the Tab cycle after Schedule (messages, then the detail when open). Focused: Up/Down/PgUp/PgDn/Home/End move the selection (switching to Hold), Enter opens/closes the detail, `/` search, `d` distress only, `f` follow, `a` acknowledge the alert, `b` alert sound, Esc clears the filters. Other keys keep their ShellSDR meaning.
- Search (seed VIEW-06), distress-only (VIEW-07), Follow/Hold (06 6), Save to `.txt` (offers the message folder; 0.2.1 and 0.2.2 remembered the last directory in `save_dir`, now unused), Copy (`wl-copy`, `xclip` or `xsel`).
- Alerts: banner until acknowledged, the terminal bell when `alert_sound` is on, `notify-send` when `desktop_notify` is on; while the DSC view is hidden the status bar shows the alert.

### 14.7 Scheduler and bookmarks
- A DSC job: Native is always on (its check shown locked). `Tune only` switches Master to DSC with no end (it stays until another job or the user changes it); `Record` is the same with an end time, after which the previous type is restored; Baseband is optional.
- A DSC bookmark is its type and frequency (DSC has no bookmark settings).

### 14.8 Version
- `Cargo.toml` 0.2.1. `FILE_MIN_VERSION` stays `0.2.0`: a 0.2.0 build reads 0.2.1 files and shows DSC bookmarks, jobs and VFO tables as an unknown type (13.7), keeping them verbatim.

### 14.9 As built
- Day files of other dates: a call whose local date is not today (file playback of an older recording) is written to its own day file and not shown; a notice names the file.
- File playback time: the file name's start time (seed TIM-02, `YYYYMMDD` + `HHMMSS`), else its modification time, plus the play position.
- The status bar shows an active alert too (red), so it is seen with the view hidden.
- Keys: letters ShellSDR does not bind now reach the focused view (`Action::Char`), and `F1..F12` arrive as `Action::FKey`, used only by the DSC view; elsewhere they do nothing.
- Fixed with 0.2.1 (a 0.2.0 bug): a Tune only job of another type had its type restored on the next tick (only Record jobs count as running); it now stays (14.7).
- A DSC Record job without Baseband records nothing extra (the messages are always recorded) and only holds DSC for its window.
- Tests: the seed's 17 suites (`dsc_conformance`, all normative cases pass; `detail` and two `list-row` cases are informative wording differences, as in DSQrust), `dsc_iq_decode` (all 15 audio vectors decoded through the IQ path with the same symbols), the service, the glue, the view, the scheduler rules. `examples/dsc_baseband.rs` writes a synthetic baseband with three calls for trying the view.

### 14.10 Steps
- [~] 0.2.1-1: Port the DSQrust library to `src/dsc/` with its unit tests; conformance suite + runner pass. DONE, AWAITING REVIEW.
- [~] 0.2.1-2: Complex demodulator core; `DSC` type + processor (channelizer, AGC, decoder); calls and meters from the DSP thread to the UI; IQ decode of the conformance recordings. DONE, AWAITING REVIEW.
- [~] 0.2.1-3: `DscService`: today's view, writer thread, follow the day file, midnight, enrichment, lookup, alerts, notifications, settings. DONE, AWAITING REVIEW.
- [~] 0.2.1-4: VFO panel (message folder, locked Native, baseband-only Record) and scheduler rules. DONE, AWAITING REVIEW.
- [~] 0.2.1-5: The DSC Work Area view: boxes, F-key bar, F9 menu, dialogs, focus, mouse, help. DONE, AWAITING REVIEW.
- [~] 0.2.1-6: Status-bar alert, README / HANDOVER, Docker packages. DONE, AWAITING REVIEW.

## 15. Version 0.2.2: the FAX (weather fax) VFO type (DONE, AWAITING REVIEW)

Decided 2026-11-21 (after 0.2.1). Audio notches stay deferred until the user says so. Sources: the fax decoder `../FAX` (C++ library `fax::fax` + `fax` tool, GPL v3 core from OpenCPN via KiwiSDR) and its sibling design `../../workspace_ms/scheduler-station/docs/`.

### 15.1 Core: the FAX library, extended (FAX 1.3.0)
- Not ported to Rust: FAX has a C API (`include/fax/fax_c.h`) and a shared library `libfax.so.1`, which ShellSDR loads with `dlopen` (`fax::ffi`, found like every type library, 13.10). Without it the FAX type is unavailable (it cannot be chosen; a VFO or bookmark of that type is kept).
- FAX changes, committed in FAX in its own style:
  - `99c0663` Phasing fix: the line-start skip is taken only from the phasing lines directly after a START tone, and only when at least 75% of them look like phasing lines (one level with a single ~5% pulse). Chart lines after a false START (sparse text, a vertical frame) no longer shift the picture (DWD 3855 kHz at 48 kHz: from line 389 on it was shifted by 1360 samples).
  - `b7cb165` PNG output by default (`--pgm` keeps the old format). The PNG is valid while it grows: every 16 lines the zlib stream is sync-flushed, closed with an empty final stored block, the Adler-32 and IEND, and the IHDR height and CRC are rewritten; the next lines overwrite that tail.
  - `a3a7d01` Live decoding (`fax::LiveDecoder`): `FaxDecoder` runs as a demodulator only and the demodulated stream is kept (1 byte per sample at 12 kHz, 43 MB per hour, bounded by a cache limit, oldest dropped first), so a picture is drawn from it at any clock correction. Pictures: a START followed by real phasing lines begins an aligned picture; a START without phasing lines is ignored inside a picture; a STOP ends one; the gap after a STOP is a picture without START, dropped at the next START if shorter than 200 lines. In live mode the tones are detected on the whole line (no false START/STOP at 48 kHz). Slant jobs copy the lines and run in any thread. C API, `libfax.so.1` (only `fax_*` exported), `FAX_PORTABLE` (no AVX-512), ctest with a synthetic fax.
- Licence: the package includes `lib/shellsdr/libfax.so.1` and `share/doc/shellsdr/{libfax-NOTICE,GPL-3}`; the binary package as a whole is GPL v3 because of it; ShellSDR's own source stays MIT.

### 15.2 Signal path
- The VFO frequency is the fax's assigned (centre) frequency, as published (e.g. DWD 3855.0 kHz); markers show 1.2 kHz around it, snap 100 Hz. The processor (`vfo::types::fax::FaxProcessor`) is the Voice USB chain (3 kHz) with its dial 1900 Hz below, so the fax carrier is 1900 Hz audio. The 48 kHz audio plays (Audio and Mute rows as for Voice: `VfoType::plays_audio`) and goes as 16-bit samples (`DataItem::FaxAudio`) to the UI's `fax::service::FaxService`, which pushes it into the library (decimated to 12 kHz there). A minimized baseband keeps 12 kHz.

### 15.3 Settings (`[vfo.Master.types.FAX]`)
- Schema: `lpm` (120, 60, 90, 240; bookmark), `ioc` (576 or 288 -> width 1809 or 904 px; bookmark), `auto_slant` (on), `clock_ppm` (the clock correction in FAX's `-s` convention, -500..500, step 0.1). (0.2.2 also had `save_dir`, `Pictures:`; since 0.2.4 F2 Save writes to the FAX type's recordings folder and the key is dropped on load.)
- Not in the schema: `display` (`Blocks` / `Pixels`), `ppm_step` (0.1 / 1).
- None of them rebuilds the processor (`dsp_settings` is empty). A new LPM or IOC starts the decoder again (the pictures so far are dropped).

### 15.4 Pictures, cache, recording, saving
- The cache keeps up to half of the computer's memory (`MemTotal / 2`), then the oldest part is dropped.
- [Record] with Native writes each fax to its own PNG that grows line by line (valid at any moment), named by the recording template at the picture's start (`$t` = `fax`, `.png`, `_2`... if taken). A picture without START gets a file once it has 200 lines. A picture over 20000 lines (2 h 45 min at 120 LPM) continues in `_part2`, ... A change of the clock correction rewrites the current part from the cache. The lines a picture lost when the next START was found (a few, written already) are cut off when its file is closed. Baseband (WAV) can be recorded at the same time.
- [Save] (F2) writes the picture shown (from its oldest line in memory down to the last line on screen) to the FAX type's recordings folder (`Dir:`), named `YYYYMMDD-HHMMSS-<VFO>-<freq>Hz-FAX.png`, and frees the cache before its end (and the older pictures).

### 15.5 Straightening (slant)
- Manual: F4/F5 step the clock correction (0.1 or 1 ppm, `s` switches, `0` resets) of the picture shown and of the pictures still to come; it is redrawn at once. A manual step turns Auto off.
- Auto (on by default): the slant of the newest picture that grew by 100 lines (from 200 lines on) is measured on a worker thread from its last 2000 lines. Until a result is found the view says `Slant: waiting (n of 200 lines)` / `Slant: measuring (n lines)` in the accent colour; then it applies it (`Slant: -58.90 ppm auto (2000 lines, confidence 3.4)`) and saves it in the profile. Early values are rough: the estimator works on 1024 columns, so at 200 lines one column of drift is about 5 ppm (the reference fax gave -53.8 at 200 lines, -58.4 at 900, -58.9 from 1700), still under about 3 px of drift over the lines seen; F6 back on re-applies the value found. New pictures start with the last value. Turning Auto off (F6) keeps it.

### 15.6 Work Area view (`ui::fax_view`)
- Status lines: the state (`stopped`, `waiting`, `receiving`, `START`, `phasing`, `STOP`), the picture (n/total, fax / aligned / no START), its start time, lines, LPM/IOC, follow or hold; then the slant state, the cache use and REC with the file and lines.
- The picture fills the rest at full width, newest lines at the bottom while following. Unicode half blocks (truecolor, or the 256-colour greys without `COLORTERM=truecolor`); on Kitty, WezTerm and Ghostty (from the environment, not inside tmux/screen) `Pixels`: a real picture placed over the area after the frame with the Kitty graphics protocol (PNG, replaced when the view changes, at most every 2 s while it grows; removed under popups, help and the menu).
- Keys: `1Help 2Save 3Pixels/Blocks 4-ppm 5+ppm 6Auto/Manual 7Older 8Newer 9Menu`; when focused: Up/Down/PgUp/PgDn/Home scroll (hold), End follows, Left/Right older/newer, `s` ppm step, `0` zero. The menu: Picture (save, older, newer, follow, pixels), Slant (auto, ppm -/+, step, zero), Help (keys, about).

### 15.7 Scheduler and bookmarks
- A FAX job: `Record` with Native records the PNGs for its window; `Tune only` switches to FAX for good (14.7 rules).
- A FAX bookmark stores LPM and IOC.

### 15.8 Steps
- [x] 0.2.2-1: FAX: phasing fix, PNG writer + default, LiveDecoder + C API + shared library, tests, FAX 1.3.0.
- [x] 0.2.2-2: ShellSDR: the `libfax` binding (`fax::ffi`), the FAX type and processor, data to the UI.
- [x] 0.2.2-3: `FaxService`: cache cap, pictures, slant worker, save, record.
- [x] 0.2.2-4: The FAX Work Area view (half blocks, Kitty), F-keys, menu, help.
- [x] 0.2.2-5: Recording and scheduler integration.
- [x] 0.2.2-6: Packaging (`libfax.so.1` built in Docker for both targets, licence notice), README / HANDOVER, version 0.2.2.

## 16. Version 0.2.5: the Digital VFO type (MFSK, pictures, RSID) (DONE, AWAITING REVIEW)

Decided 2026-11-21 (the user's request after 0.2.4). Sources: fldigi 4.1.23, checked against a real reception: Shortwave Radiogram 475 (WRMI 15770 kHz, AM), (a 30 min minimized baseband at 15625 Hz, fldigi's hand-cleaned text and its 11 pictures).

### 16.1 Port, not a library
- fldigi's MFSK receiver is ported to Rust in `src/mfsk/`, not wrapped as a `.so` like FAX. Reasons: the modem is entangled with fldigi's globals and FLTK (configuration, waterfall, picture windows, the trx thread), so a C library would be a rewrite of the glue anyway; the core that matters is small (sliding FFT, FIR, Viterbi, interleaver, varicode, picture receiver, RSID detector: about 1500 lines); it has to change anyway to take IQ instead of audio and to combine sidebands; a Rust port needs no second toolchain in the Docker packages and is tested with `cargo test`.
- Ported for receiving only, plus a test-only transmitter (`mfsk::tx`: varicode, convolutional encoder, interleaver, tones, pictures, RSID tones) that generates the test signals.
- Licence: the port is a derived work of fldigi (GPL v3 or later), so ShellSDR's source becomes GPL v3 or later from 0.2.5 (versions up to 0.2.4 stay available under MIT). `Cargo.toml` `license = "GPL-3.0-or-later"`, a `COPYING` file, README credits: fldigi (Dave Freese W1HKJ, Stelios Bounanos M0GLD, John Douyere VK2ETA and contributors), gMFSK (Tomi Manninen OH2BNS), the MFSK varicode (Murray Greenman ZL1BPU, Nino Porcino IZ8BLY), RSID (Patrick Lindecker F6CTE).

### 16.2 One type for the RSID text modes
- One VFO type, `Digital`, with a `Mode` choice like Voice. 0.2.5 has the MFSK family: MFSK-4, MFSK-8, MFSK-11, MFSK-16, MFSK-22, MFSK-31, MFSK-32, MFSK-64, MFSK-128, MFSK-64L, MFSK-128L (fldigi's names and parameters). Later modes RSID knows (PSK, Olivia, THOR...) are added to the same choice.
- Tuning as in fldigi: the VFO frequency is the dial (the AM carrier for a broadcast, the USB dial for amateurs) and the modem sits at an audio offset, `Carrier` (default 1500 Hz, 200..3800). Snap 100 Hz.

### 16.3 Signal path: IQ, no audio
- DSP thread (`DigitalProcessor`): NCO to the VFO frequency, the power-of-two channelizer to the smallest rate >= 8 kHz (15.625 kHz at 2 MHz), handed to the UI as `DataItem::DigitalIq`. No audio is produced or played (the Audio and Mute rows are hidden).
- Worker thread (`mfsk::service`): the sideband, then complex resampling to 8000 Hz (the modem; 11025 Hz for MFSK-11/22) and 11025 Hz (RSID).
  - `Upper`: the positive frequencies of the IQ (the upper sideband; a USB signal).
  - `Lower`: the IQ conjugated (the lower sideband mirrored, so its tones read the same).
  - `Both`: an AM carrier PLL (second order, 10 Hz, its phase detector behind a 100 Hz low-pass) turns the IQ so the carrier is real; its positive frequencies are then the upper sideband and its negative ones the lower sideband mirrored. Needs the AM carrier, the VFO within a few tens of Hz of it (the Radiogram recording: +16 Hz, locked at once).
- As built, `Both` is diversity, not a coherent sum: the modem runs fldigi's front end (Hilbert, mixer, band-pass, sliding FFT) once per sideband (`Rx::new_diversity`) and decides on the summed tone magnitudes, AFC and picture frequency on the summed phase products; the RSID detector adds the power of each audio frequency and its mirror (`Detector::set_fold`). Measured on the Radiogram recording: the coherent sum (the real part, i.e. synchronous AM) lost the closing RSID (2 of 15 tones cancelled by a selective fade that either sideband alone had clean) and decoded no better than one sideband (mean metric 90 to 92); diversity found every RSID and all 11 pictures with a mean metric of 95 to 97.
- The modem's front end is fldigi's, with its Hilbert filter applied as a complex filter (`h_i + j h_q`) to the complex input: for a real input it is fldigi's analytic signal, for IQ it keeps the positive frequencies (the other sideband and stations below the dial are rejected).
- As built, no setting rebuilds the processor (`dsp_settings` is empty): it only shifts and decimates; sideband, mode, carrier, AFC, RSID and squelch all go to the worker without interrupting the stream.
- AFC: fldigi subtracts the averaged error itself every symbol, an integrator that overshoots and rings (a signal 4 Hz off ended 8 Hz off); here the carrier moves by 1/32 of each measured error, same smoothing, no overshoot.

### 16.4 RSID
- fldigi's detector (both code tables, ESCAPE to the second one, error tolerance "medium" = 1), on the 11025 Hz stream, searching the whole passband (200..3800 Hz).
- On a code of a mode this build decodes (and `RSID` on): the mode and carrier switch to it, as fldigi does, with the marks below. A code of another mode (BPSK31, Olivia...) or an unknown code: only a mark, nothing switches. `EOT` is ignored. With `RSID` off no RSID is searched (it saves the FFTs on a Pi).

### 16.5 Squelch (no garbage in noise)
- fldigi prints whatever the Viterbi decoder makes of noise unless its manual squelch is set. Here the squelch is on by default, with hysteresis: it opens when the Viterbi metric (0..100, fldigi's scale) reaches the `Squelch` level and closes 15 below it. Default 50 (closes at 35), measured on the Radiogram recording (diversity): in noise the metric never passed 41, in the programme it never fell below 38 once open; over the 30 minutes one noise character passes and no programme character is lost (a single threshold of 40 lost 3 and passed 7). 0 = off. Pictures are not squelched (they start from a header that was received). AFC works only while the metric is above the closing level.

### 16.6 Text, pictures and marks
- The decoder's bytes are UTF-8 (invalid sequences as Latin-1), CR LF and CR to a line end, control characters dropped.
- Pictures: fldigi's receiver (`Pic:WxH;` grey, `Pic:WxHC;` colour, `p4`/`p2` speeds). A picture ends after its sample count, as in fldigi, and is kept with its time.
- Marks, as fldigi's "Before RSID" / "New RSID" (UTC, the mode, `@ <VFO Hz>+<carrier Hz>`, `-` for Lower):
  `==🔵*Before RSID: <<2026-11-18T18:51Z MFSK-32 @ 15770000+1496>>*==`, `==🔵*New RSID: <<2026-11-18T18:56Z MFSK-64 @ 15770000+1496>>*==` (the new carrier), `==🔵*RSID: BPSK31 (not decoded here) @ 15770000+1000*==`, `==🔵*RSID: unknown code 1234 @ ...*==`. Each is a paragraph of its own (Obsidian: blue highlight, italics). The time is the wall clock, or during file playback the file's start time plus the play position (as DSC).

### 16.7 Saving and recording
- The Digital type's recordings folder (`Dir:`) is the root: Markdown files there, pictures in its `pictures/` subfolder, named `pic_` + the recording template expanded at the picture's time (`$t` = `digital`, `_2`... if taken; since 0.2.7, see section 18), linked from the Markdown as `![Pic:199x145C](pictures/pic_20261118-190031_digital_15770000Hz.png)` in the paragraph after the `Sending Pic:199x145C;` line.
- Text is written verbatim except what Markdown would misread (a leading `#`, `>`, `-`/`*`/`+` list marker or `|`, and `<`, `*`, `` ` ``, `==`, `[`, `]` inside a line are escaped with `\`). Line ends are kept (Obsidian shows them).
- Tune only (scheduler) or the VFO without [Record]: text and pictures are decoded and shown, nothing is written. F2 Save writes the session so far (text, marks, pictures) to `YYYYMMDD-HHMMSS-<VFO>-<freq>Hz-Digital.md` in `Dir:` (with its pictures).
- [Record] with Native: a Markdown file named by the recording template (`$t` = `digital`, `.md`) grows as text arrives (flushed at least every second); each picture is written when it is complete. Baseband can be recorded at the same time.

### 16.8 Settings (`[vfo.Master.types.Digital]`)
- Schema: `mode` (bookmark), `sideband` (`Upper`, `Lower`, `Both`; bookmark), `carrier_hz` (bookmark), `afc` (on), `rsid` (on: switch on RSID), `squelch` (0..100).
- The worker reports the carrier found by AFC or RSID and a mode switched by RSID; they are written back into the settings (so a bookmark [Add] stores them).

### 16.9 Work Area view (`ui::digital_view`)
- Status line: mode, carrier, sideband, AFC, RSID, squelch, S/N, the metric bar, REC with the file.
- Waterfall of 0..4000 Hz (from the RSID detector's FFT, no extra cost) with the modem's tone band marked; a click sets the carrier.
- The received text, wrapped, newest at the bottom (follows; Up/PgUp hold, End follows); marks in the accent colour. The current or selected picture beside the text (or above it on a narrow view) in half blocks (truecolor, or the 256 colours).
- Keys: `1Help 2Save 3Mode 4Side 5AFC 6RSID 7Sql- 8Sql+ 9Menu`; focused: Up/Down/PgUp/PgDn/Home/End scroll, Left/Right older/newer picture, `c` clears the text. F9 menu: Text (save, clear, follow), Modem (mode..., sideband, AFC, RSID, squelch), Help (keys, about with the credits).

### 16.10 Scheduler and bookmarks
- A Digital job: `Record` with Native writes the Markdown for its window; `Tune only` switches Master to Digital for good (14.7 rules). A bookmark stores mode, sideband and carrier.

### 16.11 As built
- Code: `src/mfsk/` (`filters`, `viterbi`, `interleave`, `varicode`, `modes`, `rx`, `rsid`, `tx` for tests, `frontend`, `decoder`, `markdown`), `src/digital/service.rs` (worker thread, session lines, recording, save), `src/vfo/types/digital.rs` (type, `DigitalProcessor`, `DataItem::DigitalIq`), `src/digital_app.rs`, `src/ui/digital_view.rs`. New dependency `png` (pure Rust).
- Tests: loopback of every mode (text), the other sideband rejected, AFC, the noise metric, colour pictures in the four picture modes, RSID codes of both tables, the sidebands and the PLL, RSID switching, notes for other modes, squelch on noise, the Markdown, the service and its worker. `cargo test --release --test digital_radiogram -- --ignored` decodes the whole Radiogram 475 recording (43 s on the dev PC for 30 min): the three RSIDs, key lines of the text, the 11 pictures against fldigi's (no brightness bias, median difference at most 16 where noise streaks differ; ours have fewer).
- `examples/mfsk_file.rs` prints a recording's text with the metric per line (for squelch work): `cargo run --release --example mfsk_file -- <baseband.wav> Both`.
- The view's waterfall comes from the RSID detector's FFT (21 per second), so it needs RSID on to move.

### 16.12 Steps
- [x] 0.2.5-1: Port: filters, sliding FFT, Viterbi + encoder, interleaver, varicode, modes, the MFSK receiver with pictures, the RSID detector; test transmitter; loopback tests for every mode, a picture, RSID codes of both tables.
- [x] 0.2.5-2: Front end: the Digital type and processor, sidebands (Upper, Lower, Both with the PLL), resampling, the worker; IQ tests (each sideband, a congested sideband).
- [x] 0.2.5-3: Squelch, text model, Markdown, marks, pictures, saving and recording; the Radiogram recording as an ignored integration test (text compared with fldigi's, 11 pictures).
- [x] 0.2.5-4: The view, keys, menu, mouse, help; app glue, scheduler and bookmarks.
- [x] 0.2.5-5: Licence (`COPYING`, `GPL-3.0-or-later`), README (0.2.5, credits) / HANDOVER, version 0.2.5. Packages not built yet (on request).

## 17. Version 0.2.6: [Max] playback of a baseband file (DONE, AWAITING REVIEW)
User request: decode a recorded baseband as soon as possible (weather fax, DSC, Digital), not in real time.

### 17.1 Behaviour
- The File source's transport row is `[Start] [Max] [Rewind]`. [Max] starts a stopped playback at Max, or switches a running one to Max; while at Max the button reads `[1×]` and returns to real time (the real-time clock restarts at the current position). Stop and the end of the file end Max: the next Start plays in real time. It is not saved in the profile.
- At Max the file is read without pacing, in 100 ms blocks (20 of SDR++'s 5 ms blocks).
- The position line shows the speed reached, `▶▶45×` (play position over wall time, measured over 2 s windows; `▶▶` until the first window closes).
- Nothing is sent to the audio output at Max (it would only fill its buffer); a native audio recording still gets every sample.
- Spectrum, waterfall and the VFO view: the analyzers are fed only once the UI has taken their last frame, so they show every so many frames at the UI's pace instead of scrolling faster, and skipped frames cost no FFTs.
- Decoded data is dated by the file position it was published at (`Engine::take_data_at`: batches with their position); the UI sets the play position to each batch's before handing it on, so DSC call times and Digital marks and picture names are as in real time even though the file moves seconds ahead between two UI frames.

### 17.2 Back pressure
- The UI loop carries decoded data from the engine to the decoders (FAX decodes on the UI thread, Digital on its worker), so a decoder slower than the reader would let a queue grow. At Max the reader waits (5 ms steps) while 256 data batches (25.6 s of file) are not taken, or while the Digital worker has more than 100 blocks (10 s) to decode (`Engine::set_file_hold`, `DigitalService::backlog`: blocks sent and not fully answered). In real time nothing waits.
- At the end of the file after Max, the stop (which ends the recording) waits until the Digital worker has answered every block, and takes its last results first, so the Markdown gets the tail.

### 17.3 As built
- Code: `engine` (`Shared::{file_fast, file_hold}`, data with positions, `must_wait`, `batches`, `FAST_BLOCKS`, `MAX_PENDING_DATA`), `source::model` (`FileButton::Max`, `FileView::{fast, speed}`, `SpeedMeter`), `app` (`press_file_button`, Stop clears Max), `ui::run` (hands `fast`/`hold` to the engine, measures the speed, dated batches, the end-of-file wait), `ui::render` (button, speed), `digital::service` (`backlog`).
- Tried in tmux: Radiogram 475 (15.6 kHz minimized baseband, Digital Both, [Record] Native): 30 min in about 40 s (the Digital worker is the limit, as the standalone decode's 43 s); all three RSIDs with the file's times, the 11 pictures, every line of the hand-cleaned reference text verbatim, the recording complete to the end. A 250 kHz Longwave baseband with Voice: 8 to 9× (the DSP thread is the limit); [1×] went back to real time at once.
- Not tested: FAX at Max (it decodes on the UI thread: the back pressure bounds the queue, but the UI may be slow to answer keys meanwhile), a Pi 3.

## 18. Version 0.2.7: Digital picture names (DONE, AWAITING REVIEW)
User request: two Digital recordings at the same time could write over each other's pictures, as both used fldigi's time-only names (`pic_2026-11-18_190031z.png`).

- A picture is named `pic_` + the VFO's recording template expanded at the time the picture was complete (the same fields as the Markdown file: `$t` = `digital`, `$f` the VFO frequency, `$n`, `$r`...), e.g. `pic_20261122-225707_digital_3955000Hz.png`, still in the `pictures/` subfolder and still `_2`... if taken. [Save] (F2) names the pictures it writes the same way.
- Code: `digital::service::RecNaming::{name, picture_stem}`; `write_picture` and `DigitalService::save` take the naming; `AppState::digital_naming` (`digital_app.rs`) builds it for both [Record] and [Save]; fldigi's `mfsk::markdown::picture_stem` is gone.

## 19. Version 0.3.0: Work Area pages and simpler side modules (DONE, REVIEWED)
User request (2026-11-24): the Work Area should serve any side module, not only the active VFO. Bookmarks and Schedule get a manager page there and their side panels shrink to choosing and activating; a new Report module (section 20) gets its statistics and log page there. The mockups were reviewed on 2026-11-24 (decisions in 19.7 and 20.7); the version becomes 0.3.0 (agreed).

### 19.1 Decisions
- The VFO owns the Work Area by default. A module takes it only when the user opens its page; focus moving to a module never switches the Work Area (decided: no switch on focus).
- `Esc` in a focused module page gives the Work Area back to the active VFO (decided). An `Esc` that has something to cancel first (an edit, a search, a menu, a form) only cancels that, as today; only an `Esc` with nothing left to cancel hands the Work Area back.
- Source has no page (simple enough). Radio has no button: its page is the VFO view, which `Esc` brings back.
- Side modules lose their add, edit and management controls; those move to the module's page (decided).

### 19.2 Opening a page
- Each module with a page shows a button on its first row: Bookmarks `[Manager]`, Schedule `[Manager]`, Report `[Report]`. ENTER or a click opens the page in the Work Area and moves focus to it; when the page is already open, it only moves focus there.
- Accelerator `o` ("open"): with a module focused, opens its page (6.1: the button is the visible control).
- Opening a page also shows it: in Tabs the second tab switches to it; in Split it takes the half where the VFO view was. The Spectrum tab or half is unchanged.
- The Work Area joins the Tab cycle after the last module, as the DSC view does now (14.6). The page's frame title names the owner, so it is always clear what is shown.
- The owner is not persisted: a start always shows the VFO view.
- F-keys (reserved for Work Area views, 14.6) act on the page while it is shown, and every page has a cli youckable function-key bar like the VFO views.

### 19.3 Layout mockups
Tabs layout, Bookmarks page open (terminal 160 columns; the tab row hint gains `Esc: back to Master`):
```
┌Global Controls - ShellSDR v0.3.0──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│[Stop] Vol[####----] [ ] Mute [Spectrum:ON] [Waterfall:ON] [Tabs]                                                                                   │
│Center: 0 004 207 500 Hz [Center] Theme:[Candy] Panel:[◂ 32 ▸] [?] Help                                                                              │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
┌[-] Source────────────────────┐ [Spectrum] [Bookmarks]                                          w: switch, l: Split, Esc: back to Master
│...                           │┌Bookmarks: manager─────────────────────────────────────────────────────────────────────────────────────────────────────┐
┌[-] Radio─────────────────────┐│ (page, 19.4)                                                                                                         │
│...                           ││                                                                                                                      │
┌[-] Bookmarks─────────────────┐│                                                                                                                      │
│[Manager]                     ││                                                                                                                      │
│...                           ││                                                                                                                      │
```
Split layout, Schedule page open: the page takes the VFO view's half, the spectrum and waterfall stay on the right:
```
┌[-] Schedule──────────────────┐┌Schedule: manager──────────────────────────────┐┌Bookmarks  UTC 18:04───────────────────────────┐
│[Manager]                     ││ (page, 19.5)                                  ││                                               │
│...                           ││                                               │┌Spectrum───────────────────────────────────────┐
```

### 19.4 Bookmarks: panel and Manager page
Panel (32 columns): choosing a list, finding, tuning, and `[Quick Add]` (decided): the active VFO into the list shown, named "New Bookmark" (as 9) without opening the editor. Gone from the panel: `[New] [Rename] [Del] [Hide]`, `[Add] [Edit] [Remove] [Tune]`, `Strip rows:`, `Sort:`.
```
┌[-] Bookmarks─────────────────┐
│[Manager] [Quick Add]         │
│List: Laivyba                 │
│Find(/):                      │
│› 2187.5 DSC 1                │
│  4207.5 DSC 2                │
│  6312   DSC 3                │
│  8414.5 DSC 4                │
│ 12577   DSC 5                │
│ 16804.5 DSC 6                │
│DSC  all day                  │
└──────────────────────────────┘
```
- Keys stay as in section 9: Left/Right switch the list, Up/Down move, ENTER tunes, `/` finds. `Delete` moves to the page.

Page (Tabs, full width):
```
 [Spectrum] [Bookmarks]                                                                      w: switch, l: Split, Esc: back to Master
┌Bookmarks: manager────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Lists              Strip │ Laivyba: 6 of 6       Sort: [Frequency]   Find(/):                                                       │
│ › Laivyba        6   ◉   │      kHz  Name                  Type     Settings              UTC        Days            Jobs          │
│   General       12   ◉   │ ›  2187.5 DSC 1                 DSC                            all day    every day       1             │
│   Radiogram      4   ○   │    4207.5 DSC 2                 DSC                            all day    every day       1             │
│   Weather fax    3   ◉   │    6312   DSC 3                 DSC                            all day    every day                     │
│                          │    8414.5 DSC 4                 DSC                            all day    every day       1             │
│ [New] [Rename]           │   12577   DSC 5                 DSC                            all day    every day       1             │
│ [Delete] [Hide]          │   16804.5 DSC 6                 DSC                            all day    every day                     │
│ Strip rows: [◂ 3 ▸]      │                                                                                                         │
│                          ├─────────────────────────────────────────────────────────────────────────────────────────────────────────│
│                          │ DSC 2  4 207 500 Hz  DSC  all day, every day                                                            │
│                          │ Geo: -                                                                                                  │
│                          │ Notes: -                                                                                                │
│                          │ Jobs: 1600 T DSC 2                                                                                      │
└──────────────────────────┴─────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 1Help 2Add 3Edit 4Copy 5Move 6Tune 7Find 8Delete 9Menu
```
- Left column: the lists (bookmark count, `◉` shown on the strip, `○` hidden) and the list controls; Right: the selected list as a table, then the selected bookmark in full (geo info, notes, the jobs that use it).
- F2 Add captures the active VFO into the selected list (as 9), F3 edits, F4 copies into another list, F5 moves, F6 tunes, F8 deletes (asks first). The editor form (9, 13.7) is drawn inside the page in place of the details below the table; no editing popups (decided). Confirmations (delete) stay small prompts.
- Narrow page (Split, about 60 columns): the table keeps kHz, Name and Type; the lists column becomes a `List:` row on top.

### 19.5 Schedule: panel and Manager page
Panel: the summary, the master switch and the job list. Gone from the panel: `Auto start/stop:`, `Local time:`, `[Add] [Edit] [Remove] [Off]/[On]`. ENTER on a job opens the page with that job selected.
```
┌[-] Schedule──────────────────┐
│[Manager]                     │
│REC Hamburg DDH3 until 18:20  │
│Scheduler: [On]               │
│ x 1330-1400 NB m Radiogram   │
│ x 1400      T    DSC 4       │
│›● 1800-1820 NB   Hamburg DDH3│
│ x 1820      T    DSC 1       │
│ x 2200-2300 NB   Pop Shop    │
│ x 2300      T    DSC 1       │
└──────────────────────────────┘
```
Page:
```
┌Schedule: manager─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ Scheduler: [On]   Auto start/stop: [x]   Local time: [ ]                                                   UTC 18:04   Local 20:04   │
│     Start End   Days       Repeat  Action  Bookmark              kHz      Type     State                                             │
│   x 1330  1400  Su         Weekly  NB m    Shortwave Radiogram   15770    Digital  Next Su 13:30                                     │
│   x 1400        every day  Weekly  T       DSC 4                 8414.5   DSC      Next 14:00 tomorrow                               │
│ › ● 1800  1820  every day  Weekly  NB      Hamburg DDH3          3855     FAX      REC until 18:20                                   │
│   x 1820        every day  Weekly  T       DSC 1                 2187.5   DSC      Next 18:20                                        │
│   x 2200  2300  Su         Weekly  NB      Pop Shop Radio        3955     Digital  Next Su 22:00                                     │
│   x 2300        every day  Weekly  T       DSC 1                 2187.5   DSC      Next 23:00                                        │
├──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────│
│ Today (UTC)  00    02    04    06    08    10    12    14    16    18    20    22    24                                              │
│              ·····························································│···█···············│··················                   │
│ Hamburg DDH3: 3 855 000 Hz FAX, Record Native + Baseband, every day 1800-1820                                                        │
│ Last runs: today 18:00 running (1 picture so far); yesterday 18:00-18:20 OK, 1 picture, 41 MB  (more in Report)                      │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 1Help 2Add 3Edit 4Copy 5On/Off 7Find 8Delete 9Menu
```
- The table is the job list of 9b with every column spelled out; the editor form (with the bookmark picker as a list in the page) is drawn in the lower part of the page, no popups.
- New on the page, built with the Report (decided, 20.4): a day timeline (Record windows as blocks, Tune jobs as ticks, now as `│`) and the selected job's last runs.

### 19.6 Code shape
- `ui::work_area`: `WorkOwner { Vfo, Bookmarks, Schedule, Report }` in `AppState` (not persisted); the layout keeps its Tabs/Split rules and asks the owner's page to render in the VFO slot.
- A small `Page` trait (render into a rect with hit areas, key, mouse, F-key bar, `wants_escape`) implemented by the three module pages; the VFO type views stay as they are and are wrapped as the `Vfo` page.
- The bookmark and job editors move from popups into page panes; their form models (`bookmarks::editor`, the schedule editor) are kept.

### 19.7 Review decisions (2026-11-24)
- The Schedule panel keeps the master switch `Scheduler: [On]`.
- The Bookmarks panel gets `[Quick Add]`.
- Editors are drawn inside the page; no editing popups.
- The Schedule timeline and last runs come with the Report (same version, built after it).

### 19.8 Schedule lists (amendment, 2026-11-24)
User request: like bookmarks, schedules come in lists, and only the active list runs, so different kinds of schedules can be planned and switched (a day of faxes only, then back to DSC tracking).

- Jobs belong to named lists; exactly one list is active and only its jobs are run by the scheduler (the master switch still stops everything). Job ids stay unique across lists.
- File: `<profile>.schedule.v3.json` with `min_version` 0.3.0 (`active`: the active list's name, `lists`: `[{name, jobs}]`). When it does not exist, the 0.2.x `<profile>.schedule.v2.json` (itself migrated from the 0.1.x file when need be) is read once: its switches carry over and its jobs become the list "Default", which is active. The v2 file is never written again, so a 0.2.x build keeps its own schedule. A v3 file from a newer build is read-only (13.9).
- Panel: a `List: <name>` row under `Scheduler:`, `●` when it is the active list, `(not active)` otherwise; Left/Right switch the list shown, ENTER or a click opens a list picker. The jobs shown are that list's; the summary line is always about the active list.
- Page: a lists column (`●` the active list, the job count) with `[New] [Rename]` and `[Delete] [Activate]`, beside the table from 120 columns, above it otherwise. The page field order is the switches row, the lists, the list buttons, the table. F6 Move moves the selected job to another list (a picker; it gets a fresh id and stays selected there).
- Activate makes the list shown the active one. When a job of the old list is recording, a confirmation names it; activating ends that occurrence (the planner no longer finds the job). With the scheduler on, the new list's current job is restored at once, as when the scheduler is switched on (9b). The active list cannot be deleted; deleting another asks first.
- A job of a list that is not active shows `List not active (active: <name>)` as its state (or `Off`), never a recording or overlap mark.

## 20. Version 0.3.0: Report module, statistics and the event log (DONE, REVIEWED)
User request (2026-11-24): a Report module with simple stats on the panel and daily, monthly and lifetime stats plus a log in its page, for mostly unattended operation.

### 20.1 Panel
```
┌[-] Report────────────────────┐
│[Report]                      │
│Up 3d 04:12  Source 2d 22:40  │
│Master: DSC 4207.5 for 1:12:33│
│Today                         │
│ DSC     412 msgs (2 distress)│
│ FAX       6 pictures         │
│ Digital  14 kB, 3 pictures   │
│ Rec       4 files, 1.2 GB    │
│Disk 112 GB free (~19 days)   │
│! 17:58 RSPduo lost (1 min)   │
└──────────────────────────────┘
```
- `Up`: since ShellSDR started; `Source`: the source's running time in this run.
- `Master: DSC 4207.5 for 1:12:33`: the active VFO, its type and frequency, and how long it has had that type and frequency.
- Today: only the types that did work today, plus recordings.
- Disk: free space in the recordings folders' file system and how long it lasts at today's recording rate.
- The last line: the newest warning or error of the log (20.4), `dim` once older than an hour; nothing when there is none.

### 20.2 Page
Tabs `[Today] [Month] [Lifetime] [Log]` on the page's first row (Left/Right, a click, or F3 to cycle); `◂ ▸` (or `,`/`.`) step to the previous or next day or month.

Today:
```
┌Report──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ [Today] [Month] [Lifetime] [Log]                                               ◂ Mon 2026-11-24 (local) ▸                          │
│ Running   ShellSDR 23:59   Source 21:12 (88%)   Recording 3:40   Starts 1   Unclean exits 0   Source failures 1                    │
│                                                                                                                                    │
│ Work done                        Live      File                                                                                    │
│   DSC messages                    412         0   distress 2, urgency 5, safety 380, routine 25; garbled 31                        │
│   FAX pictures                      6         2   12 400 lines; saved 1                                                            │
│   Digital text                  14 kB         0   3 pictures; RSID 4 (2 switches)                                                  │
│   Recordings                  4 files    1.2 GB   native 2 (0.1 GB), baseband 2 (1.1 GB)                                           │
│   Schedule                   9 jobs run           1 skipped (source stopped by user), 0 missed, 0 failed                           │
│                                                                                                                                    │
│ By station                   Tuned     Output                                                                                      │
│   DSC 2         4207.5 kHz   12:04     380 msgs                                                                                    │
│   Hamburg DDH3  3855 kHz      0:20     1 picture, 41 MB                                                                            │
│   (manual)      3955 kHz      1:10     2 kB, 1 picture                                                                             │
│                                                                                                                                    │
│ Activity by hour   00  02  04  06  08  10  12  14  16  18  20  22                                                                  │
│   DSC              ▁▁▁▂▂▃▅▆▇█▇▆▅▅▆▇▇▆▅▃▂▂▁▁                                                                                         │
│                                                                                                                                    │
│ Health    DSP 19% (max 43%)   Drops 0   CPU 58°C (max 71°C)   Throttled no   Memory 412 MB                                         │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 1Help 2Save 3Tab 4Prev 5Next 7Find 9Menu
```
Month: one row per day with the same counters, totals at the bottom:
```
│ [Today] [Month] [Lifetime] [Log]                                               ◂ 2026-11 ▸                                         │
│   Day     Up      Source   DSC    FAX   Digital      Rec           Jobs   Warnings                                                 │
│   11-01   24:00   23:58    1204   12    -            8 / 2.4 GB    18     0                                                        │
│   11-02   24:00   22:10     980   11    31 kB / 5    9 / 3.1 GB    19     2                                                        │
│   ...                                                                                                                              │
│   Total   23d     21d      ...                                                                                                     │
```
Lifetime: the same with one row per month, then totals since the first start (date shown).

Log:
```
│ [Today] [Month] [Lifetime] [Log]          Show: [All]   Local time: [x]   Find(/):             ◂ Mon 2026-11-24 ▸                  │
│ 18:20:01        SCHED    Ended Hamburg DDH3: 20 min, 1 picture, 41 MB                                                              │
│ 18:04:12  !     DSC      DISTRESS from 314001102 SITHONIA II at 4207.5 kHz                                                         │
│ 18:00:00        SCHED    Started Hamburg DDH3 (Record Native + Baseband, FAX)                                                      │
│ 17:59:11        SOURCE   RSPduo running again                                                                                      │
│ 17:58:30  !!    SOURCE   RSPduo lost (read error), retry in 10 s                                                                   │
│ 17:40:02        USER     Tuned bookmark DSC 2 (Laivyba)                                                                            │
│ 17:12:55        FAX      Picture done: 1810 lines, slant +7.19 ppm, Hamburg DDH3                                                   │
│ 16:30:00        DIGITAL  RSID: MFSK-32 -> MFSK-64 at 15770 kHz +1500                                                               │
```
- `Show:` All / Warnings / Errors / one category; `/` finds; Enter on an entry with a file (a recording, a picture, a saved message) shows its path.

### 20.3 What is counted
- Time: ShellSDR running, source running, recording (per output), each VFO type active, each station tuned (bookmark name, or `(manual)` + frequency).
- DSC: messages by category and format, garbled; FAX: pictures (complete or cut), lines, saved; Digital: text bytes, pictures, RSID detections and switches; Voice: nothing but time and recordings.
- Recordings: files and bytes per output kind; Schedule: runs, skips (with the reason), misses, failures.
- Live and File: decoding from a baseband file (including [Max], 17) is counted apart, so a fast replay does not inflate the day.
- Health samples once a minute: DSP load, drops/overflows, CPU temperature and the Pi's throttling flags (`/sys/class/thermal`, `get_throttled` when present), memory, free disk.

### 20.4 Event log
- One line per event: time (UTC or local, 20.5), level (info, `!` warning, `!!` error), category (`APP`, `SOURCE`, `USER`, `SCHED`, `REC`, `DSC`, `FAX`, `DIGITAL`, `HEALTH`), text.
- Events:
  - APP: started (version, profile), quit, unclean exit found at start, config or bookmarks read-only.
  - SOURCE: start, stop, failure, retry, back; file played (name, speed reached at [Max]).
  - USER: bookmark tuned (panel, strip or page), VFO type changed, manual recording started/stopped. Plain frequency tuning is not logged (too many events); the station time in 20.3 covers it.
  - SCHED: job started, ended (with its output), waiting, skipped (reason), missed, failed, Once job done, scheduler switched on/off.
  - REC: file opened, closed (length, size), write error.
  - DSC: distress and urgency messages (each), the rest only counted.
  - FAX: picture started, done or cut (lines, slant), saved. DIGITAL: RSID found or switched, picture done.
  - HEALTH: thresholds crossed (disk below 2 GB or 1 day, CPU over 80°C, throttling, DSP over 90% for a minute, drops), and back to normal.
- Kept in memory for the page (today) and appended to a daily text file.

### 20.5 Storage
- Per instance (two RSPduo instances each have their own), in the report folder: `<profile>_report.v1.json` (counters per day and per month; written every minute and at quit, atomically), `<profile>_log_YYYY-MM-DD.log` (plain text, one event per line) and `<profile>_report_YYYY-MM-DD.md` (the midnight report, 20.6).
- Only the folder is configured; the file names are fixed and prefixed by the profile (decided 2026-11-24): `<profile>` is the `--config` name (`default` without it, so `default_report.v1.json`), or the file stem of `--config-path`. Several profiles can share one folder (e.g. on a NAS) without clashing; Move (below) moves only this profile's files.
- Report folder (decided): a `Dir:` setting on the Report page (Menu, Settings), default `~/.local/state/ShellSDR/`, so reports and logs can live on a NAS instead of the Pi's SD card. Changing it asks: `Move the existing reports and logs to the new folder?` with `[Move] [Start fresh] [Cancel]`. Move copies every report and log file of this profile, checks it and then deletes the old ones; Start fresh leaves the old folder alone and starts empty counters (lifetime included) in the new one. An unreachable folder (NAS down) keeps counting in memory, logs a warning, retries every minute and writes the backlog when it comes back.
- Retention (decided: configurable, on the same Settings): daily counters 400 days, logs 90 days, monthly counters forever, by default; 0 keeps forever.
- Local time (decided: selectable like the Schedule's, default on): one `Local time: [x]` checkbox, on the Log tab row and in the Settings, persisted per profile. It sets both the day boundary (which day an event or a count belongs to) and the times shown in the log, the Today and Month pages and the midnight report. Local follows the system's UTC offset (as the DSC day files, 14.5 and the Schedule, 9b); a DST change moves the shown times, not the events.
- The log file lines carry the time in the chosen zone with its offset (`2026-11-24T20:04:12+02:00`, or `...Z` in UTC), so a file stays unambiguous across a switch or a DST change; the page reads either form. `SHELLSDR_TIME` for the hook (20.6) uses the same form.
- An unclean exit is found by a `running` flag in the report file that a clean quit clears.

### 20.6 Unattended operation (decided 2026-11-24: all in 0.3.0)
- Midnight report: at the day boundary (20.5), the finished day's Today page as Markdown, `<profile>_report_YYYY-MM-DD.md` in the report folder, with links to the day's pictures and recordings.
- Event hook: a user command run on chosen events, the event given as arguments and environment variables (`SHELLSDR_LEVEL`, `SHELLSDR_CATEGORY`, `SHELLSDR_TEXT`, `SHELLSDR_TIME`, `SHELLSDR_FILE`). Chosen per category and level on the Settings (default: distress and urgency, errors, disk low, job failed, watchdog). Run detached with a 10 s timeout, at most one per event and a rate limit (default 10 a minute; the rest summarised); a failing hook is logged, never retried. Example for Simplepush: `curl -s --data-urlencode "key=XXXXXX" --data-urlencode "title=ShellSDR $SHELLSDR_CATEGORY" --data-urlencode "msg=$SHELLSDR_TEXT" https://api.simplepush.io/send`.
- Watchdog: no samples for 10 s while the source runs counts as a source failure: logged, the source restarted (the retry rules of 9b), and a hook event.
- Nothing decoded: a warning when a job of a decoding type (DSC, FAX, Digital) ends without output (`Hamburg DDH3: no picture in 20 min`); also for a manual station tuned for an hour with nothing decoded.
- Noise floor: the median spectrum level in the VFO's channel while a station is tuned, per station per day; the Month tab shows it per station as a trend (sparkline), and a drop or rise of 6 dB against the 7-day median is a warning.
- Clock check: at start and every hour, the system clock's sync state (`adjtimex` status; `timedatectl` is not needed); unsynchronised is a warning (schedules and DSC times depend on it), synchronised again is logged.
- Today's pictures: a gallery row on the Today tab (fax and Digital pictures as thumbnails, half blocks or Kitty), ENTER shows the file's path and opens the picture in the type's view when it is still in memory.

### 20.7 Review decisions (2026-11-24)
- All of 20.6 is in 0.3.0.
- Local time or UTC selectable for the day boundary and the log times (one checkbox, as in the Schedule), default Local; retention configurable (20.5).
- Report folder settable, with move or start fresh (20.5).

### 20.8 Steps for sections 19 and 20 (strict TDD, each reviewed before the next)
- [x] 0.3-1: Work Area owner: `WorkOwner` in `AppState`, the owner's page in the VFO slot (Tabs and Split), the tab row and titles, `[Manager]`/`[Report]` buttons, `o`, `Esc` back to the VFO, the Tab cycle, F-keys to the page; version `0.3.0`. DONE. As built: `ui::page` (`WorkOwner`, `render_page`, a placeholder body until 0.3-2/0.3-3), `page_app.rs` (`open_page`, `close_page`, `page_key`, `page_fkey`, `set_work_slot_visible`: the VFO view counts as hidden under a page, so its spectra and Kitty pictures stop); `[Manager]` is the first row of Bookmarks and Schedule (`BookmarkField::Manager`, `SchedField::Manager`, ENTER or a click); `Esc` returns the focus to the page's module; `[Report]` comes with the module (0.3-5).
- [x] 0.3-2: Bookmarks Manager page: lists pane, table, details, F-key bar, in-page editor; the panel without the management rows, `[Quick Add]`. DONE. As built: `ui::bookmark_page` (wide: the lists column; under 90 columns: up to 4 list rows, the buttons and the strip stepper above the table, whose columns drop from the right down to kHz, Name, Type); `BookmarkManager` gains the page's field cursor (`PageField`: lists, list buttons, strip rows, sort, find, table; Up/Down walk them, the table wraps to the lists), `PanelButton`, `transfer` (copy/move, renamed `Name (1)`... on a clash; a move follows the bookmark), `Request::{QuickAdd, Copy, Move}`; the editor is drawn under the table (the table keeps at least two rows), the list picker for Copy/Move and the name prompts stay small popups; Quick Add shows no notice (the new row is selected); F1 is the help; no F9 menu (nothing left for it). The panel's Delete key is gone (F8 / Delete on the page).
- [x] 0.3-3: Schedule Manager page: table, settings row, in-page editor and bookmark picker; the panel without the management rows. DONE. As built: `ui::schedule_page` (the switches row with the UTC and local clocks; the table with Start, End, Days, Repeat, Do (the action letters with `m` and `!`), Bookmark, kHz, Type, State; under 100 columns Start, End, Do, Bookmark, State; three details lines: tuning, action, days and window; bookmark and repeat; state); `ScheduleModule` keeps `[Manager]`, the master switch and the jobs on the panel (`SchedField`), the page has `PageField` (switches row, table) and `SchedSetting`; ENTER on a panel job opens the page on it; F-keys 1 Help, 2 Add, 3 Edit, 4 Copy (the editor on a copy, Apply adds a new job), 5 On/Off, 8 Delete (no Find: job lists are short); the panel keeps one line, the selected job's state, under the jobs; any popup (the bookmark picker, a confirmation) opens centred on the page while a page is shown. Then schedule lists (19.8): `ScheduleModule` holds the lists (the active list's jobs live in `scheduler.jobs`, so the planner is unchanged), `view`/`active`, `activate_view`, `move_job`, the list requests and prompts; `store::{ScheduleDoc (v3), ScheduleDocV2, load, load_v2, v2_schedule_path}`.
- [x] 0.3-4: Report core: counters (20.3) per day and month, the event log (20.4), storage in the report folder with the profile prefix (20.5), Local time, retention, unclean exit; events from the app, the scheduler, the source, the recorder and the decoders. DONE. As built: `report::model` (named counters per `Period` (day `YYYY-MM-DD`, month `YYYY-MM`) with per-station counters and hourly rows for DSC messages, fax pictures and Digital bytes; `Clock` (local or UTC, stamps with the offset), `Event` lines `<stamp> <-|!|!!> <CATEGORY> <text>[ | <file>]` that read back), `report::store` (`<profile>_report.v1.json`, `<profile>_log_YYYY-MM-DD.log`, `<profile>_report_YYYY-MM-DD.md`; pruning, the profile's files), `report::Report` (counts time each tick, ignoring gaps over 2 min; saves every minute and at quit; today's log read back at start; `running` flag for an unclean exit; a folder that cannot be written keeps counts and lines in memory, logs the outage once and its end, and a folder unreadable at start is merged into, never overwritten, when it comes back), `report_app` (open/close, `report_event`, `report_count`, `report_output` (decoder output; `file.` + the key during a file playback, else also to the station), the tick: source, recording, VFO type, scheduler switch, active list changes, scheduled recordings ended or stopped early (skipped), DSC calls (distress and urgency logged), finished fax pictures, Digital text, pictures and RSIDs). `[report]` in the profile TOML: `dir` (default `~/.local/state/ShellSDR`), `local_time`, `keep_days` 400, `keep_log_days` 90, `keep_months` 0 (forever). Events from the run loop (source started, failed, stopped, a file played to its end), recordings closed (size, dropped frames), bookmarks tuned by the user, scheduler starts, fax pictures saved. Station = the bookmark with the VFO's frequency and type, else `(manual) <kHz> kHz`. Not yet: missed jobs, health and the watchdog (0.3-7).
- [x] 0.3-5: Report panel and page: Today, Month, Lifetime, Log (Show, Find, Local time). DONE, REVIEWED. As built: the Report module after Schedule (`Focus::Report`, folded title `Report [Up …]`): `[Report]`, `Up … Source …` (this run), `<VFO>: <type> <kHz> for h:mm:ss`, Today's work lines (only what did work, else `nothing decoded yet`), the newest warning or error (accent for an hour, then dim); `ui::report_page`: the tab row with the period and `◂ ▸` (clickable), Today (running times, starts, unclean exits, source failures; work done live and from files with the DSC severities, fax lines and saves, Digital pictures and RSIDs, recordings, schedule runs and skips; time per VFO type; stations by time with their output; activity by hour as bars), Month (a row per day and the total), Lifetime (a row per month and the total, since the first start), Log (newest first, `Show:` filter, `Local time:`; earlier days read from their files); keys F3 tab, F4/F5 or Left/Right older/newer, Up/Down/PgUp/PgDn scroll, F6 Show, F7 Local time / UTC (persisted). Durations `h:mm:ss`, `3d 04:12` from a day. Not yet: the Log's Find, the disk line (0.3-7), the Settings (0.3-6).
- [x] 0.3-6: Report settings: folder (Move / Start fresh / Cancel, NAS down), retention. DONE. As built: a fifth tab `[Settings]` on the Report page (rows Folder, Local time, Daily counters, Logs, Monthly counters; Up/Down, ENTER, Left/Right steps of 30 days, 10 days, 12 months, 0 = forever; below them the files' paths, the number of logs and whether the folder is writable). A new folder (a small prompt) asks `Cancel / Move / Start fresh`: Move saves everything, copies this profile's files, checks their sizes, then deletes the old ones (a failed or incomplete copy undoes the copies and changes nothing; a folder that already holds this profile's reports is refused with a hint to use Start fresh); Start fresh leaves the old folder alone and counts from zero in the new one, or carries on (merges into) this profile's report already there; a new folder that cannot be reached counts in memory until it can (0.3-4). Retention changes prune at once. `Report::{relocate, set_retention}`.
- [x] 0.3-7: Health and watchdog: DSP load, drops, CPU temperature, throttling, memory, disk with days left, clock sync, the source watchdog, nothing decoded, noise floor per station. DONE. As built: `report::health` (readers: `/sys/class/thermal/thermal_zone0/temp`, the Pi firmware's `get_throttled` (bits 0..3: now), `statvfs` of the active type's recordings folder, `adjtimex` (`TIME_ERROR` = not synchronised), `/proc/self/statm`; `HealthMonitor` logs each change once: disk under 2 GB or under a day at today's recording rate, CPU over 80°C (back under 75°C), throttling, DSP over 90% for a minute, IQ blocks dropped in a minute, the clock not synchronised (at the first sample, then hourly), and each return to normal). A health sample once a minute; the day and month keep the DSP, temperature and memory maxima (`Period::maxima`) and count drops; the panel shows `Disk <free> (~<days> days)`, Today a Health line. Watchdog: a SoapySDR source with no samples for 10 s (`Engine::samples`) is logged as an error and restarted (counted as a source failure and a watchdog restart); a file is not watched (it may pause at [Max]). Nothing decoded: a scheduled job of a decoding type (DSC, FAX, Digital) that ends without output, and a decoding station tuned for an hour without output (once per stay), are warnings. Noise floor: every 10 s, the median of the spectrum frame (the whole span, not only the channel: a busy channel's median is the signal) is added to the station's day (mean shown on Today by station, a bar per day on Month); a day 6 dB or more off the median of the station's last 7 days (each with 5 minutes of readings, at least 3 days) is a warning, once per station and day. Only while the spectrum is on screen (it has no frames otherwise).
- [x] 0.3-8: Event hook, midnight report, today's pictures. DONE. As built: `report::hook` (`sh -c <command> shellsdr-hook <level> <CATEGORY> <text>`, environment `SHELLSDR_LEVEL`, `SHELLSDR_CATEGORY`, `SHELLSDR_TEXT`, `SHELLSDR_TIME` (the log's stamp), `SHELLSDR_FILE`, `SHELLSDR_PROFILE`; detached, killed after 10 s; at most 10 a minute, the rest counted and logged; a failure (exit status and the last line of stderr) is logged as a warning and never sent through the hook); `[report]` gains `hook`, `hook_errors` (on), `hook_warnings` (off), `hook_dsc` (distress and urgency, on), `hook_health` (on); the Settings tab gains the command (a prompt), the four switches and `[Test the hook]`. `report::markdown`: at the day change the finished day is written as `<profile>_report_YYYY-MM-DD.md` (running times, work done live and from files, stations with output and noise floor, health, the pictures as Markdown images, recordings, warnings and errors); a day missed (ShellSDR not running at midnight) is written at the next start. Today's pictures: fax pictures recorded to a file (`FaxService::recorded_file`) and Digital pictures written (`DigitalService::take_new_files`) are logged with their file; the Today tab lists the day's pictures (time, type, path), as a list for now, not thumbnails.
- [x] 0.3-9: Schedule timeline and last runs. DONE. As built: under the job table, `Today (UTC|local)` with hour labels and one bar row (1, 2 or 4 cells an hour as the width allows): Record windows `█` (the selected job's in the accent colour), Tune job starts `╷`, free time `·`, now reversed; it shows the list shown, active or not (for planning). The details gain `Last runs: today 18:00 running; 11-23 18:00-18:20; 11-22 18:00-18:05 skipped nothing decoded` (newest first, up to 4), paired from the Report's log: `Started <job> (`, `Ended <job> (`, `<job> stopped before its end`, `<job>: nothing decoded`; today's events from memory, the 7 days before from their log files, read once a day (`report_app::{job_runs, sched_last_runs}`).
- [x] 0.3-10: Help, README, HANDOVER; packages on request. Done: README `What is new in 0.3.0`, HANDOVER (module map, resume point), the help's page sections; the arm64 package built.

## 21. Network remote and a graphical window (21.1, 21.2: DONE in 0.4.0, AWAITING REVIEW; 21.3: PLANNED)
Evaluated 2026-11-23. Order: section 19 and 20 first (the pages are what both of these render), then 21.1, 21.2, 21.3.

### 21.1 Commands and state
- Every UI action becomes a `Command` value applied to the application state, and the state splits into the model (radio, VFOs, bookmarks, schedule, report) and what one screen shows (focus, scroll, popups). Needed by the remote, and it makes the GUI cleaner.
- As built (0.4.0): done at the engine seam, which is what the remote needs (the server runs the engine, the client keeps the whole application). `engine::Command` is plain data (serde) and covers every control the UI gives the engine (tuning, the VFO's type and settings, gains, AGC, antenna, analog bandwidth, decimation, seek, volume, the VFO view and FFT switches, the file's Max and hold); recording taps hold local writers and go through `EngineHandle::record`. The engine answers with `engine::Event`s (Spectrum, View, Data, Meter, Stats, Repaint, Failed, Notice) from `EngineHandle::poll`. The run loop drives a `Box<dyn EngineHandle>`, the local engine or a remote one, and `engine_app::EnginePush` turns state changes into commands. The split of the UI actions themselves into commands on a model is left for the egui window (21.3), where a second front end needs it.

### 21.2 Network remote
- A server ShellSDR runs the SDR, the DSP, the decoders and the recordings; a client ShellSDR shows it (like SDRconnect's compressed-audio mode).
- Streams: audio (compressed, bitrate chosen by the user), spectrum rows (dB quantized to 8 bits, delta coded and compressed; the client asks for the bin count and span, so a graphical client can ask for 2048 bins or more), decoder output as events (DSC messages, fax lines, Digital text and pictures), state changes; client to server: commands.
- Codec: Opus (decided 2026-11-24) instead of AAC: libopus is BSD and packaged on Debian and Raspberry Pi OS; the usual AAC encoder, FDK-AAC, is not GPL compatible.
- Recordings and file playback stay on the server; security: localhost by default, a token or an SSH tunnel for remote use.
- As built (0.4.0):
  - The server is headless: `shellsdr --serve [ADDR]` (default `127.0.0.1:7373`), `--token SECRET`, `--serve-file WAV` (repeatable: a baseband offered as an input with a fixed center, looped; for testing and for sharing a recording). One client at a time (another is refused as busy); `Ctrl+C` / SIGTERM stop the engine and release the device. A warning is printed when it listens beyond the machine without a token.
  - The client is the Source module's third kind, **Network**: Server (`host:port`), its status line (the server's host, version and codec, or the error), Token, Audio (Opus 16/24/32/48/64/96 kb/s or PCM), Device (the server's inputs), then the device's rows as for SoapySDR (rate, decimation, bandwidth, antenna, gains, AGC). The local and the server's device lists swap in and out of the same fields. Settings are saved per input under `net:<addr>|<args>`; a profile with the Network kind selected is saved with `min_version` 0.4.0.
  - Divergence from the plan: the decoders' products are made on the client. The server runs the source, the DSP and the VFO's processor (the DSC decoder, the FAX audio, the Digital channel IQ); the client receives the decoded data (`DataItem`) and runs the DSC service, the fax and Digital decoders, recordings, bookmarks, the schedule and the Report as with a local radio. So the client's schedule drives the remote receiver; a server that records on its own while no client is connected is a later step (it needs the application state on the server).
  - Recordings: the native (audio) recording is written from the received audio (Opus at the chosen rate, or PCM); a baseband recording is not offered over the network (the IQ stays on the server).
  - Wire: TCP frames `[u32 length][u8 kind][payload]`; control messages are JSON (`Hello`/`Welcome`/`Refused`, `ListInputs`/`Inputs`, `Probe`/`Caps`, `Start`/`Started`, `Cmd`, `Stats`, `Meter`, `Notice`, `Stop`/`Stopped`); spectrum rows are dB in a byte (0.5 dB steps, wider when a row spans more than 127 dB), delta coded along the frequency and deflated (flate2, already a dependency, instead of zstd), reduced to the 1024 bins the client asks for with the peak of each group kept; the VFO view and the decoded data are binary. Audio: 20 ms packets; the client holds 100 ms before playing (and again after running dry) and drops audio when more than 0.5 s waits. Volume is applied on the client.
  - Opus is the system libopus loaded at run time (`remote::opus`, like libfax): no build dependency; without it on either end the audio is 16-bit PCM.
  - Not done: TLS (use an SSH tunnel), several clients at once, reconnecting automatically after a lost connection (the client stops and reports it; the Report counts a source failure).

- Raspberry Pi hardware (checked 2026-11-24, Pi 3: BCM2837, Cortex-A53; Pi 4: BCM2711, Cortex-A72):
  - No audio encoder or general compression engine in hardware; Opus and zstd run on the CPU with NEON (a mono Opus stream costs a few percent of one core, to be measured).
  - The VideoCore H.264 encoder (`bcm2835-codec`, V4L2 `/dev/video11`) is usable from 64-bit on the Pi 4 under Bookworm, and should be on the Pi 3 (not verified; Trixie not verified; the Pi 5 has no H.264 encoder). Not used: the waterfall rows are bytes of dB values that delta + zstd shrink to a few kB/s, and a lossy video would smear exactly the weak signals the waterfall is for, add latency, and tie the remote to one board.
  - Neither SoC has the ARMv8 crypto extensions (no AES instructions), so TLS should prefer ChaCha20-Poly1305 (rustls does this when the client asks), or the remote should run over an SSH tunnel with the same cipher.
  - The FFTs already use NEON (`rustfft` on aarch64); the server's spectrum work is the same as today's.
  - The Pi 4's gigabit Ethernet and USB 3 make it a comfortable server; the Pi 3's 100 Mbit (shared with USB 2) is ample for these streams (well under 1 Mbit/s).

### 21.2.1 Steps (0.4.0)
- [x] 0.4-1 The engine's commands and events (`EngineHandle`, `Command` as data, `Event`, `engine_app::EnginePush`).
- [x] 0.4-2 The wire format and the codecs (`remote::wire`, `remote::opus`).
- [x] 0.4-3 The server and the remote engine (`remote::server`, `remote::client::RemoteEngine`), with a loopback test of a served file heard through Opus.
- [x] 0.4-4 The Network source (Source module, persistence, help, README), tried between two instances.

### 21.3 Graphical window (egui)
- Replaced by section 22: the egui window is a viewer of a sharing ShellSDR (22.6), painting the shared cell grid itself (no `egui_ratatui`: its 2.x needs ratatui 0.30 / egui 0.34, ShellSDR is on ratatui 0.29) with the spectrum, waterfall and pictures drawn at pixel resolution over their rectangles.

## 22. Version 0.4.1: a remote window into a running ShellSDR (DONE, AWAITING REVIEW)
Decided 2026-11-24. A ShellSDR running with its TUI (e.g. on the Pi in tmux) can be shared: another ShellSDR, in a terminal or as an egui window, shows its screen and works it like a remote desktop, and hears its audio. Everything keeps running on the sharing machine (source, DSP, decoders, recordings, schedule, Report, files); the viewer is only a window. The headless server and the Network source of 0.4.0 stay as built (21.2): they are for "this machine is the radio, the antenna is elsewhere".

### 22.1 Principles
- Everything is switched in the UI and saved in the profile; command-line switches only start a mode without a TUI: `--serve` (headless, 21.2, keeps its switches) and `--gui` (the egui viewer). No `--share` or `--attach` switch.
- One protocol for both viewers: a neutral cell grid (no ratatui types on the wire) plus overlays, tagged rectangles of the grid that a graphical viewer draws at pixel resolution; a terminal viewer ignores the overlays.
- Like a remote desktop: one focus, one cursor, one set of popups and pages; a key or click from either side acts on the same UI. Each side's screen is laid out for its own size.
- One viewer at a time (a second is refused as busy); the sharing side always stays usable.

### 22.2 Sharing (the sharing machine, Source module)
- New rows at the end of the Source module, for every kind (SoapySDR, File, Network): `Share: [Off]/[On]`, `Listen: 127.0.0.1:7373` (ENTER: prompt, `0.0.0.0` for every interface), `Token: (none)/••••••` (ENTER: prompt), and a status line (`waiting`, `viewer 192.168.1.20 (terminal, 120x40)`, or the error, e.g. the port in use). A `[Disconnect]` button while a viewer is connected.
- Saved in the profile (`[share]`: on, listen, token); a profile with Share on starts sharing at startup.
- On without a token on an address beyond the machine: a warning in the status line (as the headless server prints).
- `Mute while viewed: [ ]` (shown while a viewer is connected, saved in the profile): the sharing side's own audio output is muted while a viewer is connected; the viewer hears it either way (decided 2026-11-24).
- The Report logs viewers connecting, leaving and refused (category APP), and counts them.
- The status bar shows `Shared: viewer <ip>` while connected.
- A sharing ShellSDR does not serve its engine to a Network source (its own UI drives it); a Network source that connects to it is offered the window instead (22.3).

### 22.3 Viewing from the TUI (the Network source)
- The Network source's query learns what the server is: a headless engine server (21.2, as now) or a sharing ShellSDR. For a sharing ShellSDR the Network view shows its host and version, the Token, Audio, and `[Open window]` instead of the device rows.
- `[Open window]` turns the local screen into the remote's window: the local application keeps running underneath (its own engine stops being shown, the local radio is left as it is; Stop it first to free the device if needed), every key and mouse event goes to the sharing side, the remote screen is drawn at the local terminal's size.
- Leaving the window: `Ctrl+]` (like telnet; shown in a one-line hint at the top for the first seconds and in the help; 0.4.3: a permanent bar with `[Back]` and `F12`, see 24) or the sharing side's `[Disconnect]`, or the connection lost (then a notice). Back in the local UI, nothing changed.
- The window's audio is played by the viewer with its own volume (`Ctrl+Up/Down` in the window, as they cannot go to the sharing side's Global bar; 0 % is mute); the Network view's Audio row sets the Opus rate. The sharing side keeps playing its own audio.

### 22.4 The protocol (on the 21.2 framing)
- Handshake: `Hello` gains `want: "engine" | "window"`; `Welcome` gains `kind: "engine" | "window"`. Window session: the viewer sends `Window { cols, rows, overlays: bool, bins, codec, bitrate }`, and `Resize { cols, rows }` when its size changes.
- Screen: `Cells` frames: the cells that changed since the last frame (row, column run, then per cell the symbol, foreground, background and modifiers as RGB / indexed colours), deflated; a full frame on start and after a resize. At most 20 frames a second, only when something changed. Kitty picture placements are not sent: pictures go as overlays.
- Overlays (only when the viewer asked for them): `Overlays` frames list the rectangles of the frame: `Spectrum { rect, center_hz, span_hz, db_min, db_max }`, `Waterfall { rect, center_hz, span_hz, db_min, db_max, gradient }`, `VfoView { rect, ... }`, `Picture { rect, id, crop }`; the data come as the 21.2 spectrum rows (at the viewer's bin count, every FFT frame, so the waterfall scrolls at the FFT rate) and `Picture { id, png }` once per picture (again when it grows, at most every 2 s).
- Input: `Key { code, modifiers }` and `Mouse { kind, button, col, row, modifiers }` in cell coordinates, applied on the sharing side exactly as local input (`translate_key`, the viewer screen's own hit map).
- Audio: the active VFO's audio before the volume, Opus or PCM as in 21.2.

### 22.5 The sharing side inside
- The application state renders a second time per loop for the viewer, into an off-screen ratatui buffer of the viewer's size, with its own hit map (`render_with_hits` already takes a frame; the parts of the state that depend on the screen size (`screen_width`, scroll reconciliation, the Work Area regions, the FAX visible lines) are computed per screen, so the two sizes do not fight).
- The diff of that buffer against the previous one becomes the `Cells` frame; overlays are recorded by the widgets that draw the spectrum, waterfall, VFO view and pictures (like the hit map).
- The engine gets an audio tap beside its output (the local audio plays on), feeding the session's encoder.
- Input from the viewer goes into the same queue as local input; a popup or prompt opened by either side is on both screens.

### 22.6 The egui viewer (`shellsdr --gui`)
- A cargo feature `gui` (off by default; `tools/dev build` turns it on for both packages). The window loads X11 / Wayland and EGL / GL only when it opens (no link-time libraries), so the package runs without a desktop; `--gui` first checks for a display and those libraries and, if missing, says what to install and exits (the release build aborts on a panic, so nothing is left to fail deep inside winit). Decided 2026-11-24: arm64 too.
- `--gui` opens a window with a connect screen: server, token, audio rate, recent servers (saved in the profile's `[network]` like the Network source; `--config` selects the profile as usual). Then the shared screen fills the window.
- The cell grid painted by the viewer in a monospace font (cell size from the font and a zoom setting, `Ctrl+=`/`Ctrl+-`); the window size gives the cols and rows sent to the server.
- Overlays: the spectrum as a line, the waterfall as a texture scrolling at the FFT rate (rows at the window's pixel width, e.g. 2048 bins), the VFO view's spectra, pictures as textures; all exactly over their rectangles. A click or wheel inside the spectrum / waterfall is sent as the cell under it (the sharing side tunes as for a terminal click); a sub-cell precise click-to-tune is a later refinement.
- Keys and mouse mapped to the same key and mouse events as a terminal; the audio as in 22.3.

### 22.7 Not in this version
- Several viewers, per-viewer focus (a state split, 21.1), viewing a headless server (it has no UI), encryption (SSH tunnel as in 21.2), Kitty pictures in a terminal viewer (half blocks are drawn).

### 22.8 Steps
- [x] 0.4.1-1 Per-screen rendering: the size-dependent state per screen, render into an off-screen buffer with its own hit map; the cell diff and its encoding (tests: a diff applies to give the same buffer). As built: `ui::screen` (`ScreenLocal` swapped into the state around a screen's `prepare`, render and input: width, the left panel's scroll and viewport, what is visible, the FAX and Digital views' rows, Kitty off remotely; `render_offscreen` into a `TestBackend` buffer with its own hit map; `handle_event` the one input path), `remote::cells` (`Grid`, `CellDiff` in runs ending after 4 unchanged cells, deflated; to and from ratatui buffers).
- [x] 0.4.1-2 Sharing in the Source module (rows, profile, status, Report), the window session server (handshake, cells, input, resize, audio tap). As built: `remote::share::ShareServer` (listener thread, one viewer, `Started { codec }` answers the `Window` request, cells only when the viewer took the last ones), `ui::share_loop::ShareLoop` (follows the Share rows, applies the viewer's input on its `ScreenLocal`, renders at most every 50 ms, a viewer cannot quit the program), `EngineHandle::set_audio_tee` (local and remote engines), `AppState::remote_shows` (the engine runs the spectrum and the VFO view for either screen), `[share]` in the profile, `Shared: viewer <ip>` in the status bar, APP events in the Report.
- [x] 0.4.1-3 The Network source's `[Open window]`: the terminal viewer, `Ctrl+]`, audio; tried between two instances. As built: `remote::viewer::Viewer` (shared by the egui viewer), `ui::viewer_loop::TermViewer`; the window's volume is `Ctrl+Up`/`Ctrl+Down` in 10 % steps (a `Ctrl+M` cannot be told from ENTER in a terminal); a hint line for 4 s. `Ctrl+]` also arrives as `Ctrl+5` from most terminals. Tried in tmux: the sharer (File source, Share on from the profile) and a viewer at 120x40 then 150x45: the screen at the viewer's size, colours identical, `w` from the viewer switched both screens, the resize reached the sharer, `Ctrl+]` back, the Report's APP events; a PCM probe received 2.98 s of audio in 3 s and 13 frames a second.
- [x] 0.4.1-4 Overlays: recorded by the widgets, sent with the spectrum rows and pictures. As built for the spectrum and the waterfall: `wire::Overlay::{Spectrum, Waterfall}` (the plot rectangles right of the dB labels, the span, the dB window, the trace / center / marker colours and the waterfall gradient) recorded in the frame's `HitMap`, sent as `Msg::Overlays` when they change and empty while a popup, prompt, help, editor or menu is open; every FFT frame goes to a graphical viewer at its bin count (`ShareLoop::on_spectrum`, dropped when the viewer is behind). Not yet: the VFO view's spectra and the FAX / Digital pictures as overlays (their cells are drawn meanwhile).
- [x] 0.4.1-5 The egui viewer (`--gui`, feature `gui`): connect screen, grid, overlays, pixel waterfall, input, audio. As built: `gui/` with eframe 0.37 (glow renderer, no wgpu), DejaVu Sans Mono when installed; `--gui [SERVER]` connects at once (still the GUI mode); recent servers, the audio rate, the text size and the volume in `gui.toml` beside the profiles (not in a profile, which a TUI instance owns); braille cells drawn as dots (fonts lack them or misalign them); the cells painted in runs of one colour per row; a top bar with the server, Volume, Mute, Text size and Disconnect; keys and text from egui mapped to terminal keys, clicks and the wheel to cells. Tried on the desktop with screenshots: the shared screen, the pixel spectrum trace with the center and VFO lines, the waterfall texture growing at the FFT rate, the VFO view's braille trace.
- [x] 0.4.1-6 Help, README, HANDOVER; x64 and arm64 packages. As built: `shellsdr-0.4.1-x64.tar.gz` (6.4 MB) and `shellsdr-0.4.1-arm64.tar.gz` (5.5 MB), both with the window; no new libraries needed (the window loads GL and X11 or Wayland at run time); both cross-built, not run. `--gui` without a display, and with an unreachable one, ends with a message (tried here).

### 22.9 Decisions (review 2026-11-24)
1. `Ctrl+]` leaves the window (may change later).
2. `[Open window]` in the Network source; the window replaces the local screen until `Ctrl+]`.
3. The sharing side plays its audio while viewed, with a `Mute while viewed` switch (22.2).
4. Version 0.4.1.

## 23. Version 0.4.2: ShellSDR in a graphical window (DONE, AWAITING REVIEW)
Decided 2026-11-24: the GUI is the same ShellSDR as in a terminal, not only a viewer.
- `--gui` runs the normal application (profile, schedule, Report, devices, sharing, the Network source and its `[Open window]`) on a thread; `ui::front::Front` is the run loop's front end (`TermFront`: crossterm; `gui::GuiFront`: input from the window, frames into a `TestBackend` of the window's size, overlays and spectrum rows handed over). The window paints the cells (braille as dots) and the overlays at pixel resolution, and sends keys (text and IME commits as characters), clicks (a release goes to the cell of its press) and the wheel. Closing the window is a quit request (`q`); `Ctrl+=` / `Ctrl+-` / `Ctrl+0` set the text size; `gui.toml` keeps the window size and the text size. The connect screen of 0.4.1 is gone.
- Overlay streams: `STREAM_MAIN` (the wideband spectrum and waterfall), `STREAM_CHANNEL` (the Voice view's channel spectrum and waterfall, with the filter edges dashed) and `STREAM_AUDIO` (the audio waterfall); `Overlay::{Spectrum, Waterfall}` carry `stream` and `edges_hz`, rows travel as `Frame::Rows { stream, row }` (also to a remote graphical viewer); a waterfall whose span changes (a new zoom) starts over.
- While a popup or dialog is open, no overlays are drawn (the cells show the plots); covering only the dialog's area was considered and left out (user, 2026-11-24).
- Both packages have it; without a display or its libraries `--gui` says so and exits.
- Also fixed: the headless server refused a client as busy right after that client's query (the Network source's query then Start); a new connection now waits up to 1 s for the previous session to end.
- Tried: the user with a real keyboard (Space, `w`, `?`, Esc, Tab, arrows, clicks, the wheel, the text size, Stop and `q`, closing the window while running), the Voice view in pixels.

## 24. Version 0.4.3: the remote window's bar and the compression (DONE, AWAITING REVIEW)
Asked 2026-11-25 after trying 0.4.2: leaving a window, the audio here, Start on a sharing server, and the compression neither visible nor changeable once connected.
- The window's bar (user, 2026-11-25): `Ctrl+]` did not leave a window in `--gui` (egui names the key `CloseBracket`; the mapping now uses its symbol), and a keyboard without `]` (Lithuanian) has no such key. A window now has one bar row of its own above the shared screen (the server gets one row less; clicks and overlays shift by a row): `[Back]` and `F12` leave, `Ctrl+]` still does; `[Mute]` / `Alt+M` and `[-]` / `[+]` / `Ctrl+Up` / `Ctrl+Down` are the audio heard here (the shared audio is taken before the server's volume, so the server's Mute and volume are its own speakers only). In `--gui` the text of an Alt+letter is dropped (it came as a key). Start (Space, `[Start]`) on a Network source pointing at a sharing ShellSDR opens its window instead of running. Tried in tmux between two instances: Space opened the window, `Alt+M` and `Ctrl+Down` changed only the bar (the server's Mute unchanged), `F12` and `Ctrl+]` came back.
- The compression (the Network source's row, renamed from `Audio` to `Compression`; `Off (PCM)` for 0) is shown as it actually travels, on both ends: `wire::audio_label` gives "Opus 32 kb/s", "PCM 768 kb/s", or PCM with why when Opus was asked for ("no libopus here", "no libopus on the server", "Opus failed on the server"). The window's bar shows it (`Audio [..]`); a running headless stream shows `Network: ..` in the status bar (`EngineHandle::net_audio`, kept in `NetworkView::link`); the sharing side shows it in the viewer line (`10.0.0.2 (window, 120x40, Opus 32 kb/s)`), its status bar (`Shared: viewer 10.0.0.2, Opus 32 kb/s`) and the Report, so a window sees it on the shared screen as well.
- Changed while connected, without reconnecting: `Msg::SetAudio { codec, bitrate }` (client) and `Msg::AudioIs { codec, bitrate }` (server; audio packets after it use the new codec, so the client swaps its decoder in order). `Msg::Started` carries `bitrate` too (`serde(default)`: older peers interoperate). Only sent to a server of 0.4.3 or newer (`wire::changes_audio`; an older one would drop the connection on an unknown message): the window bar then says the server changes it only on a new connection, and a headless stream reconnects (the bit rate stays in the stream key). Both servers build the audio with `opus::Packer` (Opus at a bit rate, or PCM in 20 ms packets).
- Keys and clicks: the bar's `[Opus 32 kb/s]` steps through `NET_BITRATES` (a right click back), `Alt+C` steps on; the window's choice is kept in the Network source's row. On a headless stream the row itself (Enter list, Left/Right) applies at once.
- Tried in tmux between two instances: Space opened the window; both the bar and the sharer's viewer line and status bar showed `Opus 48 kb/s`; `Alt+C` stepped to 64 kb/s, then two steps on to PCM, shown on both ends within a second. Loopback tests change it on a headless stream and on a window and check the tone still decodes.

## 25. Version 0.4.4: the application icon (DONE, AWAITING REVIEW)
- `assets/icon/shell-sdr-icon.svg` (the user's) is compiled into a build with the GUI (`gui::icon`, `include_bytes!`) and drawn at 256x256 with `resvg` (optional, with the `gui` feature) when the window opens: `ViewportBuilder::with_icon`, app id `shellsdr`. If it cannot be drawn the window opens without it.
- A Linux executable carries no icon a desktop reads, so a build without the GUI gets it from the package: `tools/dev build` installs `share/icons/hicolor/scalable/apps/shellsdr.svg`, `share/applications/shellsdr-terminal.desktop` (every build, `Terminal=true`) and `shellsdr.desktop` (`shellsdr --gui`, only when built with the GUI); both entries pass `desktop-file-validate`.
- Tried: the user saw the icon on the window.


## 26. Version 0.4.5: the HFDL-ACARS VFO type (DONE, AWAITING REVIEW)
Asked 2026-11-25: an HFDL decoder as a VFO type, like DSC, built from the HFDL-ACARS seed (`../HFDL-ACARS-Decoder-Seed`: requirements, algorithms, data formats and conformance vectors) and the approved mockups. The user's choices: full ACARS decoding ported to Rust; hexdb and adsbdb lookups, each switched on separately; alerts on emergency content; everything stored, the noise hidden by default; the user's location a program setting; distances; all channels of a band in one VFO with band centers as bookmarks; an aircraft view; channels with their states and tabbed views.

### 26.1 The type
- `vfo::types::hfdl::HfdlType`, token `hfdl`, label `HFDL`. Settings: `span_khz` (default 280, 10 to 2000, bookmarkable) and `message_dir` (default: the recordings folder). The VFO's bandwidth is the span; snap 500 Hz; minimum rate span / 0.9; native recording always on (the messages); no audio.
- The engine passes the VFO and source center to the processor (`_vfo_hz`, `_center_hz` in the settings, `engine::build_processor`). The channels are every ground-station frequency (`hfdl::stations`) inside the span (`types::hfdl::channels`).
- `HfdlProcessor` runs `hfdl::rx::Receiver` (one overlap-save FFT channelizer for all channels, per channel: preamble search, M1 and rate, sync, equalizer, deinterleaver, Viterbi, descrambler) and emits `DataItem::HfdlFrame { freq_khz, m1, octets, age_s, level_db, noise_db, ferr_hz }` and every 0.5 s `DataItem::HfdlStatus { channels }`; both travel over the network remote (wire tags 3 and 4).
- The channelizer's per-block rotation is reduced exactly (integer `k0 * block * hop mod n`): a float angle lost all precision after minutes at 250 kHz, and decoding stopped (found by playing the whole 37 min recording; test `a_channel_stays_coherent_after_hours`).

### 26.2 Messages and storage
- `hfdl::pdu` (MPDU, SPDU, LPDU, HFNPDU), `hfdl::acars` (blocks, reassembly), `hfdl::apps` (ARINC 622, ADS-C, CPDLC FANS-1/A, MIAM, media advisory, OHMA): trees with the dumphfdl / libacars JSON names.
- `hfdl::msg`: classification (class, severity: emergency, caution, routine, link; keywords as whole words), subject, search terms, watch list, alerts remembered 30 min, the aircraft table (ICAO address, else registration), records.
- Day files `YYYY-MM-DD.ndjson` by local date (seed `spec/05-data`): one line per message with the PDU in hex and the stored context (`icao`/`icao_src`, `reasm`, `joined`, `systable`), the user's location (`rx`), distances, the ground station (`gs_st`), enrichment (`acft`, `route`); a reader parses `pdu` again with the stored context. Written by a writer thread; lines written by others are followed live.

### 26.3 The view (Work Area)
- `hfdl_app` (state, commands, prompts) and `ui::hfdl_view`: menu bar (View, Filter, Aircraft, Tools, Help; `F9`), status line, the channels box (state: silent, activity, decoding; frames, errors, last), tabs Messages / Aircraft / Ground stations, the alert banner, the list with the detail beside or below, the function-key bar `1Help 2Save 3View 4Watch 5Copy 6Detail 7Search 8Follow 9Menu`. Mouse: rows, tabs, menus, keys, wheel.
- Times of a recording follow the recording (the file's start time plus the frame's position); channel states use the reception clock.
- Alerts: banner, status bar `HFDL EMERGENCY` / `CAUTION`, bell (`b`), desktop notification (menu), acknowledge `a`.
- The location is a program setting (`[location]` in the profile; `AppState::location`), shared with DSC; a DSC table location of an older profile is taken over once.
- Tools > `Add HFDL band bookmarks`: the list `HFDL` with the 13 band centers (`hfdl::stations::BANDS`), each with a span covering all its channels; existing names are kept.

### 26.4 Enrichment and lookups (seed group `enrich`, `lookup`)
- `hfdl::enrich`: the aircraft, airlines and airports CSV files named in the Aircraft menu (`aircraft_file`, `airlines_file`, `airports_file` in the VFO's `HFDL` table), columns by header name, read on a worker thread. A message gets `acft` (registration, type code, type, operator from the file, else the lookup cache; the airline from the flight ID: 3 letters ICAO, else 2 characters IATA) and `route` (lookup cache, else CPDLC departure and destination airports, completed from the airports file).
- `hfdl::lookup`: hexdb.io and adsbdb.com, off by default (`lookup_hexdb`, `lookup_adsbdb`), endpoints in `hexdb_aircraft_url`, `hexdb_route_url`, `adsbdb_aircraft_url`, `adsbdb_route_url` (`{icao}`, `{callsign}`). Only downlinks; a key without an answer younger than 24 h (aircraft, not found) or 6 h (route); one request per key at a time, 1 per second per service, hexdb first, the next service after "not found" or a failure. Answers in `lookup-cache.ndjson` next to the day files, for display only (never exported).
- The message shows at once and is written after its lookups with their results; a failure is told once per message and is not cached; quitting writes waiting records without results.
- The center of a recording comes from its RIFF comment, else the first `digits[.digits]` + `Hz` / `kHz` / `MHz` in its name (`source::file::center_from_name`, vector `time/center.json`).

### 26.5 Verification
- `tests/hfdl_conformance.rs` runs the seed's vectors (frame decode, parse sequence, ACARS, applications, classify, alerts, search, aircraft, NDJSON write and read, file center, synthetic and real I/Q). Real recordings: 53 of 57 required frames (the weak 300 bit/s frames are the known gap). An ignored test, `long_recording` (`HFDL_REC`, `HFDL_REC_CENTER`, `HFDL_REC_SECONDS`, `HFDL_REC_SKIP`), plays a whole recording: on the 37 min 8.9 MHz recording 701 good frames against dumphfdl's 708 (610 shared); on 10 MHz 94 against 104.
- Tried here: the release binary on the 8.9 MHz recording (scratch profile in tmux): messages, aircraft and channels fill during playback; 63 messages by 5:36.
- Not tried: a live SDR; the online lookups against the real services (only with a fake GET in the tests); the large OpenSky file's load time on the Pi.

### 26.6 Receiver sensitivity (0.4.6)
- Measured first: a trace (`hfdl::rx::set_trace`, `HFDL_TRACE` in `long_recording`) tells where each frame search ended (second A, M1, or a frame), and the frames dumphfdl decoded but ShellSDR did not were sorted by that stage. On three new recordings (6.6, 8.9 and 13.3 MHz, 37 min together): 37 such frames, all 300 bit/s; 17 decoded but failing the FCS, 13 with no first A, 5 lost at the second A, 2 at M1.
- Tried and dropped (no gain on the full recordings, 204 good frames before): soft bits scaled by each data segment's noise from the training blocks, unclipped or with the repeated bits added before rounding (198 to 204); clipped and only weighted down (204). HF noise is impulsive: clipping each symbol's soft value at a nominal point is what holds. Faster or slower gain control (0.01, 0.005) and a smaller equaliser step (0.1) alone: 198 to 203.
- Found: those settings decide the frames near the noise either way, so two demodulators per channel with different settings decode more than either. As built: `TUNINGS` (gain control 0.01 with equaliser step 0.2; 0.002 with 0.1), both fed the same channel samples; a frame from both (same channel, start within 0.5 s) is held 0.25 s and the copy with more FCS-valid parts kept (`pdu::frame_quality`). The status line shows the first demodulator's level and noise.
- Result, good frames against dumphfdl 1.7.0 on the same channels:

| Recording | Before | Two demodulators | dumphfdl |
|---|---|---|---|
| 6.6 MHz (16 min) | 72 | 75 | 79 |
| 8.9 MHz (10 min) | 44 | 47 | 46 |
| 13.3 MHz (11 min) | 88 | 96 | 103 |
| 8.9 MHz (37 min) | 701 | 728 | 708 |
| 10.06 MHz (37 min) | 94 | 112 | 104 |
| Total | 999 | 1058 | 1040 |

- The real excerpts: 54 of 57 required frames (53 before). No frame came out twice.
- Cost: the demodulators are most of the receiver's work, so about twice the CPU (here about 3 s per 120 s of a 15-channel band, from 1.5 s). The matched filter now reads one slice and the A / M1 search correlates 127-bit registers by XOR and popcount (the same frames, a little faster). Not measured on the Pi.

## 27. Version 0.4.7: HFDL in the Report (DONE, AWAITING REVIEW)
The Report (section 20) counts and logs the HFDL VFO's work as it does DSC, FAX and Digital.

### 27.1 Counted
- `hfdl.messages` and per severity `hfdl.emergency`, `hfdl.caution`, `hfdl.routine`, `hfdl.link`; `hfdl.bad_frames` (frames failing their FCS). Live and from files (`file.` prefix) as the other decoders; per station (the band bookmark); `hfdl.messages` by hour.
- **Planes seen** (`hfdl.aircraft`): distinct aircraft heard, from downlinks only (an uplink to an aircraft does not mean it was heard), by ICAO address, else registration. **Stations received** (`hfdl.stations`): distinct ground stations heard, from uplinks and squitters.
- Distinct counts are not sums, so they are kept apart: `<profile>_seen_YYYY-MM.json` in the report folder, each aircraft or station with a mask of the days of the month it was heard on. A day's count is the entries with its bit, a month's its entries, the lifetime's the entries of all months. The counters file gets the day's and the month's counts as maxima (`Period::maxima`, they only grow); the lifetime total is the union (`Report::lifetime`). Saving merges with the file (masks OR-ed), so a folder outage or a second instance loses nothing; the files move with the folder and are pruned with `keep_months`.
- Live only: a recording played back is not counted as planes seen today.

### 27.2 Logged
- Category `HFDL` (also a `Show:` filter on the Log tab). Each HFDL alert as it is raised: emergency and caution messages as warnings (`EMERGENCY <aircraft> on 8977 kHz: <subject>`), watched aircraft as info.
- The hook: `[x] on HFDL alerts` (`hook_hfdl`, on by default) sends emergency and caution alerts.
- Nothing decoded: an HFDL job or station counts as a decoding type.

### 27.3 Shown
- Panel: `HFDL n msgs (k emergency)` and `n planes, k stations`.
- Today: rows HFDL messages (by severity, bad frames), HFDL planes seen, HFDL stations received; HFDL messages by station and by hour. Month and Lifetime: columns HFDL, Planes, GS.
- The midnight Markdown: HFDL messages (emergency, caution), planes seen and ground stations received.

## 28. Version 0.4.8: pictures at pixel resolution in the window (DONE, AWAITING REVIEW)
Section 22 promised the window (`--gui`) would paint pictures at pixel resolution as it does the spectrum and the waterfall; until 0.4.8 it painted the terminal's half blocks. Now the FAX picture, the MFSK picture and the MFSK (Digital) waterfall are painted as pixels, in the window itself and in a window into a sharing ShellSDR (the Network source's `[Open window]`).

### 28.1 How
- The views still draw the half blocks (a terminal, or a terminal viewer, sees them). When the screen is painted by a window (`AppState::pixel_pictures`, per screen), a view also records an overlay `Picture { rect, slot, fit }` and the picture's pixels (`remote::picture::Picture`, one per slot: FAX, MFSK picture, MFSK waterfall). The window paints the pixels over the rectangle (a texture, smoothed); until the first pixels come the half blocks show.
- FAX: the lines on screen, rendered at the rectangle's size in pixels (the window's cell size), at most the picture's own width and 2 megapixels. MFSK picture: the picture as received, as large as fits, at the top and centered. MFSK waterfall: every spectrum bin of 0..4 kHz across, the rows shown down.
- A picture is made once per change (`AppState::cached_picture`), not on every frame.

### 28.2 Over the network
- The overlay travels with the others (`Msg::Overlays`); the pixels in a binary frame of their own (`KIND_PICTURE` = 8: slot, key, size, grey or RGB, deflated), when they change and at most every 0.5 s per slot (a FAX picture follows its lines in steps).
- Only to a graphical viewer of 0.4.8 or newer (its `Hello` version): an older one would close the connection on the new frame and refuse an overlay list holding the new kind, so it gets neither and keeps the half blocks. A 0.4.8 window into an older ShellSDR shows half blocks too: both ends need 0.4.8.
- Hidden, as the other overlays, while a dialog or a view's menu is open over them.

