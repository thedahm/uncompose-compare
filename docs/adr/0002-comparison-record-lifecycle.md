# ADR-0002: Comparison-record destination, single-write, and schema validation

Concluding a session (issue #16, spec #8) writes the immutable comparison record. Three
choices here are the repo's to own — the spec (#8, "Decisions this repo should ADR") names
the destination default and overwrite refusal and the single-write contract explicitly, and
owning the v0 schema forces a validation decision. We decided:

## Destination default and overwrite refusal

**The record is written to `<ULID>.json` in the invoking directory by default, and `--out`
overrides that path. An existing destination is refused, never overwritten.**

- The invoking directory is where a CLI user expects an output artifact to land, and the
  record's own ULID `id` names the file, so the on-disk name and the record's identity are
  the same fact — no separate naming scheme to keep in sync.
- Refusing an existing destination (via `create_new`, which tests and claims the name in one
  step) keeps the "written once, never mutated" promise (CONTEXT.md, uncompose#65) true even
  when a user points two sessions at the same explicit `--out`. Overwriting would let an
  evaluation be silently replaced — exactly the revision-after-the-fact the immutable record
  exists to prevent.
- **The bytes then land atomically**: `create_new` only reserves the name; the record is
  written to a temp sibling and renamed over the reservation, and a failure part-way through
  removes both. Writing in place would leave a truncated record on a full disk — and, worse,
  a reservation that makes every retry look like the overwrite case, so the session could
  never conclude. The reserve-and-rename split keeps the existence check above *and* the
  fix-and-retry contract below true at the same time.

## Single write per session

**The record endpoint accepts exactly one successful write. A second conclude — or any
conclude after a write — is refused (409).**

The server holds a write-once flag flipped only after a record is successfully written; the
single-threaded request loop makes a plain `Cell` sufficient. A conclude that fails
validation or hits an existing destination does *not* flip it, so the listener can fix the
verdict (everything stays editable until a record exists, per #61) and retry. An abandoned
session — server killed, tab closed, never concluded — writes nothing, because writing is
the conclude action and nothing else triggers it.

> **Amended by ADR-0010 (M5 slice 5).** A *successful* conclude now ends the session in both
> modes: the response is delivered, the server shuts down, and the process exits. A second
> conclude is therefore impossible rather than refused (409) — the write-once flag stays a
> within-session invariant guarding the request loop. Refusal-then-retry above is unchanged.

## Validation against the embedded schema

**The v0 JSON Schema lands in-repo (`schemas/compare/v0/uncompose.compare.schema.json`) as
the owned artifact, is embedded in the binary, and the record endpoint validates every
assembled record against those exact bytes** (via the `jsonschema` crate, `default-features`
off so no HTTP resolver is pulled — the zero-network-I/O contract stays intact, and the
self-contained schema needs no external `$ref` resolution).

- Validating against the committed schema file — rather than re-encoding its rules in Rust —
  means the published schema and the server's enforcement cannot drift (the concern
  AGENTS.md's documentation policy names). The `schema` field is pinned in the schema with
  `const`, so the id a record claims is the id the schema declares — the server reads it out
  of the embedded bytes rather than repeating it as a literal.
- The server, not the browser, supplies the record's `schema`, `id`, timestamps,
  `candidates[]` (path/sha256/size), `mode`, and `playback`; the browser POSTs only the
  session-authored `result`, `observations`, `loops`, and `context`. Hashes therefore
  provably match the loaded inputs — a forged body cannot rewrite a candidate's identity.

### Considered options

- **Hand-rolled validation in Rust** (no schema-validator dependency, matching the repo's
  dependency-light JSON-output code): rejected because it duplicates the schema's rules in
  code and invites drift; the whole point of owning a published schema is that it is the
  single source of truth.
- **`jsonschema` with default features**: rejected — it pulls `reqwest` and an async HTTP
  stack to resolve remote `$ref`s, which no self-contained local schema needs and which
  would put a network client in a binary whose defining promise is zero network I/O.

We also **encoded the "confidence required iff a preference is chosen" rule** (which the
uncompose#65 draft states in prose but did not express) as an `if/then/else` in the owned
schema, so validation enforces the full result contract rather than leaving that one rule to
application code. This is a completion of the draft, not a divergence from it; the draft's
resolution is still the normative source for everything else.

## Consequences

- A record's filename is its ULID; downstream tools (Project's import, M5) key on the
  record `id`, which is the same value. In project mode the record lands under
  `<root>/evaluations/` and is auto-imported at conclude (ADR-0010).
- Pointing `--out` at an existing file is a hard error with a clear message, not a clobber.
- The schema file is now load-bearing: changing it changes what the server accepts, and the
  `schema` `$id` string is duplicated as a constant in the binary (the value written into
  every record). Both are covered by the process-boundary tests.
