# AFK workflow: Sandcastle

[Sandcastle](https://github.com/mattpocock/sandcastle) drains `ready-for-agent` tickets
without a human at the keyboard. Wayfinder remains the planning flow; Sandcastle only
executes fully specified tickets.

## Shape

- **Provider**: Docker. `.sandcastle/Dockerfile` documents the contributor toolchain
  (stable Rust + Node 22 + Python/maturin). Worktrees warm `node_modules`,
  `frontend/node_modules`, and `target` so each cycle starts with caches primed.
- **Tracker**: GitHub Issues, filtered to the `ready-for-agent` label
  (`docs/agents/triage-labels.md`).
- **Template**: spec-delivery loop (`.sandcastle/main.mts`), ported from
  `uncompose-project`'s M2. It picks the next open sub-issue of a spec, claims it by
  assignment at pickup, implements on a named `sandcastle/issue-<n>` branch, and opens a
  per-sub-issue PR that is merged via `gh pr merge --merge`. Once the sub-issue API reports
  the spec's children complete, it finalizes: a spec PR against `main`, a Fable-5 review
  comment, and an Opus-5 address round. A configurable `MAX_PARALLEL` cap (default 2)
  bounds concurrent issue work. The agent does not close the parent spec; a human does.

The root `package.json` exists only to host this workflow's dependencies; the shipped
product is the Rust binary plus its embedded `frontend/`.

## Repo checks

Every cycle runs this repo's root gates before landing: `npm run typecheck` and
`npm run test` (the latter builds the frontend, then runs `cargo test`).

## Running it

One-time setup:

1. `npm install`
2. `cp .sandcastle/.env.example .sandcastle/.env` and fill in `CLAUDE_CODE_OAUTH_TOKEN`
   (from `claude setup-token`) and `GH_TOKEN`.
3. `npx @ai-hero/sandcastle docker build-image`

Then run the loop. Logs land in `.sandcastle/logs/`.
