# ADR-0003: The pinned `playback.loudness_match` schema shape

Loudness matching (issue #30, spec #26, decision uncompose#66) is the first thing to write
into the v0 record's `playback` object, which until now was an unconstrained `{}`
placeholder the schema described but did not shape. The spec (#26, "Decisions this repo
should ADR") names the pinned `playback.loudness_match` shape as this repo's to own. We
decided:

## The shape

**`playback` requires a single `loudness_match` object and nothing else. `loudness_match`
requires `enabled` (boolean); when `enabled` is `true` it also carries `method` (string,
`bs1770-integrated` in v0.1) and `candidates`, a per-label map of
`{measured_lufs, gain_db}` numbers (`measured_lufs` is `null` for a lane the integrated gate
gave no reading for — see the consequences). When matching is off the server writes exactly
`{"loudness_match": {"enabled": false}}`.**

```json
"loudness_match": {
  "enabled": true,
  "method": "bs1770-integrated",
  "candidates": {
    "A": { "measured_lufs": -11.2, "gain_db": -2.4 },
    "B": { "measured_lufs": -13.6, "gain_db": 0.0 }
  }
}
```

- **`enabled` is always present, so the absence of matching is stated, not implied** (spec
  story 23). A reader never has to infer "no `loudness_match` means off"; off is written
  down. The schema enforces the coupling with an `if/then/else`: `method` and `candidates`
  are present exactly when `enabled` is `true`, so a record can neither claim a method
  without matching nor enable matching without recording what it did.
- **`gain_db` is capped at `maximum: 0`.** Matching is gain-only attenuation down to the
  quietest lane — never a boost, so nothing clips and no limiter colors the audio (#66).
  The schema refuses a positive gain, so a record that claims to have boosted a lane is
  invalid by construction, not merely by convention. The quietest lane records exactly
  `0.0`.
- **`candidates` is keyed by candidate label**, the same reference key every other part of
  the record uses (`candidates[].label`, `result.preference`, `observations[].candidate`).
  A per-label map (rather than a parallel array) makes a lane's figures directly
  addressable by the label the listener saw, and extends to the M5 SRC lane joining the
  match group without a shape change.
- **`method` is a free string, not an enum**, so the record survives a future normalization
  method (#66, spec story 24) without a schema version bump: v0.1 writes
  `bs1770-integrated`, a later release can write another name, and old readers still parse.

## Server-authoritative, like the rest of `playback`

The measurement runs server-side at load (one BS.1770 integrated pass per candidate over
the already-decoded PCM), and the server assembles `playback` from that measurement — never
from the browser's posted body, which supplies only `result`, `observations`, `loops`, and
`context` (ADR-0002). A browser cannot forge a measured LUFS or an applied gain any more
than it can forge a candidate's hash.

## Consequences

- `playback` is now a required, closed object: every record carries a `loudness_match`, and
  the process-boundary tests assert its shape both matched and unmatched. A record written
  before this change (empty `playback`) would no longer validate — acceptable, as no v0.1
  record has been published yet.
- The session endpoint advertises the same `loudness_match` shape it will record, so the
  workbench reads the per-lane `gain_db` it applies as a static gain from the exact shape
  the record carries — one shape, measured once, both displayed and engraved.
- A non-finite measurement (a lane below the integrated gate reads `-inf` LUFS) is left at
  `0.0` gain and does not become the match reference, so a silent input can never produce a
  `-inf` gain that would fail the `number` type.
- **That lane's `measured_lufs` is recorded as `null`, not as a number.** JSON has no
  `-inf`, and the two candidate encodings are not equally honest: flooring it to `0.0`
  would engrave the *loudest possible* figure for the *quietest possible* lane —
  indistinguishable from a real reading, and self-inconsistent with the `0.0 dB` gain
  beside it. `null` says the one true thing: the gate produced no reading. `measured_lufs`
  is therefore typed `["number", "null"]`; `gain_db` stays a plain number, because a lane
  that could not be measured still has a defined applied gain (exactly 0 dB). Readers that
  average or plot the figures must skip the nulls — which is the correct treatment of a
  measurement that does not exist.
