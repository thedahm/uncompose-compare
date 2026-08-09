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
`v0.1.0` tag.

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
uncompose-compare cache clear
```

- `--port` pins the loopback port instead of taking an ephemeral one.
- `--out` writes the concluded comparison record to this path instead of `<ULID>.json` in
  the directory you ran from. An existing destination is refused, never overwritten.
- `--cache-max-bytes` caps the playback-proxy cache, pruned least-recently-used at
  startup. It defaults to 2 GiB — the flag is here because a cache that can grow to
  gigabytes of your disk should be yours to bound.
- `cache clear` deletes every cached proxy and reports what it removed.
