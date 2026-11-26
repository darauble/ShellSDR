# Conformance suite

Language-neutral test vectors for the DSC decoder. An implementation in any
language conforms when it reproduces every **normative** expected result
here. The vectors are JSON (plus a few WAV files), so a test runner can be
written in whatever language the implementation uses.

Every expected result has been **reviewed by hand** against the ITU texts
and the specification. Where a result is not meant to bind, it is marked
non-normative and explained in [FINDINGS.md](FINDINGS.md).

## Layout

| File | Suite | Normative | What it covers |
|---|---|---|---|
| `codec/codewords.json` | codewords | yes | 10-bit error-detecting code for symbols 0..127, DX/RX phasing and dot-pattern codewords |
| `parse/symbols.json` | parse | yes | Message body symbols → parsed fields, severity, ECC, content id |
| `ndjson/write.json` | ndjson-write | yes | Message record → one line of the per-day message file |
| `ndjson/read.json` | ndjson-read | yes | One line → record, or rejection |
| `ndjson/day_file.json` | day-file | yes | Which day file a message belongs to |
| `time/file_name.json` | file-name-time | yes | Recording start time from a WAV file name, per time zone |
| `time/iso8601.json` | iso8601 | yes | UTC / local timestamp formatting |
| `geo/distance.json` | distance | yes | Great-circle distance, km and NM |
| `geo/parse_location.json` | parse-location | yes | Parsing a user-entered location |
| `stations/mmsi.json` | mmsi-lookup | yes | MMSI → country (MID), coast-station test, built-in coast stations |
| `presentation/list_row.json` | list-row | **no** (F-11) | Texts of a message-list row; wording is free |
| `presentation/search.json` | search | yes | Smart search and distress-only filter |
| `presentation/search_terms.json` | search-terms | yes | The searchable terms of each message (F-11) |
| `presentation/detail.json` | detail | **no** | An example detail view (content guidance only) |
| `alerts/alerts.json` | alerts | yes (`alert` flag only) | Which messages raise an alert |
| `config/merge.json` | config-merge | yes | Three-way merge of settings between running instances |
| `audio/decode.json` + `audio/*.wav` | audio-decode | yes | End-to-end WAV → decoded messages; WAV formats (16/24-bit, float, extra chunks, first channel of stereo); synthetic calls: other sample rates and tones, expansion sequences, calls back to back, lost calls |
| `scenarios.md` | – | yes | Given/When/Then scenarios for behaviour that is not a pure function (below) |

`manifest.json` lists the suites with their case counts.

## File format

Every suite file has the same shape:

```json
{
  "suite": "parse",
  "description": "what is being tested",
  "normative": true,
  "compare": { "method": "how to compare", "float_abs_tolerance": 1e-9 },
  "cases": [
    {
      "id": "distress-alert",
      "source": "itu-sample",
      "description": "…",
      "input": { … },
      "expected": { … },
      "req": []
    }
  ]
}
```

Some suites carry extra top-level data (for example `search.json` has the
`messages` that the queries run over, and `codewords.json` has `special`).

Case fields:

- `id`: unique within the suite, and stable.
- `source`:
  - `itu-sample`: sample symbol sequences of well-formed calls.
  - `real-recording`: from a real radio reception.
  - `synthetic`: built for the test, often by mutating a real sample.
  - `itu-table`: one entry per row of an ITU table (the codewords).
  - For audio, `synthetic` WAVs come from a small DSC transmitter written
    to M.493-16 §1–3, independent of the receiver under test.
- `normative` (optional): overrides the suite default for this case.
- `non_normative_fields` (optional): keys of `expected` that are informative
  only. The rest of the case stays normative.
- `note` (optional): why. Usually points to a finding.
- `req`: the requirement IDs (`spec/01-requirements.md`) this case proves.

## The message record in inputs

Suites that take a whole message (`ndjson-write`, `alerts`, `list-row`,
`detail`, `search`, `search-terms`) give it as one object. Each field maps to
day-file keys (`spec/05-data/record.schema.json`):

