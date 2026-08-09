# ADR-0007: A blind session advertises applied gains but conceals the measured LUFS

`--blind --loudness-match` (issue #32, spec #26) composes concealment with loudness matching.
ADR-0006 already fixed the *timing*: per-lane loudness figures follow the identity reveal and
surface only at conclude. This ADR fixes the *payload* — what a pre-reveal blind response may
carry — because ADR-0003's "the session advertises the same `loudness_match` shape it will
record" cannot hold in blind mode without leaking. We decided:

## The decision

**A blind session's advertised `loudness_match` carries `enabled`, `method`, and a per-label
map of `gain_db` only — never `measured_lufs`. The record (and the conclude reveal) still carry
the full `{measured_lufs, gain_db}` per label, exactly as sighted.**

- **The gain is what the engine must apply; the measured LUFS is a fingerprint.** Level-fair
  playback needs each lane's static `gain_db` — withholding it would break the matching the
  flag asked for. A measured integrated LUFS, by contrast, is a distinctive per-candidate
  property: publish it before reveal and a listener can fingerprint which file is which. So the
  applied gain rides the blind payload and the measured figure does not. This is the same
  "carries no more than the engine needs" line the opaque audio reference draws (ADR-0005).
- **A relative gain is not an identity.** The louder lane's `gain_db` encodes only the loudness
  *difference* between the two lanes (the quietest lane is always the `0.0` reference), which is
  inherent to any level-fair comparison — not an absolute figure that pins a lane to a file.
- **Sighted mode is unchanged.** ADR-0003's single-shape promise still holds for a sighted
  session: the endpoint advertises exactly what it records. Blind mode is the stated exception,
  and only for the pre-reveal window — the record and the reveal restore the full shape.

## Consequences

- The blind session payload gains a dedicated reduced projection (`blind_loudness_match_json`)
  distinct from the record/sighted `loudness_match_json`; the concealment-leak sweep asserts no
  `measured_lufs` — key or value — appears in any pre-conclude blind response.
- The conclude reveal, already the `{label, path, sha256, size}` mapping (ADR-0006), now also
  carries each lane's `{measured_lufs, gain_db}` when matching ran — the identities and the
  loudness figures held back during the session surface together at the one irreversible write.
  The figures are built from the same trusted measurement the record carries, so reveal and
  record cannot diverge.
- The record is untouched: `playback.loudness_match` carries the full per-label numbers in
  blind mode exactly as sighted (concealment is a session concern, not a storage one —
  uncompose#65, shared with ADR-0004/0005/0006).
