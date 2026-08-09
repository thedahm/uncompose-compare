# ADR-0006: Reveal the blind mapping at conclude, not at verdict save

A blind session (issue #29, spec #26) conceals which candidate is which until the listener has
committed. The confirmed #61 prototype (branch `prototype/compare-workbench`, variant D)
revealed the label→file mapping the moment the verdict was **saved**. Compare's shipped verdict
(issue #16) diverges from that prototype in one load-bearing way: a saved verdict stays
**editable** — Save engraves, but nothing is final until the record is written. The spec (#26,
"Reveal at conclude, not verdict save" and "Decisions this repo should ADR") names this timing
divergence as this repo's to own. We decided:

## The decision

**The reveal coincides with the one irreversible event: the successful record write. The
`/record` conclude response carries the label→file mapping, and the UI shows it only then.
Verdict save reveals nothing.**

- Revealing at save would let a listener learn which file won and *then* edit the verdict —
  blind integrity is broken the instant knowledge and revision overlap. Compare's verdict is
  editable right up to conclude (issue #16, its own DoD), so save is the wrong seam. Conclude
  is the only point after which the verdict can no longer change.
- The conclude response returns `reveal`: the same `{label, path, sha256, size}` per candidate
  the record carries, built from the identical trusted-candidate list, so the mapping the UI
  shows can never diverge from what was written. A refused conclude (a second conclude, an
  invalid record, an overwrite) reveals nothing — only a completed write reveals, exactly as
  it is the only thing that concludes the session.
- The reveal is displayed only for a blind session. A sighted session already names the files
  on its lane rows, so there is nothing to reveal; the response still carries the mapping
  (uniform shape), but the UI shows the reveal panel only when the session was blind.

## Consequences

- The prototype's reveal-at-save is deliberately not carried forward; the visual reference for
  the anonymous switch buttons (#61 variant D) is kept, its reveal timing is not.
- `Recorder::conclude` returns a `Conclusion { path, reveal }` rather than a bare path, so the
  reveal is produced at exactly the write that flips the write-once guard. The reveal is the
  trusted candidates verbatim — identity comes from the load, never the browser, so the mapping
  is correct by construction (shared with ADR-0004/0005: concealment is a session concern; the
  record always carries full identities, uncompose#65).
- Enforced end to end: the process-boundary test asserts the conclude response's reveal matches
  the written record; the browser flow spec asserts no basename, path, hash, or size appears in
  the DOM before conclude and that the post-conclude reveal matches the record on disk.
- Loudness figures follow the same reveal timing in blind mode (per-lane LUFS hidden until
  reveal) — that hiding lands with issue #32; #29 covers the identity reveal.
