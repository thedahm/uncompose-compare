# AFK workflow: Sandcastle

[Sandcastle](https://github.com/mattpocock/sandcastle) drains `ready-for-agent` tickets
without a human at the keyboard. Wayfinder remains the planning flow; Sandcastle only
executes fully specified tickets.

What the loop actually does — its phases, models, and defaults — is described where it is
implemented, in `.sandcastle/main.mts`. This file records the choices around it.

## Choices

- **Provider: Docker**, not a hosted sandbox. `.sandcastle/Dockerfile` doubles as the
  contributor toolchain record (stable Rust + Node 22 + Python/maturin), so the thing
  agents build in is the thing a human can reproduce.
- **Tracker: GitHub Issues**, filtered to `ready-for-agent` (`docs/agents/triage-labels.md`)
  — the same queue a human works, not a parallel one.
- **Branch per sub-issue, PR per sub-issue.** Every unit of agent work leaves a paper
  trail that can be read and reverted on its own.
- **Humans merge the spec PR and close the parent spec.** The loop opens and reviews; the
  decision to ship stays with a person.

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
