# Findings: the decision log

The questions that came up while the expected results were reviewed and
the specification was written, and what was decided. Each entry gives the
issue, the decision and why. The decisions are part of the specification.
Other files cite them by ID (`F-nn`).

| ID | Issue | Decision |
|---|---|---|
| F-01 | How strictly is a day-file line read? | one valid JSON object |
| F-02 | File-name time in DST transitions | the time in the name; defined for the gap and the overlap |
| F-03 | Location text with trailing text | exactly two numbers |
| F-04 | Where are EOS and ECC when EOS is repeated? | first EOS after the content, ECC right after it |
| F-05 | Channel-number coding | verified against Table A1-5 |
| F-06 | 7-digit frequency coding | per §8.3.2.1, with the TX element |
| F-07 | Area call corner | signed like positions; layout per §5.3 |
| F-08 | Nothing decodable: empty or absent? | absent |
| F-09 | Precision of stored positions | 6 decimal places |
| F-10 | WAV files beyond the 44-byte header | full RIFF reading, first channel |
| F-11 | Are list-row texts normative? | (c) no; search over defined terms |
| F-12 | Distress acknowledgement vs. relay | read the distress telecommand |
| F-13 | Position acknowledgement | Pos4 and UTC, not a frequency |
| F-14 | One format-specifier copy for distress and all ships | kept, marked unconfirmed, no alert |
| F-15 | Undefined frequency/channel forms | own kinds, not frequencies |
| F-16 | Format/category 103 (VTS) | `vts`, never alerts |
| F-17 | Automatic service fields | number and call duration decoded |
| F-18 | Expansion sequences (ITU-R M.821) | decoded and joined to the call |
| F-19 | Field values out of range | absent |
| F-20 | MID table and identity forms | the full ITU table; M.585-10 forms |
| F-21 | M.493-16: nature 112 (EPIRB) dropped but still used | decode as legacy |
| F-22 | M.493-16: category 106 "not to be used" but used by ACS | decode as ACS |
| F-23 | M.821: course example contradicts the coding rule | follow the rule |
| F-24 | When does the receiver look for the end? | from character 39 |
| F-25 | Where does the audio output of a call end? | at the last ECC |
| F-26 | A lost call followed by another | drop it; resync on new phasing |
| F-27 | A call cut at the character limit | record it, never alert |
| F-28 | Two equal copies of an invalid codeword | (b) garbled |
| F-29 | Content id start value | FNV-1a with a stated, non-standard start value |
| F-30 | How is the settings file read and written? | as JSON; exact numbers |
| F-31 | Stop while a call waits for a possible expansion | flush the decoders |
| F-32 | Empty device name; self-cancel vector | refused; vector uses 110 |
| F-33 | Whose position is a relayed position? | the distress identity's |
| F-34 | Write vector expected `snr` missing from its input | input gains `snr_db` |
| F-35 | Is the time source read back from a day file? | yes; absent or unknown is wallclock |
| F-36 | Spec names differ from the vector names | the vectors' names everywhere |

The ITU source is **ITU-R M.493-16 (12/2023)**. Content found only in
M.493-15 is kept as *legacy: decode, don't expect* (`spec/03-domain/`).

---

## F-01 How strictly is a day-file line read?

A reader that searches the line for `"key":` rejects valid JSON with a
space after the colon, and can pick up a key name that occurs inside a
string value.

- **Decision.** A line MUST be one valid JSON object (RFC 8259): any
  whitespace, any key order, all string escapes. Anything else (trailing
  text, an array, a torn line) is rejected. `sym` MUST be a non-empty array
  of integers 0..255, or the line is rejected. Known keys of an unexpected
  type are treated as absent; unknown keys are ignored.
- **Why.** The day files are JSON; any JSON writer must be readable, and a
  torn line must never be half-read.