| Input field | Day-file keys | Meaning |
|---|---|---|
| `symbols` | `sym` | the symbols, as decoded |
| `parsed` | `id`, `fmt`, `cat`, `from`, …, `ecc_*`, `errs` | the parser's result for `symbols` (as in `parse/symbols.json`) |
| `schema_version` | `v` | |
| `utc_epoch`, `utc_offset`, `time_source` | `utc`, `off`, `tsrc` | reception time (s), local offset (s), its source |
| `source_label` | `src` | the channel label |
| `source_file` | `file` | the WAV file, or `null` |
| `snr_db` | `snr` | signal-to-noise estimate in dB, or `null` when unknown (F-34) |
| `rx_location` | `rx` | the user's location at reception, or `null` |
| `position_dist_km` | `pos_km` | distance to the message's position |
| `from_info`, `to_info`, `distress_info` | `from_st`, `to_st`, `dmmsi_st` | station data at reception: `name`, `country`, `has_pos`, `lat`, `lon`, `pos_source` (`psrc`), `pos_time` (`ptime`), `dist_km` (`km`) |

`null` means absent, as everywhere (rule 1 below).

## Comparison rules

1. **`null` means absent.** If `expected.x` is `null`, the implementation MUST
   NOT produce a value for `x`. "Unknown" and "absent" are different
   (e.g. `"category": "unknown"` vs `"category": null`).
2. **Floats** compare with the suite's `float_abs_tolerance` or
   `float_rel_tolerance`. `float_abs_tolerance_overrides` gives per-key
   tolerances, with dotted paths.
3. **Strings and integers** compare exactly.
4. **Names are the vocabulary, not the code.** Enumerations are given as
   lowercase strings (`"individual"`, `"distress"`, `"req"`). Map them to
   whatever your language uses, but the meaning must be identical.
5. **Informative** suites, cases or fields show one acceptable behaviour.
   An implementation MAY differ there, and SHOULD say how.
6. **Audio**: the message count and every message's symbols must match
   exactly. Times may differ by `utc_epoch_tolerance_s`, because the exact
   sample at which a decoder locks on is implementation-specific.
7. **Time zones**: cases with a `time_zone` must run with that IANA zone as the
   local zone (the result depends on its DST rules).

## Using it in a new implementation

1. Write a small runner, in your language, that loads each suite file, feeds
   `input` to the matching function of your implementation, and compares
   against `expected` using the rules above.
2. Make every normative case pass. Report informative differences instead
   of failing on them.
3. Map your test names to the case `id`s, so failures are traceable.

## Scenarios

`scenarios.md` covers what vectors cannot: concurrency, files over time,
channel staging, settings files, online lookups, fuzzing. Each scenario has
an ID (`SC-…`) and names its requirements. The implementation writes each
one as an automated test and names the test after the ID. The file also
lists the test seams the implementation needs (temporary directories,
message injection, fake devices, a lookup stub).

## Not covered yet

- **Intermediate signal-processing stages** (demodulated bits, phasing
  detection, symbol recovery). `spec/04-algorithms/` describes them, but
  only the end-to-end result is normative.
- **Robustness**: decoding under added noise, frequency offset or clock
  drift. The synthetic WAVs are clean signals; a decoder's sensitivity is
  not specified, so no threshold is normative.
- **User interface**: checked by review against `spec/07-ui/`, not by
  vectors.

## Data notes

- `audio/20261020T152248_audio_8414.5kHz.wav` is a real HF DSC reception
  (48 kHz, mono, 16-bit) and contains real maritime identities (MMSIs) as
  broadcast on air. The other `20261020T…` WAVs are derived from it, or
  generated (silence, deterministic noise). The `20261021T100000_synth_*`
  WAVs are synthetic calls built from the ITU sample sequences.
- The file names of the WAVs are part of the input: they carry the
  recording's local start time.
