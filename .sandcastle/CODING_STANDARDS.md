# Coding Standards

## Style

- Rust, rustfmt defaults, clippy clean (`-D warnings`). CI enforces both.
- Error messages are actionable: what failed, why, what to do next. Errors on stderr, meaningful exit codes.
- No abstraction layers introduced "for testability" or "for later". Plain functions, small modules.

## Testing

- Default seam is the CLI process boundary: run the compiled `uncompose-project` binary in a real temp dir; assert exit codes, stdout/stderr, and the bytes of `uncompose.project.json`. Every feature is tested here unless genuinely awkward.
- Unit tests only for edge cases the CLI seam reaches poorly (slug disambiguation, path validation). Still behavior only, never internals.
- No mocks, no filesystem abstractions. Real files, real temp dirs.
- Manifests produced in tests are validated against the JSON Schema.

## Architecture

- Thin CLI over a core library crate. The CLI parses args and formats output; the core owns manifest semantics.
- Integrity is derived, never stored (uncompose #62). At most a cached `last_verified` per asset.
- Manifest written canonically: fixed field order, 2-space indent, trailing newline, atomic temp+rename.
- Unknown plain fields are invalid; `ext` subtrees pass through rewrites untouched (uncompose #64).
- Decisions of record live in `docs/adr/`. If code contradicts an ADR, flag it, don't silently override.
