# ADR-0010: The evaluations handover and the conclude lifecycle

M5 slice 5 (issue #41, spec #42; contracts uncompose#67 res. 1, 5, 7, 8, 9) closes the
project loop: a project-mode conclude writes the record into the project and registers it,
in one act. Two of the choices here amend ADR-0002 — the record destination in project mode
and, more consequentially, the session lifecycle — so they are recorded here rather than
edited into it.

## The record destination in project mode

**A project-mode record is written to `<root>/evaluations/<record-ulid>.json`**, the
`evaluations/` directory created if needed. Standalone destination behavior is unchanged:
`<ULID>.json` in the invoking directory, or `--out`. The same single-write discipline
carries over verbatim — `create_new` reservation, temp sibling, rename over the reservation,
both halves undone on failure (ADR-0002). Only the directory the record lands in changes;
`--out` and `--project` are mutually exclusive (ADR-0009), so there is one destination per
session and no way to point a project session somewhere else.

The project root is made absolute before it is stored, so both the on-disk destination and
the handover's argv below carry absolute paths regardless of how `--project` was spelled.

## The conclude lifecycle: save-and-close is one act

**After the conclude response is delivered, the server shuts down and the process exits —
in *both* modes.** This intentionally replaces M3's serve-forever-after-conclude (#67
res. 8: save and close are one act). The exit code tells the truth about what happened:

- **0** — the record was saved (standalone) and, in project mode, registered.
- **nonzero** — the auto-import failed (project mode). The record is still written.

The response is written to the browser *first*, then the loop breaks and the process exits,
so the closing screen always arrives before the server goes away. A refused conclude —
schema-invalid, an existing destination, a dangling candidate reference — writes nothing and
does *not* end the session: the listener can fix the cause and conclude again, exactly as
before. An abandoned session (killed before any conclude) still writes nothing. Changing a
saved verdict remains a new session and a second evaluation entry — the immutable record is
never revised in place.

Consequences worth stating: a second conclude is no longer a 409, it is simply impossible
(the process is gone), so the old "second conclude is refused" test is superseded by one
asserting the process exits. The write-once `Cell` still guards the single request loop —
it is now a within-session invariant that never has to survive a second conclude.

### Considered alternatives

- **Serve forever after conclude (M3's behavior), in both modes.** Rejected: it leaves a
  live server and a live process after the one irreversible act is done, and — in project
  mode — no natural place to surface the exit code the caller (a wrapping `uncompose`
  command, a script) needs to know whether the evaluation registered. Save-and-close makes
  the process's exit the answer.
- **Exit only in project mode; keep serving standalone.** Rejected: two lifecycles for one
  action is the kind of split that quietly rots. #67 res. 8 makes it one act; the standalone
  session closing too is the honest reading, and the abandoned-session guarantee already
  holds either way.

## The auto-import handover

**After the record is written, project mode invokes the pinned argv
`uncompose-project import --project <abs-root> <abs-record>`** (both paths absolute). The
tool is resolved off PATH — its presence was pre-flighted before the server bound
(ADR-0009), so a session that could never register never started.

**Failure semantics (#67 res. 7): the record file is never the casualty.** On an import
failure — a nonzero exit, or a tool that will not run — the record is *kept*, the tool's
stderr is relayed, the process exits nonzero, and the exact recovery command
(`uncompose project import <record>`) is printed **last**. No retries, no rollback: undoing
the write to "match" a failed import would throw away the very evaluation the listener just
made, which is the opposite of what an immutable record is for.

**The registration outcome rides the conclude response**, alongside the M4 reveal, so the
closing screen can show "registered" or the recovery command without a second request. A
standalone session hands the record to no one, so it carries no registration key at all —
the same "no more than needed" shape the reveal follows (ADR-0006): the field is present
exactly when there is a handover to report.

The handover runs *synchronously inside conclude*, before the response is built, so the
outcome the browser sees and the exit code the process returns are the same fact computed
once. In tests it runs against a stub `uncompose-project` on a synthetic PATH (the fake-tool
pattern) that records its argv and exits as directed — this repo does not wait on Project's
real evaluation import (uncompose-project#25) to certify the seam.