- Vectors: `whitespace`, `unicode-escapes`, `trailing-content`,
  `not-an-object`, `symbol-out-of-range`, `symbol-not-integer`,
  `key-in-string`, `wrong-type-ignored`.

## F-02 File-name time in DST transitions

The file name holds local time. One hour occurs twice when clocks go back
(`20261025T033000`, Europe/Vilnius), and one does not exist when they go
forward. The C library's `mktime` guesses in these hours, and the guesses
differ between systems.

- **Decision.** The reported local time MUST equal the time in the file
  name. In the repeated hour, take the earlier instant (summer time). In
  the missing hour, which cannot be represented, shift forward by the gap
  (`03:30` → `04:30+03:00`). At most three separator characters between
  date and time.
- **Why.** The name is what the user sees; the result must not depend on
  the platform. Compute the offset yourself instead of relying on
  `mktime`.
- Vectors: `dst-gap`, `dst-overlap`, `dst-after-overlap`,
  `three-separators`, `four-separators` (not found).

## F-03 Location text with trailing text

Should `"54.5 25.2 xyz"` be read as 54.5, 25.2?

- **Decision.** No. Exactly two numbers, nothing else.
- **Why.** A half-read location gives wrong distances to every ship; an
  error the user sees is better.
- Vectors: `trailing-garbage`, `three-numbers`, `not-a-number`, `infinity`
  (`nan` and `inf` must not pass the range check).

## F-04 Where are EOS and ECC when EOS is repeated? (high)

Taking the last symbol as the error-check character (ECC) and the one
before it as the end of sequence (EOS) fails for sequences that end with
`EOS, ECC, EOS, EOS` (the EOS repetitions of the transmission). The ECC is
an XOR parity, so the "computed" value then always equals the final EOS
symbol, and **the check reports OK whatever the content**.

- **Decision.** The EOS is the first EOS symbol (117, 122 or 127) **after
  the second telecommand**, i.e. not before the position where the second
  telecommand ends (distress alerts, which have no telecommands, search
  after the self-identity). The ECC is the symbol right after it. Anything
  later is ignored. With no EOS, or a garbled ECC, EOS and/or ECC are
  absent. The ECC covers the format specifier once (the chosen copy), the
  content and the EOS.
- **Why "after the second telecommand".** In M.493-16 ACS calls (Table
  A1-4.9.2) the second telecommand is 120..125, so it can be **122**, an
  EOS value. No other field can carry 117, 122 or 127.
- Vectors: `allships-safety` (real ECC 32), `eos-repeated-bad-ecc` (a
  changed content symbol is caught), `eos-repeated-good-ecc`, `no-eos`,
  `acs-call`.

## F-05 Channel-number coding

HF/MF and VHF channel numbers are assembled as
`(s0 mod 10)*10000 + s1*100 + s2`. Sample `individual-hfmf-channel-b`
gives channel 44272, which looks implausible.

- **Decision: correct as it is.** Table A1-5: for HF/MF (HM digit 3) the
  channel is the digits TM M H T U, which is exactly this formula. For VHF
  (HM 9) it is M H T U with TM 0, so the formula gives the same result.
  44272 is simply what that sample transmits. The VHF digit M and TM ≠ 0
  are covered by F-15.
- Vectors: `individual-hfmf-channel-a/b`, `vhf-channel`.

## F-06 7-digit frequency coding (high)

How are the four symbols of a 7-digit (10 Hz resolution) frequency read?
A tempting reading, `s0*100 + s1 + s2/100 + s3/10000`, gives 4122.55 kHz
for `41 22 55 00`.

- **Decision.** Per §8.3.2.1 and Table A1-5 the symbols carry the digits
  `4 TM | M H | T U | T1 U1`: the frequency is
  `lo(s0)*10000 + s1*100 + s2 + s3/100` kHz, so `41 22 55 00` is
  **12255.00 kHz**. The TX element (the next four symbols, or four 126s)
  is read too.
- Vectors: `freq-7-digit`, `freq-7-digit-rx-tx`.

