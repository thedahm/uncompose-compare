//! Integration tests at the CLI process boundary — the one seam this spike
//! verifies. Every test runs the compiled binary the way a user's machine
//! would and asserts only externally observable behavior (exit codes, stdout,
//! HTTP responses and headers).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_uncompose-compare");

/// A launched server plus the loopback address it printed. Killed on drop so a
/// failing assertion never leaks a process.
struct Serving {
    child: Child,
    addr: String,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Launch the binary and read the `http://127.0.0.1:<port>/` line it prints.
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
    let addr = url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();

    Serving { child, addr }
}

/// Minimal HTTP/1.1 GET over a fresh connection. Returns (status, headers, body).
fn http_get(addr: &str, path: &str) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
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

#[test]
fn serves_embedded_hello_page() {
    let server = launch();
    let (status, headers, body) = http_get(&server.addr, "/");

    assert_eq!(status, 200, "GET / should serve the embedded index.html");
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
}

#[test]
fn serves_embedded_js_bundle() {
    let server = launch();
    // The bundle is content-hashed; discover its path from the served index.
    let (_, _, index) = http_get(&server.addr, "/");
    let index = String::from_utf8_lossy(&index);
    let asset = index
        .split(['"', '\''])
        .find(|s| s.contains("assets/") && s.ends_with(".js"))
        .expect("index references a JS asset")
        .trim_start_matches('.');

    let (status, headers, body) = http_get(&server.addr, asset);
    assert_eq!(status, 200, "embedded JS asset {asset} should be served");
    assert!(!body.is_empty(), "JS asset should not be empty");
    assert!(
        headers.contains("javascript"),
        "JS asset should carry a javascript content-type, headers were:\n{headers}"
    );
}

#[test]
fn unknown_path_is_404() {
    let server = launch();
    let (status, _, _) = http_get(&server.addr, "/does-not-exist");
    assert_eq!(status, 404);
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
