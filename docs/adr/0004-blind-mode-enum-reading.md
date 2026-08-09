# ADR-0004: `ab-blind-randomized` as the blind session's recorded mode

Blind listening (issue #28, spec #26, decision uncompose#65) is the first thing to write a
non-`ab` value into the v0 record's `mode` field. The #65 schema draft reserved three modes —
`ab`, `ab-blind`, `ab-blind-randomized` — but never said which a blind v0.1 session writes.
The spec (#26, "Decisions this repo should ADR") names that reading as this repo's to own. We
decided:

## The decision

**A `--blind` session records `mode: ab-blind-randomized`. `ab-blind` stays reserved and is
unreachable in v0.1; nothing writes it.**

- The one blind session v0.1 offers shuffles the label↔file assignment by an OS-randomness
  coin flip at load (spec story 2), so argument order tells the listener nothing. "Randomized"
  is the honest name for what happened, and downstream consumers can weight a randomized blind
  verdict accordingly (spec story 19).
- `ab-blind` is kept for a future **concealed but unshuffled** session — e.g. an external
  protocol that assigns the labels itself, where the labels are meaningful and must not be
  reshuffled. v0.1 has no such path, so writing `ab-blind` would claim a protocol the tool
  does not implement. The enum value stays in the schema (a v0 record reader must accept it)
  but the server never emits it.
- `ab` is unchanged: a sighted session records `ab`, byte-for-byte as before.

## Consequences

- `Session::mode()` is the single source of the recorded mode: `ab-blind-randomized` when the
  session is blind, `ab` otherwise. The record assembler reads it there rather than hard-coding
  a string, so the mode and the shuffle can never disagree.
- The record still carries full identities in blind mode — `candidates[]` maps each shuffled
  label to its real path/sha256/size — so `ab-blind-randomized` labels a *session* property,
  not a redaction of the record (uncompose#65; concealment is a session concern, ADR context
  shared with ADR-0005). The blind reconnection is asserted at the process boundary: labels
  {A, B} map bijectively onto the two real input hashes.
- Reaching `ab-blind` later is an additive change (a new reachable code path), not a schema
  bump: the enum already admits it.