## F-07 Area call corner

Positions are signed by quadrant (south and west negative). Should the
corner of an area call be too, or carry the quadrant only as letters?

- **Decision.** The corner is signed like positions. Layout per §5.3:
  quadrant, latitude (2 digits), longitude (3 digits), Δlat and Δlon (2
  digits each). The corner is the north-west one; the area extends Δlat
  south and Δlon east.
- **Why.** A stored value must be unambiguous on its own.
- Vectors: `area-call`, `area-call-sw`.

## F-08 Nothing decodable: empty or absent?

A frequency element whose symbols are all garbled, or a sequence with no
category symbol at all: is the field empty, `"unknown"`, or absent?

- **Decision.** A field is **absent** when nothing could be decoded.
  `"unknown"` is for a symbol that was received but is not in the table.
  So: no category symbol → `category` absent; category symbol 104 →
  `"unknown"`.
- Vectors: `garbled-short`, `empty`, `single-symbol`, `garbled-category`.

## F-09 Precision of stored positions

Six significant digits (e.g. `151.209`) lose about 100 m at high
longitudes.

- **Decision.** Coordinates are written with 6 decimal places (about
  0.1 m), the same as station positions.
- Vectors: the tolerance on these keys is 1e-6.

## F-10 WAV files beyond the 44-byte header

Many WAV files have extra chunks (`LIST` metadata is common), or 8/24/32-bit
or float samples. A reader that takes the format from fixed offsets and the
samples from byte 44 misreads them.

- **Decision.** The reader walks the RIFF chunks, skips unknown ones (odd
  sizes are padded) and reads only the `data` chunk's bytes. Supported:
  integer PCM 8/16/24/32-bit and IEEE float 32/64-bit, plain or
  `WAVE_FORMAT_EXTENSIBLE`. Float full scale is ±1.0. Multi-channel files:
  the **first channel** is decoded. Headers outside 1..64 channels or
  1–1000 kHz are rejected.
- **Why the first channel.** An I/Q or dual-receiver recording must not be
  mixed down.
- Vectors: `stereo-first-channel` (right = inverted left, so a mix-down
  decodes nothing), `pcm24-list-chunk`, `float32-extensible`.

## F-11 Are list-row texts normative? (decision)

Search could match what the list shows (`INDV`, `DISTRESS`, `man
overboard`, …). Then those texts would have to be normative to keep search
identical everywhere, and every front end would be bound to them.

- Options:
  - (a) Make the texts normative.
  - (b) Make them informative, and search "what the front end displays".
  - (c) Search over a fixed set of terms, independent of display.
- **Decision (2026-11-08): (c).**
  - Display texts are informative. A front end may show `INDV` or
    `Individual`. The list-row and detail vectors are examples only.
  - Search is normative and exactly tested, over the terms of
    `spec/07-ui/search-terms.md`. A query is split into words at
    whitespace; each word must occur, case-insensitively, as a substring of
    one term, and all words must match.
- Vectors: `presentation/search_terms.json`, `presentation/search.json`.

## F-12 Distress acknowledgement vs. relay (high)

In calls with category distress (Tables A1-4.2 to A1-4.4), the telecommand
after the self-identity says what the call is: **110 distress
acknowledgement**, **112 distress alert relay**. Without it, an all-ships
acknowledgement and an all-ships relay look the same. Message 4
(subsequent communications: 100, 109, 113 or 126) also has to be read.

- **Decision.** `distress_telecommand` (110 / 112) and `subsequent_comm`
  (message 4) in all distress calls; a distress alert's message 4 is not a
  first telecommand. Self-cancel (§8.6) requires telecommand 110: a relay
  with equal identities is a man overboard device relaying its own alert
  (§8.4).
- Vectors: `allships-distress-ack`, `self-cancel` (an all-ships
  acknowledgement, as in Table A1-4.2), `mob-relay-own-id`.

