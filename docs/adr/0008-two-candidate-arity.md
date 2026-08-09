# ADR-0008: Blind mode and loudness matching stay arity-2 until M5 names the third lane

Spec #26 story 33 asks that blind concealment and loudness matching be built "against the
candidate abstraction rather than the two bare files, so that the SRC lane and project refs
join the match group and the concealment rules without redesign". The M4 implementation
honors that for every *contract* and none of the *signatures*: the record, the schema's
`playback.loudness_match`, the reveal, and the session payload are all keyed by candidate
label, while the code that produces them is `[Candidate; 2]`, `[Loudness; 2]`, a coin flip
rather than a permutation, and `let [a, b] = …` in every helper. The final review on PR #39
asked for a deliberate call rather than a silent one. We decided:

## The decision

**Keep the arity-2 signatures for v0.1. Generalize when M5 defines what the SRC lane *is*,
not before.**

The reason is not effort — a `Vec<Candidate>` migration is mechanical. It is that the
generalization cannot be written correctly yet, because the third lane's semantics are the
open question, and every one of these sites needs an answer that only M5 has:

- **Does SRC join the shuffle?** A blind session shuffles *candidates*. The source is the
  reference a listener compares against; concealing which candidate is which does not imply
  concealing (or permuting) the reference. A permutation written today would have to guess.
- **Does SRC join the match group?** Matching attenuates every lane to the quietest. If the
  source is the anchor, it may instead be the *reference level* the candidates match to —
  a different rule, not a longer loop.
- **Do the blind refusals extend to it?** Blind mode refuses a duration/rate/channel
  mismatch between candidates because the interface would have to display the difference. A
  source lane that is allowed to differ (a different master, a different rate) changes what
  "unblindable" means.
- **Is it a `Candidate` at all?** `--source` and `--project` refs may be a distinct kind
  with its own record slot rather than a third entry in `candidates[]`, which the v0 schema
  and `result.preference` both treat as the set of things that can win.

Writing `Vec` plumbing now would answer all four by accident, in whichever way made the
current two-file code compile — and an M5 that disagrees would have to unpick it. Two
concrete lanes with `[_; 2]` are honest about the arity the tool actually has; they also
make the compiler point at every site that must be revisited, which a `Vec` that silently
accepts a third element would not.

## What story 33 does buy, and it is the load-bearing half

The part that would have been expensive to retrofit is already done: **the label is the
reference key everywhere.** `candidates[].label`, `result.preference`,
`observations[].candidate`, `playback.loudness_match.candidates` (a per-label map, ADR-0003),
the reveal, and the session payload all address a lane by label. A third lane joins those
shapes by adding a key — no record migration, no schema version bump, no reader change.
Concealment is likewise defined per candidate (opaque per-candidate audio references,
ADR-0005; per-candidate blind projections in `to_blind_json`) rather than as a special case
of "two files".

## Consequences

- M5 is a signature change, not a data change. The sites that widen, in full:
  `Session::{candidates, loudness}` (`[T; 2]` → a growable collection), the `let [a, b]`
  destructures in `refuse_if_unblindable` / the mismatch predicates / `to_json` /
  `to_blind_json` / `labels`, `match_gains`, `Session::load`'s coin flip (→ a permutation
  over the concealed lanes), `PlaybackEngine`'s positional `(a, b, laneGain)` constructor
  and its `Record<Label, …>` maps, and the workbench's `["A", "B"]` literals.
- Because those are all *type* changes over label-keyed data, the failure mode of the M5
  migration is a compile error, not a wrong record.
- The two-lane assumption is stated here rather than discovered: nothing in the record
  format, the schema, or the served payload encodes "exactly two", so a v0.1 record written
  today still reads correctly when a third lane exists.
