# Uncompose Compare

Local-first, two-file audio comparison for honest listening tests.

## Agent skills

### Issue tracker

Issues and PRDs live as GitHub issues on `thedahm/uncompose-compare`, managed with the
`gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles, using their default label strings. See
`docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` and `docs/adr/` at the repo root. See
`docs/agents/domain.md`.

### AFK workflow

Sandcastle drains `ready-for-agent` tickets on named branches; humans merge. See
`docs/agents/afk-workflow.md`.

## Documentation policy

Commit only documentation that code alone can't explain: decisions, tradeoffs, and
constraints (ADRs are the canonical form). Research and findings go to issues or the wiki,
never into the repo. See `CLAUDE.md`.