## F-13 Position acknowledgement

A position acknowledgement (first telecommand 121, Table A1-4.7) carries
Pos4 in message 2 (a position, then one 126) and the UTC time as message 3.
Read as a frequency element, 62°28′N 006°13′E would become "6228.00 kHz".

- **Decision.** With first telecommand 121, message 2 is Pos4 and message
  3 the UTC (`event_time`). Six 126s with EOS 122 is the M.493-16 refusal
  (`position_refused`).
- Vectors: `position-request`, `position-ack`, `position-refused`.

## F-14 One format-specifier copy for distress and all ships

§4.2: receivers **must** detect the format specifier twice for distress
(112) and all-ships (116) calls, to avoid false alerts. For other formats
one copy is enough.

- **Decision: kept, but marked.** `format_unconfirmed` is true for a 112
  or 116 call without two readable, identical copies; such a call is
  recorded but never raises an alert.
- **Why.** Recording everything loses nothing; the alert is what §4.2
  protects.
- Vectors: `distress-format-single-copy`, `allships-format-disagree` (and
  their alert cases).

## F-15 Undefined frequency/channel forms

Table A1-5 defines HM digits 0–2 (frequency), 3 (HF/MF channel), 4 (7-digit
frequency), 8 (ITU-R M.586 equipment only) and 9 (VHF channel, TM 0).
HM 5–8 as frequencies would be 50 MHz and above, but frequencies are only
coded below 30 MHz (§8.3.2.1).

- **Decision.** Kinds `freq`, `hfmf`, `vhf` (with `vhf_mode`, the digit M:
  0 normal, 1 ship frequency as simplex, 2 coast frequency as simplex),
  `m586` (no value), `unknown`. VHF with TM ≠ 0 is `unknown`.
- Vectors: `vhf-channel-simplex`, `vhf-tm-nonzero`, `freq-m586`,
  `freq-hm-undefined`.

## F-16 Format/category 103 (VTS)

Symbol 103 as format specifier and as category means a call to a group of
ships in a VTS area (ITU-R M.825, Table A1-3 note 3). Such calls **shall
not activate any alarms**.

- **Decision.** Format `vts`, category `vts`; no alert for either.
- Vectors: `vts-format` (with category urgency, still no alert),
  `vts-category`.

## F-17 Automatic service fields

Format 123 calls (Tables A1-4.10.1/2) carry a telephone number as message 3
(§8.3.3: 105 or 106, then digit pairs). The end-of-call acknowledgement
carries a call duration in message 2, coded per ITU-R M.689/M.1082, not
M.493.

- **Decision.** Both are decoded: `number` (digits as a string) and
  `duration_s` (ITU-R M.689-3 / M.1082-1).
- Vectors: `auto-number-odd`, `auto-number-even`,
  `auto-end-ack-duration`, `auto-end-ack-no-duration`,
  `auto-end-ack-unsolicited`, `auto-end-request`.

## F-18 Expansion sequences (ITU-R M.821)

A distress alert, and some acknowledgements and relays, may be followed by
an expansion message with enhanced position resolution (§11.4, Table
A1-4.11: 100, four symbols, EOS, ECC, EOS).

- **Decision: decoded, with the precise position.** All seven M.821
  fields are decoded (`expansion`), including requests (110) and "no data"
  (126). Field 100 gives `precise_position`. Coding:
  `spec/03-domain/field-coding.md`, *Expansion sequence*.
- **Receiver.** After a call's ECC the receiver looks at up to three more
  characters. If an expansion data specifier arrives, it reads on to the
  expansion's EOS; otherwise it outputs the call unchanged. Input that ends
  during this look-ahead still yields the call (a flush at the end of
  input).
- Vectors: `expansion-position`, `expansion-position-fillers`,
  `expansion-position-ack`, `expansion-nav-speed-course`,
  `expansion-station-id`, `expansion-requests`, `expansion-bad-ecc`;
  audio `synth-expansion`, `synth-expansion-fillers` (both on-air layouts
  M.821 allows).

