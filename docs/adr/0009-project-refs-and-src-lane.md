# ADR-0009: Project refs and the SRC lane

M5 slice 4 (issue #40, spec #42) turns the two positionals into manifest refs under
`--project` and adds the SRC lane. ADR-0008 deferred the SRC lane's semantics until M5
"defines what the SRC lane *is*" — every widening site it enumerated needed an answer only
M5 has. This is that definition.

## The manifest and ref grammar

`--project <dir>` reads exactly `<dir>/uncompose.project.json` — no upward walk, no plumbing
command, a direct file parse. The manifest this repo consumes is minimal: a project `id`
(ULID), an `assets[]` list (`{id, slug, file, sha256}`), and an optional `derivations[]`
graph (`{id, inputs[], outputs[]}` by asset id). uncompose-project owns the full schema;
this repo reads only what ref resolution and the SRC search need, so a richer manifest still
parses.

A ref is one of exactly two v0.1 forms:

- a **bare token** is an asset slug — the asset whose `slug` matches;
- `<name>@<derivation>` selects, among that derivation's **output** assets, the one whose
  file basename-stem equals `<name>`.

No-match and ambiguity list the derivation's outputs with slug and filename, so the message
alone tells the listener what they could have named. A raw path (anything with a separator)
refuses in project mode — the positionals are manifest handles, not files — and `--out`
conflicts with `--project` (the project record's destination is the handover's, slice 5).

Pre-flight, in order, all before the server binds: the manifest parses, `uncompose-project`
is on PATH (a session that cannot be registered never starts — the check is a plain PATH
scan, since this repo stubs the tool's evaluation import), both refs resolve, and each
resolved file is hashed by the existing load pipeline and checked against the manifest's
recorded sha256 (a drifted asset refuses, named).

## The four questions ADR-0008 left open

**Is the SRC lane a `Candidate`?** No — it is a *playback lane*, not a comparison candidate.
`result.preference` and `observations[].candidate` are "the set of things that can win"; the
source is the reference both candidates are judged *against*, and it can never win. So SRC
never enters the record's `candidates[]`, `labels()` stays `[A, B]` (you cannot prefer the
source), and a note made while auditioning SRC is a general observation (null candidate), not
one tagged to a lane. Concretely SRC is a third `Session.source: Option<Candidate>` with label
`"SRC"`, kept beside the `[Candidate; 2]` A/B pair rather than folded into it — the arity the
tool actually has is "two candidates and maybe a reference", and the types say exactly that.

**Does SRC join the match group?** Yes (spec #42, #66): attenuate-to-quietest across A, B,
*and* the source, so the candidates are level-fair against the reference they are compared to.
`match_gains` takes the source's LUFS into the reference search; the A/B gains are then
relative to the quietest of all three. SRC's own row rides `playback.loudness_match.candidates`
keyed by `"SRC"` (an open, label-keyed map — ADR-0003), so the record documents what was done
to every playback lane, not just the two that can win.

**Does SRC join the shuffle / concealment?** No. Concealment is A/B only. The source is the
anchor a blind listener compares against; revealing it discloses nothing about which candidate
is which. So SRC is never permuted, its audio reference is the content hash (not an opaque
token), and it rides the blind session payload fully identified — the same shape a sighted
session serves it. One carve-out: in a blind session SRC's *measured LUFS* is still concealed
(gain-only, like A/B) until the reveal. If SRC is the quietest reference, its measured figure
plus the A/B gains would let a listener back out A/B's measured LUFS — the very fingerprint
#32 hides — so the measurement stays held back even though the identity does not.

**Do the blind refusals extend to SRC?** No. The refusals (`refuse_if_unblindable`) exist
because a duration/rate/channel difference *between the concealed candidates* would have to be
displayed and would identify them. SRC is identified and is allowed to differ (a different
master, a different rate); it joins the sample-locked graph clamped to the shortest lane like
A/B, so a length difference degrades looping gracefully rather than refusing. Playback runs to
the *longest* lane: a shorter one falls silent at its own end, and only the longest ending ends
the transport — otherwise the short lane would stop a transport whose other lanes are still
sounding.

## The SRC auto-resolution rule

Project mode resolves the SRC lane as **the asset that is an input of both candidates'
producing derivations**. A candidate resolved via `name@derivation` has that derivation as its
producer; a bare-slug candidate's producers are every derivation that outputs it. The shared
source is the intersection of the two candidates' producer-inputs, minus the candidates
themselves. Share exactly one → that is SRC; share none → the lane is simply absent, and the
launch *says so* — a note on stderr naming both refs, before the served URL goes to stdout, so
a missing third lane is never something the listener has to infer; share more than one →
ambiguous, refused with the options listed (pass `--exclude-source`). Bare-file mode has no
manifest, so it takes an explicit `--source <path>`; without it there is no SRC lane.
`--exclude-source` says nothing: the listener asked for the absence.

## Consequences

- The M5 migration ADR-0008 anticipated landed as a *signature* change over label-keyed data,
  not a data change: `Session` grew `source`/`source_loudness`, the frontend engine became
  lane-list-based (`LaneId = "A" | "B" | "SRC"`, A/B still the `x` toggle pair), and the
  record schema needed no version bump — `candidate.asset`/`candidate.project` and the open
  `loudness_match.candidates` map were already declared.
- A v0.1 record from before this slice still reads correctly: it has no SRC row and no
  asset/project fields, both of which are optional.
- SRC being a lane but not a candidate is the load-bearing call. It keeps `result.preference`,
  the reference-integrity check, and the blind reveal cleanly A/B, and it is why project-mode
  records carry asset ids on their two candidates without the source needing a record slot of
  its own (the record destination/handover is slice 5).
