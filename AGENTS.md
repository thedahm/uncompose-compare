# Uncompose Compare

Local-first, two-file audio comparison for honest listening tests.

This is the single source of agent instructions for the repo; `CLAUDE.md` points here.

## Agent skills

### Issue tracker

Issues and PRDs live as GitHub issues on `thedahm/uncompose-compare`, managed with the
`gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles, using their default label strings — `needs-triage`,
`needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See
`docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` and `docs/adr/` at the repo root. See
`docs/agents/domain.md`.

### AFK workflow

Sandcastle drains `ready-for-agent` tickets on named branches; humans merge. See
`docs/agents/afk-workflow.md`.

## Documentation policy

Commit only documentation that code alone can't explain: decisions, tradeoffs, and
constraints ("we considered X, chose Y because Z" — ADRs are the canonical form).
Research, findings, and spike write-ups go to issues or the wiki, never into the repo, so
committed docs can't drift from or duplicate the implementation.

## Repo checks

Every change runs the root gates before landing: `npm run typecheck` and `npm run test`
(the latter builds the frontend, then runs `cargo test`). CI runs those plus `cargo fmt` /
`clippy`, the frontend lint and build, and the packaging and sync-harness jobs.
