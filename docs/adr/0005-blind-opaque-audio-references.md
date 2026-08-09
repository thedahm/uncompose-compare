# ADR-0005: Opaque per-session audio references for blind sessions

A blind session (issue #28, spec #26) must reveal nothing about which candidate is which. The
sighted `/audio/<sha256>` scheme (#11) names each proxy by its source content hash — which a
listener who owns the inputs can reproduce by hashing their own files, decoding the shuffle.
The spec (#26, "Decisions this repo should ADR") names the opaque-reference scheme as this
repo's to own. We decided:

## The decision

**In a blind session each candidate's audio is served under a per-session opaque reference —
16 bytes of OS randomness, hex-encoded — minted at load and never derived from the content.
The content-hash URL is not servable in blind mode. Sighted sessions are unchanged: they key
by `sha256`.**

- `Candidate.audio_ref` is the key the audio endpoint resolves through. Sighted: it is the
  source `sha256`, so the served payload and the endpoint are byte-for-byte what they were.
  Blind: it is the opaque token, so no content-derived identifier appears in any blind-session
  response.
- The proxy table (`Session.proxies`) is keyed by `audio_ref`, not by `sha256`. A blind
  session therefore has no `/audio/<sha256>` entry at all: a request for the content-hash URL
  is a plain 404, exactly like any unknown reference. The listener cannot confirm a guess by
  hashing an input and asking for its proxy.
- The proxy still lives in the content-hash cache on disk (`<sha256>.flac`), because the cache
  is shared across sessions and keyed by content by design (#74). Only the *served reference*
  is opaque; the on-disk filename is never exposed in an HTTP response, and the concealment
  contract covers responses, not the local cache.

## The concealment contract

The blind session payload carries per candidate only: `label`, the shared `duration_ms`, and
the `/audio/<opaque>` reference. Name, path, sha256, size, and per-candidate technical metadata
are omitted — the browser never receives identity it must not show. This is enforced as a
tested fact at the process boundary (spec testing decision 1): a concealment-leak sweep asserts
no basename, path, sha256 hex, or decimal size of either input appears in the session payload,
any audio URL, or any blind-session response body.

## Consequences

- The audio endpoint is unchanged in shape — it still resolves a path segment through
  `proxies` and 404s an unknown one — but the segment is now a per-session reference, opaque in
  blind mode and the content hash in sighted mode. Path safety is still by construction: only a
  reference the load already inserted resolves.
- The record is unaffected: it carries the real `sha256` per candidate (identities always, by
  uncompose#65), independent of the opaque reference the *session* served audio under. The
  opaque reference is a session artifact and is never written to disk.
- Adding a third lane (M5's SRC) needs one more opaque reference at load, no shape change.
