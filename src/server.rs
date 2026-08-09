//! The guarded loopback server and its endpoints (#72).
//!
//! The privacy contract holds on every response — loopback bind, per-session
//! token, Host check, blanket `Cache-Control: no-store`, constant-time token
//! compare — and covers the `/session`, `/audio/<reference>`, and `/record`
//! endpoints as well as the embedded bundle.

use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;

use rust_embed::RustEmbed;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::hex;
use crate::record::Recorder;
use crate::session::Session;

/// The Vite/React bundle, embedded at compile time. `build.rs` guarantees the
/// folder is present and non-empty, so a build that reaches here has assets.
#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

/// Bind the loopback server. `port` is the `--port` pin; 0 asks the OS for an
/// ephemeral one. The server is never exposed beyond this machine.
pub fn bind(port: u16) -> Result<(Server, u16), Box<dyn std::error::Error + Send + Sync>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let server = Server::http(addr).map_err(|e| format!("cannot bind {addr}: {e}"))?;
    let bound = server
        .server_addr()
        .to_ip()
        .ok_or("server bound to a non-IP address")?
        .port();
    Ok((server, bound))
}

/// Resolve the request against the session, record, and audio endpoints and the
/// embedded bundle, enforcing the #72 contract (Host check, then token) before
/// serving anything.
pub fn serve(mut request: Request, token: &str, session: &Session, recorder: &Recorder) {
    // DNS-rebinding guard: only a loopback Host is ever honored. A page on
    // another origin that resolves its name to 127.0.0.1 still sends its own
    // Host, so this refuses it before any asset is touched.
    if !host_is_loopback(&request) {
        return refuse(request);
    }

    // The token may arrive as a `?token=` query param (the printed URL) or as
    // the cookie the page seeds for its sub-resource requests. Either presenting
    // the right value authorizes the request; a wrong or missing one refuses it.
    let raw = request.url().to_string();
    let (path, query) = raw.split_once('?').unwrap_or((raw.as_str(), ""));
    let matches = |t: &str| ct_eq(t.as_bytes(), token.as_bytes());
    let query_ok = query_token(query).is_some_and(matches);
    let cookie_ok = cookie_token(&request).is_some_and(matches);
    if !query_ok && !cookie_ok {
        return refuse(request);
    }

    let path = path.trim_start_matches('/');

    // The session endpoint: the workbench's source of truth for candidate
    // metadata. Same no-store guarantee as every other response.
    if path == "session" {
        let _ = request.respond(json_response(200, &session.to_json()));
        return;
    }

    // The record endpoint: an explicit conclude POSTs the concluded session
    // here. The server validates it against the owned schema, stamps completion,
    // and writes the immutable record exactly once (issue #16). A second conclude
    // — or any conclude after a successful write — is refused.
    if path == "record" && *request.method() == Method::Post {
        let mut body = String::new();
        if request.as_reader().read_to_string(&mut body).is_err() {
            let _ = request.respond(json_response(
                400,
                &json!({"error": "unreadable request body"}),
            ));
            return;
        }
        let (status, payload) = match recorder.conclude(session, &body) {
            // The reveal (label→file) rides the successful-write response and
            // nothing before it — the browser had no identity until now (#29).
            Ok(c) => (200, json!({ "path": c.path, "reveal": c.reveal })),
            Err(e) => (e.status(), json!({ "error": e.to_string() })),
        };
        let _ = request.respond(json_response(status, &payload));
        return;
    }

    // Audio proxy endpoint: resolve strictly through the per-session reference
    // table, so a request names a reference we already loaded — never a
    // filesystem path. Sighted sessions key by source hash; blind sessions key
    // by an opaque per-session reference, so a content hash is never servable
    // (#28) and the shuffle cannot be decoded. An unknown reference is a 404,
    // not a chance to read arbitrary files.
    if let Some(reference) = path.strip_prefix("audio/") {
        let response = match session.proxies.get(reference) {
            Some(proxy) => match std::fs::read(&proxy.path) {
                Ok(data) => Response::from_data(data)
                    .with_header(header("Content-Type", proxy.container.content_type()))
                    .with_header(header("Cache-Control", "no-store")),
                // A proxy pruned out from under us resolves like an unknown
                // reference.
                Err(_) => not_found(),
            },
            None => not_found(),
        };
        let _ = request.respond(response);
        return;
    }

    // Map "/" to the SPA entry point.
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
        None => not_found(),
    };

    // A broken client connection is not our problem to recover from.
    let _ = request.respond(response);
}

/// A JSON response, uncached like every other response.
fn json_response(status: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
}

/// A 404 for an unknown asset or audio reference — like every other response,
/// never cached.
fn not_found() -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string("not found")
        .with_status_code(404)
        .with_header(header("Cache-Control", "no-store"))
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
fn cookie_token(request: &Request) -> Option<&str> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Cookie"))
        .and_then(|h| {
            h.value
                .as_str()
                .split(';')
                .map(str::trim)
                .find_map(|kv| kv.strip_prefix("token="))
        })
}

/// A per-session token: 16 bytes of OS randomness, hex-encoded. The tool is
/// Linux-only (spec #1), so `/dev/urandom` is a fine, dependency-free source.
pub fn session_token() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
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
