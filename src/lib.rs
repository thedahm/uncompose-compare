//! uncompose-compare's core: everything the `uncompose-compare` command does
//! once its arguments are parsed.
//!
//! `uncompose-compare <a> <b>` loads two audio files as candidates A and B (in
//! argument order), hashing and decoding each at startup, then serves the
//! embedded UI over the guarded loopback server (#72) plus a `/session`
//! endpoint that reports the metadata the workbench needs.
//!
//! The modules split along the seams the binary actually has:
//!
//! - [`audio`] — decode a source to integer PCM and encode a lossless proxy.
//! - [`cache`] — the XDG content-hash proxy cache (populate, reuse, prune, clear).
//! - [`session`] — the two loaded candidates and the content-hash → proxy table.
//! - [`record`] — the v0 comparison record: assemble, validate, write once.
//! - [`server`] — the loopback server, its privacy contract, and the endpoints.
//!
//! The CLI over this core (`src/main.rs`) only parses arguments, wires these
//! together, and formats output.

pub mod audio;
pub mod cache;
pub mod record;
pub mod server;
pub mod session;

/// Lowercase hex-encode a byte slice.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Fill `buf` with OS randomness. The tool is Linux-only (spec #1), so
/// `/dev/urandom` is a fine, dependency-free source — and one place, so the
/// session token, the record ULID, and the blind shuffle/opaque references all
/// draw from the same reader and each map the one `io::Error` into its own
/// error type.
pub fn random_bytes(buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)
}
