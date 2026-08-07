//! Spike 3: minimal server privacy contract.
//!
//! Serves the Vite/React stub bundle — embedded into this binary via
//! rust-embed — over a loopback-only HTTP server on an ephemeral port, honoring
//! the minimal #72 privacy contract decided in `thedahm/uncompose`:
//!
//!   * bind 127.0.0.1 on an OS-assigned ephemeral port (never exposed off-box);
//!   * print a URL carrying a per-session token, and refuse any request that
//!     does not present it (via query string or the cookie the page seeds);
//!   * refuse any request whose Host header is not 127.0.0.1 (a DNS-rebinding
//!     guard — a malicious page can't drive this server through the browser);
//!   * send `Cache-Control: no-store` on *every* response — served, refused, or
//!     not-found — so nothing this server emits is ever cached.
//!
//! Scope is still the packaging spike (spec #1): serve the embedded page and
//! answer `--version`/`--help`, nothing more.

use std::fs::File;
use std::io::{Read, Write};

use clap::Parser;
use rust_embed::RustEmbed;
use tiny_http::{Header, Request, Response, Server};

/// The Vite/React stub bundle, embedded at compile time. `build.rs` guarantees
/// the folder is present and non-empty, so a build that reaches here has assets.
#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

/// uncompose-compare — serve the embedded UI over loopback.
#[derive(Parser)]
#[command(name = "uncompose-compare", version, about, long_about = None)]
struct Cli {}

fn main() {
    Cli::parse();

    if let Err(err) = run() {
        eprintln!("uncompose-compare: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Loopback bind on an ephemeral port: the OS hands us a free port and we
    // never expose the server beyond this machine.
    let server = Server::http("127.0.0.1:0")?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or("server bound to a non-IP address")?
        .port();

    // Per-session token: the printed URL is the only thing that carries it, so
    // knowing the port alone is not enough to talk to the server.
    let token = session_token()?;

    let url = format!("http://127.0.0.1:{port}/?token={token}");
    println!("{url}");
    std::io::stdout().flush()?;

    for request in server.incoming_requests() {
        serve(request, &token);
    }

    Ok(())
}

/// Resolve the request against the embedded bundle and reply, enforcing the
/// #72 contract (Host check, then token) before serving anything.
fn serve(request: Request, token: &str) {
    // DNS-rebinding guard: only a loopback Host is ever honored. A page on
    // another origin that resolves its name to 127.0.0.1 still sends its own
    // Host, so this refuses it before any asset is touched.
    if !host_is_loopback(&request) {
        return refuse(request);
    }

    // The token may arrive as a `?token=` query param (the printed URL) or as
    // the cookie the page seeds for its sub-resource requests.
    let raw = request.url().to_string();
    let (path, query) = match raw.split_once('?') {
        Some((path, query)) => (path, query),
        None => (raw.as_str(), ""),
    };
    let query_ok = query_token(query).is_some_and(|t| ct_eq(t.as_bytes(), token.as_bytes()));
    let cookie_ok = cookie_token(&request).is_some_and(|t| ct_eq(t.as_bytes(), token.as_bytes()));
    if !query_ok && !cookie_ok {
        return refuse(request);
    }

    // Map "/" to the SPA entry point.
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    let response = match Assets::get(path) {
        Some(file) => {
            let content_type = file.metadata.mimetype();
            Response::from_data(file.data.into_owned())
                .with_header(header("Content-Type", content_type))
                .with_header(header("Cache-Control", "no-store"))
                // Seed the token so the browser can authenticate the
                // sub-resource requests it makes without a query of its own.
                .with_header(header(
                    "Set-Cookie",
                    &format!("token={token}; Path=/; SameSite=Strict"),
                ))
        }
        None => Response::from_string("not found")
            .with_status_code(404)
            .with_header(header("Cache-Control", "no-store")),
    };

    // A broken client connection is not our problem to recover from.
    let _ = request.respond(response);
}

/// Refuse a request that fails the #72 contract: 403, no body of substance, and
/// — like every other response — never cached.
fn refuse(request: Request) {
    let response = Response::from_string("forbidden")
        .with_status_code(403)
        .with_header(header("Cache-Control", "no-store"));
    let _ = request.respond(response);
}

/// True when the request's Host header names the loopback address (with or
/// without a port). A missing Host — illegal under HTTP/1.1 — is refused.
fn host_is_loopback(request: &Request) -> bool {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str())
        .map(|host| host.rsplit_once(':').map_or(host, |(name, _)| name) == "127.0.0.1")
        .unwrap_or(false)
}

/// Extract the `token` value from a `&`-separated query string, if present.
fn query_token(query: &str) -> Option<&str> {
    query.split('&').find_map(|kv| kv.strip_prefix("token="))
}

/// Extract the `token` value from the request's Cookie header, if present.
fn cookie_token(request: &Request) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Cookie"))
        .and_then(|h| {
            h.value
                .as_str()
                .split(';')
                .map(str::trim)
                .find_map(|kv| kv.strip_prefix("token=").map(str::to_string))
        })
}

/// A per-session token: 16 bytes of OS randomness, hex-encoded. The spike is
/// Linux-only (spec #1), so `/dev/urandom` is a fine, dependency-free source.
fn session_token() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Constant-time equality so token checking leaks no timing signal.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn header(field: &str, value: &str) -> Header {
    Header::from_bytes(field.as_bytes(), value.as_bytes())
        .expect("static header field/value are always valid")
}
