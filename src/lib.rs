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
