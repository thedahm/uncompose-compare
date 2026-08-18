# Uncompose Compare

Honest, local-first A/B listening for two audio files.

When you have produced two derived versions of a piece of audio — two separations, two
masters, two renders — there is no honest way to judge them in an ordinary player:
switching files loses your position, hides small differences behind resync gaps, and
leaves no trace of what you heard or decided. Uncompose Compare loads exactly two files,
lets you listen to the same moment in either one with zero drift, loop the region where
the difference lives, write down what you hear as you hear it, pick a winner, and keep a
trustworthy record of the whole evaluation.

Everything runs locally: your audio never leaves your machine, and you are responsible for
having the rights to the audio you process.

## Status

Pre-v0.1: the first release is being built in the open on the
[issue tracker](https://github.com/thedahm/uncompose-compare/issues), with decisions this
repo owns recorded in [`docs/adr/`](docs/adr/). The install line below goes live with the
`v0.1.0` tag. See the [release notes](docs/releases/v0.1.0.md) and
[known limitations](docs/known-limitations.md) for what v0.1 does and does not do.

## Install

Uncompose Compare is part of the [Uncompose](https://github.com/thedahm/uncompose) family.
It installs as the `uncompose-compare` command and is also reachable through the
`uncompose` dispatcher as `uncompose compare`:

```sh
# placeholder — goes live with v0.1.0
uncompose compare a.wav b.wav
```

That loads `a.wav` and `b.wav` as candidates A and B and opens the listening workbench in
your browser.

## Command line

```
uncompose-compare <A> <B> [--port <PORT>] [--out <PATH>] [--cache-max-bytes <BYTES>]
                          [--project <DIR>] [--source <PATH>] [--exclude-source]
                          [--loudness-match] [--blind]
uncompose-compare cache clear
```

- `--port` pins the loopback port instead of taking an ephemeral one.
- `--out` writes the concluded comparison record to this path instead of `<ULID>.json` in
  the directory you ran from. An existing destination is refused, never overwritten.
  Conflicts with `--project` (project records' destination is the handover's).
- `--cache-max-bytes` caps the playback-proxy cache, pruned least-recently-used at
  startup. It defaults to 2 GiB — the flag is here because a cache that can grow to
  gigabytes of your disk should be yours to bound.
- `--project <DIR>` runs in project mode: the two positionals are resolved as manifest
  refs against `<DIR>/uncompose.project.json` instead of as file paths. A ref is an
  asset id, or `<name>@<derivation>`. The candidates' shared source becomes the SRC
  lane unless `--exclude-source` is passed.
- `--source <PATH>` supplies the shared source for the SRC lane in bare-file mode: a
  third, always-identified lane that joins the sample-locked graph and the loudness
  match group. In project mode the source is auto-resolved instead, so this conflicts
  with `--project`.
- `--exclude-source` (project mode only) omits the SRC lane even when the candidates
  share a source.
- `--loudness-match` measures ITU-R BS.1770 integrated loudness per candidate at load
  and attenuates the louder lane down to the quietest (gain-only, never boost). Off by
  default — faithful as-is playback is the baseline.
- `--blind` shuffles the label↔file assignment by an OS coin flip at load and conceals
  every identifying detail (name, path, hash, size) from the served surface, so a
  preference is judged without knowing which file is which. Refuses identical content
  or a duration/rate/channel mismatch before binding. The written record still carries
  full identities.
- `cache clear` deletes every cached proxy and reports what it removed.