## F-19 Field values out of range

Positions with minutes ≥ 60 or latitude > 90, times like 25:61, area
corners beyond ±90/±180. ITU does not say what a receiver should do with
them.

- **Decision: absent.** Positions (minutes ≥ 60, beyond 90°/180°), times
  (beyond 23:59) and area corners (beyond 90°/180°) are absent; a nature of
  distress outside 100..110 and 112 is absent.
- Vectors: `position-minutes-out-of-range`, `time-out-of-range`.

## F-20 MID table and identity forms

MIDs are allocated by the ITU (Table of Maritime Identification Digits,
ITU-R M.585 and the ITU MARS database), not in M.493. A partial table, or
one taken from elsewhere, gives wrong countries. M.585 also defines
identity forms where the MID is not the first three digits: `111MID…` SAR
aircraft, `970`/`972`/`974` AIS-SART/MOB/EPIRB, `98MID…`, `99MID…`.

- **Decision.** `spec/03-domain/mid.csv` is the full ITU table (294 rows,
  downloaded 2026-11-08), and `identities.md` gives the M.585-10 forms. The
  MID is found by the identity's form, not by taking the first three
  digits after any leading zeros. Parsed identities carry `kind` and
  `mid`. Country names are the ITU's own, e.g. "Lithuania (Republic of)".
- Vectors: `stations/mmsi.json`, one case per identity form and one per
  MID (`mid-201` … `mid-775`).

## F-21 M.493-16: nature of distress 112 (EPIRB) dropped but still used

M.493-15 Table A1-3 defined symbol 112 as nature of distress "EPIRB
emission". M.493-16 removed it from Table A1-3 and the EPIRB rows of Tables
A1-4.1, A1-4.3 and A1-4.4, but Table A1-4.2 still has a VHF "distress
acknowledgement (EPIRB)" row with nature 112.

- **Decision: decode as legacy.** Nature 112 is "EPIRB emission", marked
  legacy (`symbols.csv`, column `m493_15`). Old equipment still sends it.

## F-22 M.493-16: category 106 "not to be used" but used by ACS

M.493-16 Table A1-3 marks category 106 with note 6 ("should not be used in
any future expansion"). The new Table A1-4.9.2 (ACS, automatic connection
system, MF/HF) uses category 106 in every row, and the second telecommand
120..125 to count the remaining ACS transmissions (§8.3.1, Table A1-3).

- **Decision: decode as ACS.** Category 106 is "ACS". The second
  telecommand 120..125 is "N remaining ACS transmissions" (N = symbol −
  120).

## F-23 M.821: course example contradicts the coding rule

M.821-1 §2.1.2.4 codes the course as four digits: hundreds, tens, units and
tenths of degrees. Its example in §3.3 says 298° is sent as `103 02 98`. By
the rule those digits (0, 2, 9, 8) mean 29.8°; 298° would be `103 29 80`.
The speed example, `102 01 24` = 12.4 kn, follows the rule.

- **Decision: follow the rule** (§2.1.2.4). `103 02 98` decodes as 29.8°.
- Vector: `expansion-nav-speed-course`.

## F-24 When does the receiver look for the end?

The shortest call, a distress alert (18 characters), has its ECC at
character 39 of the received stream. Looking for the closing EOS copies
only from character 40 notices its end late; a call that follows within
that time has its phasing read into the first call, and is lost.

- **Decision.** The end search starts at character 39
  (`spec/04-algorithms/02-receiver.md`).
- Vector: audio `synth-back-to-back`.

## F-25 Where does the audio output of a call end?

A call ends with its ECC, but the transmitter then repeats the EOS (§3,
Fig. A1-1), and a receiver notices the end only after it has seen some of
those characters. How many land in the output would depend on the method,
so two correct decoders could differ.

