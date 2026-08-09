# Contributing to Uncompose Compare

Thanks for your interest in Uncompose Compare. The project is pre-v0.1 and this guide is
intentionally minimal. It grows into a full contributor guide once v0.1 exists.

## Working on the code

The repo is a single Rust binary crate that embeds a Vite/React frontend and serves it
over a guarded loopback server. Two toolchains:

- **Rust side**: a stable toolchain. `cargo fmt`, `cargo clippy`, and `cargo test` at the
  repo root run the fmt/lint/test lanes. The binary embeds `frontend/dist` at build time
  (`build.rs` fails loudly if the bundle is absent), so build the frontend first — the
  root `npm run build` / `npm run test` scripts do this for you.
- **Frontend side**: Node 22 and `npm`. `npm --prefix frontend install`, then
  `npm --prefix frontend run lint | typecheck | test | build`. The frontend carries only
  the Web Audio plumbing the cross-engine sync harness drives; the audio graph, not the
  DOM, is the thing under test.

From the repo root, `npm run typecheck` and `npm run test` are the canonical gates (the
latter builds the frontend, then runs `cargo test`). `npm run build` produces the release
binary; `npm run wheel` packages it as a maturin wheel.

## Testing at the seams

Uncompose Compare is tested at two boundaries a user or downstream tool can actually
observe, never at internals:

- **The installed binary's process boundary** (`tests/cli.rs`): spawn the binary the way
  a machine runs it and assert exit codes, messages, the tokened loopback URL, the
  privacy refusals, and the written comparison record.
- **The served page in real browsers** (`harness/`): the three-engine (Chromium, Firefox,
  WebKit) sync matrix that keeps the sync contract a tested fact on every push, plus
  workbench flow specs driving the DoD slice end to end.

Fixtures are deterministic seeded noise generated at test time — never committed audio.

## Governance

Uncompose Compare is created and maintained by Dominic Hanzely
([@thedahm](https://github.com/thedahm)), who acts as the project's maintainer and final
decision-maker. Significant decisions this repo owns are recorded as numbered architecture
decision records in [`docs/adr/`](docs/adr/); ecosystem-wide decisions live in
[`thedahm/uncompose`](https://github.com/thedahm/uncompose)'s ADR series and are
referenced explicitly. Issues and pull requests are answered on a best-effort basis.

## Documentation carries rationale, not narration

Code is the source of truth for what the project does; committed documentation exists to
carry what code cannot: the reasoning, the constraints, and the roads not taken. ADRs in
[`docs/adr/`](docs/adr/) are the home for "we did X instead of Y because". Comments state
constraints the code can't show. Research, findings, and spike write-ups live on the issue
tracker or the wiki, never in the repo, so committed docs can't drift from the code.

## Before opening a large pull request

Open an issue first. Discussing the change before you build it keeps you from investing
effort in something that conflicts with a recorded decision or the current milestone.
Small fixes (typos, broken links, obvious corrections) are welcome directly.

## Conduct

Participation in the project is covered by the [Code of Conduct](CODE_OF_CONDUCT.md).
