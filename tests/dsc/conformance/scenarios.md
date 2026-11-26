# Scenarios

Behaviour that vectors cannot pin down: concurrency, files over time,
settings, lookups. Each scenario is normative for the requirements it names.
The implementation writes it as an automated test in its own language and
test framework, under the scenario's ID.

## Test seams

The scenarios drive the **session** (`spec/02-architecture.md`) through its
commands and observe its events, its store and the files on disk. They need
three seams, which the implementation provides for tests:

1. **Directories.** The config and data directories can be pointed at a
   fresh temporary directory (on Linux, e.g. through `XDG_CONFIG_HOME` and
   `XDG_DATA_HOME`). No test may touch the user's real directories.
2. **Message injection.** A decoded message (symbols, time, source label) can
   be delivered to the session exactly as a decoder would deliver it,
   without audio.
3. **Fake devices and network.**
   - The list of audio devices can be replaced, and a fake device can play
     a WAV file in real time.
   - The lookup endpoint is a setting, so tests point it at a local HTTP
     stub.

Sample messages (symbols from `parse/symbols.json`):

| Name | Symbols (case id) | Notes |
|---|---|---|
| `SAFETY` | `individual-safety` | from 477231700 (ship), to 006452700 (coast) |
| `DISTRESS` | `distress-alert` | from 258999119, position 62°28′N 006°13′E |
| `RELAY` | `allships-distress-relay` | from 002570000 (coast), distress 257388000 (ship) |

"Now" means the computer clock at the time of the test.

---

## Decoding

**SC-DEC-09: Decoders are independent** (`DEC-09`)

- Given the WAV files of `audio/decode.json`.
- When all of them are decoded at the same time, each by its own decoder on
  its own thread.
- Then each file's messages equal the expected ones, exactly as when it is
  decoded alone.

## Storage

**SC-STO-03: Concurrent appends never interleave** (`STO-03`)

- Given an empty day file.
- When two writers append 1000 records each to it at the same time. Use
  records with long source labels (≥ 2000 characters) so that one record is
  larger than a typical write buffer.
- Then the file has 2000 lines, and every line is a valid record by the
  `ndjson-read` rules.

**SC-STO-07: Out-of-order dates** (`STO-07`)

- Given the local zone `Europe/Vilnius`.
- When messages arrive in the order 2026-10-20T23:59:58+03:00,
  2026-10-21T00:00:01+03:00, 2026-10-20T23:59:59+03:00.
- Then `2026-10-20.ndjson` holds the first and third, and
  `2026-10-21.ndjson` holds the second.

**SC-NFR-03: Stop loses nothing** (`NFR-03`)

- Given a WAV channel on `audio/20261021T100000_synth_back_to_back.wav`,
  started.
- When the session is stopped at a random moment (20 runs), or destroyed
  without a stop at a random moment (20 runs).
- Then in every run, each message for which `MessageAdded` occurred is in
  the day file exactly once, and no run crashes or hangs.
- If `live` is built: given a fake device that plays
  `audio/20261021T100000_synth_1700hz_11025.wav` in real time, on a
  channel at 1700 Hz.
- When the session is stopped 0.1 s after the call's last character, while
  the receiver still waits for a possible expansion sequence (F-31).
- Then the message is added and written.

## Viewing

**SC-VIEW-02: Several day files merge in time order** (`VIEW-02`)

- Given `2026-10-20.ndjson` with messages at 10:00 and 12:00, and
  `2026-10-21.ndjson` with one at 11:00 (written in that order).
- When both are opened together.
- Then the view holds the three messages ordered 10:00 (20th), 12:00
  (20th), 11:00 (21st).

## Alerts

**SC-ALR-05: Files never alert** (`ALR-05`)

- Given a day file with a `DISTRESS` record.
- When it is opened.
- Then no `Alert` event occurs and the alert state is NONE.
- When the same message is injected as decoded.
- Then exactly one `Alert` occurs.

## Channels (group `live`)

**SC-CH-03: Tone spacing** (`CH-03`)

- Given a live channel on device `a.monitor` at 800 Hz.
- When channels are staged on `a.monitor` at 900 Hz, at 1000 Hz, and on
  `b.monitor` at 800 Hz.
- Then 900 Hz is refused (100 Hz apart), 1000 Hz is accepted (200 Hz), and
  `b.monitor` at 800 Hz is accepted (another device).
- When the 1000 Hz channel is edited to 850 Hz.
- Then the edit is refused.

**SC-CH-04: Stage, start, stop** (`CH-04`)

- Given two staged channels, one on a present device and one on an absent
  one.
- When the session starts.
- Then one channel runs, the other is INACTIVE, and the mode is LIVE.
- When the session stops.
- Then both channels are still staged, the present one is IDLE, and the
  mode is VIEWING.
- Given only channels on absent devices.
- When the session starts.
- Then the start is refused and the mode stays VIEWING.

**SC-CH-05: Channels persist** (`CH-05`)