- **Decision: the output ends at the last ECC.** A decoder's symbols for a
  message run from the first format specifier through the call's ECC, or
  through the expansion sequence's ECC when there is one (M.821). Nothing
  after it. The parser still accepts trailing symbols (the parse suite has
  such cases).
- Vectors: every `audio/decode.json` case compares the symbols exactly.

## F-26 A lost call followed by another (high)

A call that fades out before its end can swallow the next call in two
ways:

1. When the signal is lost (more than 20 bad characters), the receiver goes
   back to searching for phasing. If it keeps the partial call's symbols,
   the next call is appended to them.
2. Often the signal is not even "lost". Two equal copies of a steady
   pattern, such as another station's dot pattern, can pass as a symbol
   (see F-28), so the error count stays low, and the receiver reads on
   through the next call's dot pattern, phasing and content. If the
   70-character limit then cuts the call, the next call's phasing symbols
   104..106 look like an expansion sequence (specifiers 100..106).

Either way the second call is lost, and the parser sees a garbled call with
the first one's format specifier, possibly a confirmed distress alert.

- **Decision.**
  - The partial call is dropped when the signal is lost.
  - While a call is being read, the receiver also looks for a new phasing
    sequence. If it finds one, it drops the call and starts over there.
  - A call ended by the character limit gets no expansion look-ahead.
- Vectors: audio `synth-lost-call`, `synth-lost-call-2s`;
  `synth-lost-call-3s` (informative).

## F-27 A call cut at the character limit (decision)

When no EOS is found within 70 characters (`MAX_CHARS`), the call ends
there. It has no EOS and no ECC; its fields come from whatever was
received and can be garbage. Its format specifier may still be a
confirmed 112 112.

- Options:
  - (a) Report it as any other call: a garbage distress alert is possible.
  - (b) Drop it, like a lost call. A distress alert attempt is five
    consecutive alerts (§11.1, §11.4; ITU-R M.541), so the next copy
    brings what this one would.
  - (c) Report it, but never alert on it.
- **Decision (2026-11-08): record it, never alert.** Everything received
  is recorded, including a cut call. A call without an EOS never raises an
  alert, whatever its format or category. A call with an EOS but a bad ECC
  still alerts.
- Vector: `distress-cut-no-eos` (parse and alerts).

## F-28 Two equal copies of an invalid codeword (decision)

Each symbol arrives twice, as DX and as RX five characters later (§1.2).
A rule "if the two copies are equal, take them" accepts them even when both
fail the 10-bit check (§1.1). Random noise rarely corrupts both copies the
same way, but a steady pattern does: the dot pattern (`1010101010`) fails
the check, yet each pair of its copies is equal, so it would be read as
symbol 42 with no error counted. That is how F-26's second form goes
unnoticed.

- Options:
  - (a) Equal copies are always taken.
  - (b) Equal copies are taken only if the codeword is valid.
- **Decision (2026-11-08): (b).** Choose in this order: a valid RX copy,
  then a valid DX copy, else the symbol is garbled
  (`spec/04-algorithms/02-receiver.md`).
- **Why.** §1.1 gives the codeword check without exceptions.

## F-29 Content id start value

The content id is a 64-bit FNV-1a hash of the symbols, started at
`1469598103934665603`. The standard FNV-1a offset basis is
`14695981039346656037` (one digit more). A library FNV-1a therefore gives
different ids.

- **Decision: use the stated value** (`spec/04-algorithms/03-parser.md`,
  step 8). Check value: `individual-safety` gives `584c203a0db32c6b`.
- **Why.** Content ids in existing day files use it; another value would
  break duplicate detection against stored messages. The ids are as unique
  as with the standard value.

## F-30 How is the settings file read and written?

Searching the text for `"key":` reads a key name inside a string value as
the key: a save directory named `x "alert_bell": 0` would turn the alert
sound off. Writing tones as whole hertz, or the location with fewer
digits, makes values read back differently from what was set, which
confuses the three-way merge (it compares exactly).

