//! Integration tests at the CLI process boundary — the one seam this spike
//! verifies. Every test runs the compiled binary the way a user's machine
//! would and asserts only externally observable behavior (exit codes, stdout,
//! HTTP responses and headers).
//!
//! Issue #4 tightens the served surface to the minimal #72 privacy contract:
//! the printed URL carries a per-session token, requests without it (or with a
//! non-127.0.0.1 Host) are refused, and every response — served, refused, or
//! not-found — carries blanket no-store headers.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_uncompose-compare");

/// A launched server plus the loopback address and session token it printed.
/// Killed on drop so a failing assertion never leaks a process.
struct Serving {
    child: Child,
    addr: String,
    token: String,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Launch the binary and parse the `http://127.0.0.1:<port>/?token=<tok>` line
/// it prints, returning both the loopback address and the session token.
fn launch() -> Serving {
    let mut child = Command::new(BIN)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to launch binary");

    let stdout = child.stdout.take().expect("child stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("read URL line");

    let url = line.trim();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "expected a loopback URL, got {url:?}"
    );

    // The privacy contract (#72): the printed URL carries a session token.
    let (base, query) = url
        .split_once('?')
        .unwrap_or_else(|| panic!("printed URL should carry a token query, got {url:?}"));
    let token = query
        .strip_prefix("token=")
        .unwrap_or_else(|| panic!("URL query should be token=<...>, got {query:?}"))
        .to_string();
    assert!(!token.is_empty(), "session token must be non-empty");

    let addr = base
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();

    Serving { child, addr, token }
}

/// Minimal HTTP/1.1 GET over a fresh connection with full control over the Host
/// header and an optional Cookie. Returns (status, lowercased-headers, body).
fn request(addr: &str, path: &str, host: &str, cookie: Option<&str>) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let cookie_line = match cookie {
        Some(c) => format!("Cookie: {c}\r\n"),
        None => String::new(),
    };
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{cookie_line}Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("write request");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response has header/body separator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = raw[split + 4..].to_vec();

    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .expect("parse status code");

    (status, head.to_lowercase(), body)
}

/// GET with the correct (loopback) Host header and no cookie.
fn http_get(addr: &str, path: &str) -> (u16, String, Vec<u8>) {
    request(addr, path, addr, None)
}

#[test]
fn serves_embedded_hello_page() {
    let server = launch();
    let (status, headers, body) = http_get(&server.addr, &format!("/?token={}", server.token));

    assert_eq!(status, 200, "GET /?token=<tok> should serve index.html");
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("uncompose-compare"),
        "served page should contain the app marker, got: {body}"
    );
    assert!(
        headers.contains("cache-control: no-store"),
        "responses must be uncached, headers were:\n{headers}"
    );
    assert!(
        headers.contains("content-type: text/html"),
        "index should be served as HTML, headers were:\n{headers}"
    );
    // The page seeds a token cookie so the browser can authenticate the
    // sub-resource requests it makes without a query string of its own.
    assert!(
        headers.contains(&format!("set-cookie: token={}", server.token)),
        "authorized page should seed the token cookie, headers were:\n{headers}"
    );
}

#[test]
fn serves_embedded_js_bundle() {
    let server = launch();
    // The bundle is content-hashed; discover its path from the served index.
    let (_, _, index) = http_get(&server.addr, &format!("/?token={}", server.token));
    let index = String::from_utf8_lossy(&index);
    let asset = index
        .split(['"', '\''])
        .find(|s| s.contains("assets/") && s.ends_with(".js"))
        .expect("index references a JS asset")
        .trim_start_matches('.');

    let (status, headers, body) =
        http_get(&server.addr, &format!("{asset}?token={}", server.token));
    assert_eq!(status, 200, "embedded JS asset {asset} should be served");
    assert!(!body.is_empty(), "JS asset should not be empty");
    assert!(
        headers.contains("javascript"),
        "JS asset should carry a javascript content-type, headers were:\n{headers}"
    );
    assert!(
        headers.contains("cache-control: no-store"),
        "assets must be uncached too, headers were:\n{headers}"
    );
}

#[test]
fn cookie_authorizes_sub_resource() {
    let server = launch();
    // A browser loads the page with `?token=`, gets the cookie, then requests
    // its assets carrying only that cookie (query strings don't propagate to
    // sub-resources). Prove that cookie-only requests are authorized.
    let (_, _, index) = http_get(&server.addr, &format!("/?token={}", server.token));
    let index = String::from_utf8_lossy(&index);
    let asset = index
        .split(['"', '\''])
        .find(|s| s.contains("assets/") && s.ends_with(".js"))
        .expect("index references a JS asset")
        .trim_start_matches('.');

    let cookie = format!("token={}", server.token);
    let (status, _, body) = request(&server.addr, asset, &server.addr, Some(&cookie));
    assert_eq!(
        status, 200,
        "cookie-authorized asset {asset} should be served"
    );
    assert!(!body.is_empty(), "JS asset should not be empty");
}

#[test]
fn missing_token_is_refused() {
    let server = launch();
    let (status, headers, _) = http_get(&server.addr, "/");
    assert_eq!(status, 403, "a request without the token must be refused");
    assert!(
        headers.contains("cache-control: no-store"),
        "even refusals must be uncached, headers were:\n{headers}"
    );
}

#[test]
fn wrong_token_is_refused() {
    let server = launch();
    let (status, _, _) = http_get(&server.addr, "/?token=deadbeefdeadbeefdeadbeefdeadbeef");
    assert_eq!(
        status, 403,
        "a request with the wrong token must be refused"
    );
}

#[test]
fn bad_host_is_refused() {
    let server = launch();
    // Valid token, but a non-loopback Host header (DNS-rebinding guard).
    let (status, headers, _) = request(
        &server.addr,
        &format!("/?token={}", server.token),
        "evil.example.com",
        None,
    );
    assert_eq!(status, 403, "a non-127.0.0.1 Host must be refused");
    assert!(
        headers.contains("cache-control: no-store"),
        "even refusals must be uncached, headers were:\n{headers}"
    );
}

#[test]
fn unknown_path_is_404() {
    let server = launch();
    let (status, headers, _) = http_get(
        &server.addr,
        &format!("/does-not-exist?token={}", server.token),
    );
    assert_eq!(status, 404);
    assert!(
        headers.contains("cache-control: no-store"),
        "not-found responses must be uncached too, headers were:\n{headers}"
    );
}

#[test]
fn version_flag_exits_zero() {
    let out = Command::new(BIN).arg("--version").output().unwrap();
    assert!(out.status.success(), "--version should exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "--version should print the crate version, got: {text}"
    );
}

#[test]
fn help_flag_exits_zero() {
    let out = Command::new(BIN).arg("--help").output().unwrap();
    assert!(out.status.success(), "--help should exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("uncompose-compare"),
        "--help should name the command, got: {text}"
    );
}
