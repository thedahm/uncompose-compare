# uncompose-compare

## Documentation policy

Commit only documentation that code alone can't explain: decisions, tradeoffs, constraints ("we considered X, chose Y because Z" — ADRs are the canonical form). Research, findings, and spike write-ups go to issues or the wiki, never into the repo, so committed docs can't drift from or duplicate the actual implementation.

## Agent skills

### Issue tracker

Issues live in GitHub Issues for `thedahm/uncompose-compare` (use the `gh` CLI). See `docs/agents/issue-tracker.md`.

### Triage labels

Default label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