- **Decision.** The file is read with a JSON parser. An unparseable file
  gives all defaults, and a key of the wrong type keeps its default.
  Numbers are written with full precision (they read back exactly).
  Booleans are written as `true`/`false`; `0`/`1` are also read.
- Spec: `spec/05-data/config.schema.json`, `CFG-04`. Scenario `SC-CFG-04`.

## F-31 Stop while a call waits for a possible expansion

After a call's ECC the receiver waits up to three characters (300 ms) for
an expansion sequence (F-18). If stop (or quit) releases the decoders
without flushing them, a call that has just ended is lost (`NFR-03`).

- **Decision.** Stop flushes every decoder after capture has stopped and
  before the decoders are released (`spec/06-behaviour.md` §1).
- Scenario `SC-NFR-03`.

## F-32 Empty device name; self-cancel vector

- Staging a live channel with an empty device name is refused
  (`spec/06-behaviour.md` §2).
- The ndjson-write vector `self-cancel` uses distress telecommand 110. With
  112 it would be a relay (F-12), not a self-cancel, and the `selfcancel`
  key would go untested.

## F-33 Whose position is a relayed position?

A distress relay or acknowledgement carries the position of the vessel in
distress, but its sender is another station, often a coast station.
Caching the position for the sender too would move that station to the
distress position, and since the cache is loaded after the built-in and
user positions, this would outlive a restart (`ENR-04`).

- **Decision.** A received position is cached for the distress identity
  if the message has one, else for the sender.
- Spec: `ENR-04`, `spec/02-architecture.md`. Scenario `SC-ENR-04`.

## F-34 A write vector expected a value its input did not hold

Found in the clean-room trial. The ndjson-write case `wav-source` expected
`"snr":12.5` in the written line, but its input record had no SNR field, so
no implementation could pass it. The record input of the vectors was not
described anywhere either.

- **Decision.** The input record has the field `snr_db` (dB, or `null`
  when unknown), and `wav-source` gives 12.5. The writer writes `snr` when
  the value is present, like every other key (`record.schema.json`). The
  record input is described in `conformance/README.md`.
- **Why.** The schema lets a writer record the SNR, so the writing must be
  tested; the input has to carry the value.

## F-35 Is the time source read back from a day file?

Found in the clean-room trial. The ndjson-read case `roundtrip-wav-source`
read a line with `"tsrc":"filename"` but expected the time source
`wallclock`: the expected results came from a reader that ignored `tsrc`.
The schema marks `tsrc` as stored, and `TIM-04` says a record keeps where
its time came from.

- **Decision.** The reader keeps `tsrc` from the line. An absent or
  unknown value reads as `wallclock`.
- **Why.** A message decoded from a WAV file and viewed later must still
  say that its time came from the file name; that tells the user how far
  to trust it.
- Vectors: `roundtrip-wav-source` (now `filename`), `tsrc-mtime`,
  `tsrc-unknown`.

## F-36 Spec names differ from the vector names

Found in the clean-room trial. `spec/04-algorithms/03-parser.md` named the
formats `geographic_area`, `common_interest` and `all_ships`, and
`search-terms.md` did too, while the vectors use `area`, `common` and
`allships`. The parser text also said a missing ECC gives
`received = -1, computed = -1`, while the vectors have `null`. And
`identities.md` described the identity kinds in words only, without the
names the vectors use (`sar_aircraft`, `mob`, …).

- **Decision.** The vectors' names are the vocabulary. The spec text uses
  them: format names in `03-parser.md` and `search-terms.md`; a missing ECC
  is ABSENT in the parsed record (the day file writes `-1`, per
  `record.schema.json`); `identities.md` gives each kind's name.
- **Why.** Rule 4 of `conformance/README.md` lets an implementation map
  the names to its own, but the spec must not offer a second set of names
  to map from.
