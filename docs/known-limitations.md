# Known limitations

Honest gaps in `uncompose-compare` v0.1, so you know what you're getting before
you install rather than after. Everything here is a deliberate v0.1 scope call,
not an oversight; each is tracked as a backlog issue for anyone who wants to pick
it up.

## Linux only

The wheel embeds a Vite-built frontend via `rust-embed` and is built and
published for Linux only. macOS and Windows are untested and unsupported for
now.

## Exactly two candidates

A comparison is always between exactly two candidates, A and B. There's no
third (or Nth) candidate slot in v0.1 — see
[ADR-0008](adr/0008-two-candidate-arity.md) for why the arity is fixed
deliberately rather than left unaddressed. The SRC lane (the shared source both
candidates derive from, when project mode can resolve one) plays alongside A/B
but is a lane, not a candidate: it never enters the record's `candidates[]` and
never wins a preference.

## No schema migration machinery

The comparison-record schema sits at `v0` for the whole pre-1.0 period, and
breaking changes may ship between releases without a URL bump (see the
[v0.1.0 release notes](releases/v0.1.0.md) and
[uncompose#64](https://github.com/thedahm/uncompose/issues/64)). There's no
`migrate` command in v0.1: a `schema` URL this tool doesn't recognize is
refused outright, never best-effort read. If your record predates a breaking
change, the remedy is to regenerate it or hand-edit it to match the current
schema.