- Given two live channels and one WAV channel staged.
- When the program is restarted (a new session on the same directories).
- Then the two live channels are staged again, with their labels, devices
  and tones, and the WAV channel is not.

**SC-CH-08: Live locks the view** (`CH-08`)

- Given a running session.
- When the test opens a day file, sets the message directory, or stages,
  edits or removes a channel.
- Then each is refused with a message, and nothing changes.

## Enrichment (group `enrich`)

**SC-ENR-04: Positions from DSC are cached** (`ENR-04`)

- When `DISTRESS` is injected with time T.
- Then the station data hold 258999119 at 62.466667, 6.216667, source `dsc`,
  time T.
- And `positions_cache.csv` ends with that line.
- And a following message from 258999119 carries that position in `from_st`.
- When `RELAY` is injected with time T.
- Then 257388000 (the distress identity) is at 76.05, 32.833333, source
  `dsc`, and nothing is cached for 002570000, the coast station that sent
  it (F-33).

**SC-ENR-06: Cache reload, last line wins** (`ENR-06`)

- Given `positions_cache.csv` with two lines for 258999119: first 60.0, 5.0,
  then 61.0, 6.0.
- When the program starts.
- Then 258999119 is at 61.0, 6.0.

## Online lookup (group `lookup`)

The stub serves the aprs.fi format:

```
found:     {"command":"get","result":"ok","what":"loc","found":1,"entries":[{"name":"TEST VESSEL","mmsi":"477231700","lat":"52.58333","lng":"1.71667","time":"<T>","lasttime":"<T>"}]}
not found: {"command":"get","result":"ok","what":"loc","found":0,"entries":[]}
bad key:   {"command":"get","result":"fail","description":"authentication failed"}
```

**SC-LKP-01: Off by default, needs a key** (`LKP-01`)

- Given fresh settings.
- When `SAFETY` is injected with time now.
- Then the stub receives no request.
- Given lookup on and no key.
- When `SAFETY` is injected twice.
- Then the stub receives no request, and exactly one notice mentions the
  missing key.

**SC-LKP-02: What is looked up** (`LKP-02`)

Given lookup on with a key:

| Inject | Requests expected |
|---|---|
| `SAFETY`, time now | 477231700 only (006452700 is a coast station) |
| `RELAY`, time now | none: the relay's own position makes 257388000 fresh, and 002570000 is a coast station |
| `SAFETY`, time now − 2 h | none (too old) |
| `SAFETY`, time now, after `positions_cache.csv` has 477231700 observed 10 min ago | none (fresh) |
| `SAFETY`, time now, with 477231700 in `ships.csv` with a position | none (user data) |

**SC-LKP-03: Shown at once, written after** (`LKP-03`)

- Given lookup on and a stub that answers "found" after 2 s.
- When `SAFETY` is injected.
- Then `MessageAdded` occurs within 0.25 s, and the day file does not yet
  hold the record.
- After the answer:
  - `MessageUpdated` occurs;
  - the day file holds one record whose `from_st` has name `TEST VESSEL`,
    source `aprs.fi` and the time from the stub;
  - the channel no longer shows the lookup as pending.

**SC-LKP-04: Failures are told, records kept** (`LKP-04`)

- For each of "not found", "bad key", and the stub not listening:
  - When `SAFETY` is injected.
  - Then one notice names 477231700 and the reason.
  - And the record is written without a lookup result.

**SC-LKP-05: Quit during a lookup** (`LKP-05`)

- Given a stub that never answers.
- When `SAFETY` is injected and the session is destroyed 0.5 s later.
- Then destruction completes within 2 s, and the day file holds the record
  without a lookup result.

## Settings

**SC-CFG-04: Bad settings files** (`CFG-04`)

| File content | Expected settings |
|---|---|
| missing | defaults |
| `{ not json` | defaults |
| `{"alert_bell": 0, "has_location": 1, "user_lat": 1.25, "user_lon": 2.5}` | sound off, location (1.25, 2.5): 0/1 accepted |
| `{"save_dir": "x \"alert_bell\": 0", "alert_bell": true}` | sound on: a key inside a string is not a key |
| `{"user_lat": "bad", "default_tone": 812.5}` | latitude default (0), tone 812.5 |
| `{"channels": [{"channel_label": "a", "device": "d", "tone": 1234.5}, 7]}` | one channel, tone 1234.5 |

The program starts in every case. Saving and loading again gives the same
values, exactly.

## Robustness

**SC-NFR-02: Fuzzing** (`NFR-02`)

Each part must survive these inputs without a crash, hang or unbounded
memory use. Each runs for at least 100 000 iterations, or 60 s.

| Part | Inputs |
|---|---|
| Parser | random symbol sequences of length 0…80 with values 0…127 and 255; and each `parse/symbols.json` case with 1…5 random symbols changed |
| Day-file reader | random byte strings up to 4 KiB; and each `ndjson/read.json` line with random bytes changed |
| WAV reader | each audio vector with random header bytes changed, truncated at random points, or with chunk sizes set to 0, odd, or past the end of the file |
| Settings reader | random JSON values of every type for every key |
