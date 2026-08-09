# Uncompose Compare

Local-first, two-file audio comparison: load exactly two derived versions of a piece of
audio, listen to the same moment in either with zero drift, and keep an honest record of
what was heard and decided.

## Language

**Candidate**:
One of the two audio files under comparison, labeled `A` and `B` in argument order. The
label is the reference key everywhere — in the UI switch surface, in observations, and in
the written record — so that concealment (blind mode, later) can shuffle labels without a
schema change.
_Avoid_: file, track, version, input

**Source (SRC lane)**:
The shared origin both candidates derive from, offered as a third playback lane labeled
`SRC` (project mode auto-resolves it; bare-file mode takes an explicit `--source`). It is
a *lane*, not a candidate: it plays sample-locked with A/B and joins the loudness match
group, but it never wins a preference, never enters the record's `candidates[]`, and stays
identified in blind mode (concealment is A/B only). Absent when the pair shares no source.
_Avoid_: reference, original, candidate C, third file

**Region**:
A single span of the timeline the listener drags out on a waveform to concentrate on and
loop over. The UI term is "region"; the comparison record stores it as the `loops[]`
field (at most one entry in the two-file slice). "Region" and `loops[]` name the same
thing at the two ends of the app — keep both, and never let either drift.
_Avoid_: selection, loop (in UI text), range, marker

**Observation**:
A timestamped note the listener pins while listening, attached to a candidate (`A`, `B`,
or both) and to a playback position, collected in a chronological, append-only ledger.
Observations are the honest protocol of the session: stored in the order made, never
reordered.
_Avoid_: note, comment, annotation, marker

**Comparison record**:
The immutable file written once when the session concludes, conforming to the v0 JSON
Schema this repo owns. It carries the candidates (label, path, sha256, size), the
`loops[]` region, the append-only observations, and the result (preference label or null,
with confidence and optional criterion/summary). Written exactly once, never mutated: an
evaluation cannot be quietly revised after the fact. It lands as `<ULID>.json` in the
invoking directory standalone (or `--out`), and under the project's `evaluations/` in
project mode.
_Avoid_: report, result file, output, log, manifest

**Handover (evaluation handover)**:
Concluding a project-mode session is one act — save *and* close. After the record is
written to `evaluations/`, the server auto-imports it (`uncompose-project import`) and the
process exits: 0 when saved and registered, nonzero when the import failed. The record is
never the casualty of a failed handover — it is kept, the import's stderr is relayed, and
the exact recovery command is offered. The lifecycle change (conclude ends the session in
both modes) applies standalone too: an abandoned session still writes nothing.
_Avoid_: sync, upload, export, registration (for the whole act)
