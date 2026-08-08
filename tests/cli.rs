//! Integration tests at the CLI process boundary — the one seam this repo
//! verifies. Every test runs the compiled binary the way a user's machine
//! would and asserts only externally observable behavior (exit codes, stdout,
//! stderr, HTTP responses and headers).
//!
//! Issue #10 makes the binary the real two-file command: `uncompose-compare
//! <a> <b>` loads two audio files as candidates A and B, hashing and decoding
//! each, and exposes their metadata over a `/session` endpoint under the same
//! #72 privacy contract the spike's served page already enforces. Bad
//! invocations fail with a clear message and a non-zero exit before the server
//! ever binds.
//!
//! Fixtures are deterministic seeded noise written as 16-bit PCM WAV at test
//! time (per #73 — never committed audio).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_uncompose-compare");

/// A scratch directory for a test's fixtures, removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "uncompose-compare-test-{}-{tag}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Write a 16-bit PCM WAV of `frames` sample-frames at `sample_rate`/`channels`,
/// filled with deterministic seeded noise. Returns the byte length written.
fn write_wav(path: &Path, frames: u32, sample_rate: u32, channels: u16, seed: u32) -> u64 {
    let bits = 16u16;
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    let data_len = frames * block_align as u32;

    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    // A trivial LCG keeps the noise deterministic without a dependency.
    let mut state = seed.wrapping_add(1);
    for _ in 0..frames {
        for _ in 0..channels {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let sample = (state >> 16) as i16;
            buf.extend_from_slice(&sample.to_le_bytes());
        }
    }

    std::fs::write(path, &buf).expect("write wav fixture");
    buf.len() as u64
}

/// A launched server plus the loopback address and session token it printed.
/// Killed on drop so a failing assertion never leaks a process. Holds the
/// fixture dir so the files outlive the server.
struct Serving {
    child: Child,
    addr: String,
    token: String,
    _fixtures: TempDir,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Launch the binary against two matched fixtures (no duration mismatch).
fn launch() -> Serving {
    let dir = TempDir::new("serve");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 1);
    write_wav(&b, 44_100, 44_100, 2, 2);
    launch_with(dir, &a, &b)
}

/// Launch the binary against two specific files, parsing the tokened URL line.
fn launch_with(dir: TempDir, a: &Path, b: &Path) -> Serving {
    let mut child = Command::new(BIN)
        .arg(a)
        .arg(b)
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

    Serving {
        child,
        addr,
        token,
        _fixtures: dir,
    }
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

// --- Issue #10: the two-file load pipeline and /session endpoint ------------

/// Fetch the `/session` JSON body over the loopback token, asserting the
/// no-store and content-type guarantees along the way.
fn session_json(server: &Serving) -> String {
    let (status, headers, body) =
        http_get(&server.addr, &format!("/session?token={}", server.token));
    assert_eq!(
        status, 200,
        "/session should be served to a tokened request"
    );
    assert!(
        headers.contains("cache-control: no-store"),
        "the session endpoint must be uncached like every response, headers:\n{headers}"
    );
    assert!(
        headers.contains("content-type: application/json"),
        "the session endpoint should be JSON, headers:\n{headers}"
    );
    String::from_utf8(body).expect("session body is utf-8")
}

#[test]
fn session_reports_both_candidates_with_metadata() {
    let dir = TempDir::new("meta");
    let a = dir.join("alpha.wav");
    let b = dir.join("bravo.wav");
    // A: 1 second stereo at 44.1k. B: 0.5 second stereo at 44.1k.
    let a_size = write_wav(&a, 44_100, 44_100, 2, 7);
    write_wav(&b, 22_050, 44_100, 2, 9);
    let server = launch_with(dir, &a, &b);

    let json = session_json(&server);

    // Candidate A: label, name, decoded duration and rate, and the size at load.
    assert!(json.contains("\"label\":\"A\""), "A labeled A: {json}");
    assert!(json.contains("\"name\":\"alpha.wav\""), "A named: {json}");
    assert!(json.contains("\"frames\":44100"), "A frame count: {json}");
    assert!(json.contains("\"sample_rate\":44100"), "A rate: {json}");
    assert!(json.contains("\"channels\":2"), "A channels: {json}");
    assert!(json.contains("\"duration_ms\":1000"), "A ms: {json}");
    assert!(
        json.contains(&format!("\"size\":{a_size}")),
        "A size at load ({a_size}): {json}"
    );

    // Candidate B in argument order.
    assert!(json.contains("\"label\":\"B\""), "B labeled B: {json}");
    assert!(json.contains("\"name\":\"bravo.wav\""), "B named: {json}");
    assert!(json.contains("\"frames\":22050"), "B frame count: {json}");
    assert!(json.contains("\"duration_ms\":500"), "B ms: {json}");

    // Both inputs are hashed (sha256 = 64 hex chars each).
    let sha_count = json.matches("\"sha256\":\"").count();
    assert_eq!(sha_count, 2, "both candidates carry a sha256: {json}");
    for chunk in json.split("\"sha256\":\"").skip(1) {
        let hash = &chunk[..64.min(chunk.len())];
        assert_eq!(hash.len(), 64, "sha256 is 64 hex chars: {hash}");
        assert!(
            hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "sha256 is hex: {hash}"
        );
    }
}

#[test]
fn session_flags_duration_mismatch() {
    let dir = TempDir::new("mismatch");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 44_100, 44_100, 1, 3); // 1000 ms
    write_wav(&b, 33_075, 44_100, 1, 4); // 750 ms
    let server = launch_with(dir, &a, &b);

    let json = session_json(&server);
    assert!(
        json.contains("\"duration_mismatch\":true"),
        "differing durations must flag a mismatch: {json}"
    );
    assert!(
        json.contains("\"duration_delta_samples\":11025"),
        "mismatch is stated in samples: {json}"
    );
    assert!(
        json.contains("\"duration_delta_ms\":250"),
        "mismatch is stated in ms: {json}"
    );
}

#[test]
fn session_no_mismatch_when_durations_match() {
    let server = launch(); // two 1-second fixtures
    let json = session_json(&server);
    assert!(
        json.contains("\"duration_mismatch\":false"),
        "matched durations must not flag a mismatch: {json}"
    );
}

#[test]
fn session_endpoint_refuses_missing_token() {
    let server = launch();
    let (status, headers, _) = http_get(&server.addr, "/session");
    assert_eq!(status, 403, "the session endpoint needs the token too");
    assert!(
        headers.contains("cache-control: no-store"),
        "even the session refusal must be uncached, headers:\n{headers}"
    );
}

#[test]
fn session_endpoint_refuses_bad_host() {
    let server = launch();
    let (status, _, _) = request(
        &server.addr,
        &format!("/session?token={}", server.token),
        "evil.example.com",
        None,
    );
    assert_eq!(
        status, 403,
        "the session endpoint honors the Host check too"
    );
}

/// The distinct exit + message a bad invocation must produce. Returns
/// (exit code, stderr).
fn run_expecting_failure(args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(BIN).args(args).output().expect("spawn binary");
    assert!(
        !out.status.success(),
        "expected a non-zero exit for args {args:?}"
    );
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn zero_arguments_is_a_clear_error() {
    let (code, stderr) = run_expecting_failure(&[]);
    assert_ne!(code, Some(0));
    let lc = stderr.to_lowercase();
    assert!(
        lc.contains("required") || lc.contains("usage"),
        "zero args should name the missing operands: {stderr}"
    );
}

#[test]
fn one_argument_is_a_clear_error() {
    let dir = TempDir::new("one-arg");
    let a = dir.join("a.wav");
    write_wav(&a, 1000, 44_100, 1, 1);
    let (code, stderr) = run_expecting_failure(&[a.to_str().unwrap()]);
    assert_ne!(code, Some(0));
    let lc = stderr.to_lowercase();
    assert!(
        lc.contains("required") || lc.contains("<b>"),
        "one arg should report the second candidate missing: {stderr}"
    );
}

#[test]
fn three_arguments_is_a_clear_error() {
    let dir = TempDir::new("three-arg");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    let c = dir.join("c.wav");
    write_wav(&a, 1000, 44_100, 1, 1);
    write_wav(&b, 1000, 44_100, 1, 2);
    write_wav(&c, 1000, 44_100, 1, 3);
    let (code, stderr) = run_expecting_failure(&[
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        c.to_str().unwrap(),
    ]);
    assert_ne!(code, Some(0));
    assert!(
        stderr.to_lowercase().contains("unexpected"),
        "a third positional should be rejected as unexpected: {stderr}"
    );
}

#[test]
fn missing_file_is_a_clear_error() {
    let dir = TempDir::new("missing");
    let a = dir.join("a.wav");
    write_wav(&a, 1000, 44_100, 1, 1);
    let missing = dir.join("nope.wav");
    let (code, stderr) = run_expecting_failure(&[a.to_str().unwrap(), missing.to_str().unwrap()]);
    assert_eq!(code, Some(1), "a load failure exits 1");
    assert!(
        stderr.contains("cannot read") && stderr.contains("nope.wav"),
        "a missing file should be reported by name: {stderr}"
    );
}

#[test]
fn undecodable_file_is_a_clear_error() {
    let dir = TempDir::new("undecodable");
    let a = dir.join("a.wav");
    write_wav(&a, 1000, 44_100, 1, 1);
    let bad = dir.join("bad.wav");
    std::fs::write(&bad, b"this is definitely not an audio file").unwrap();
    let (code, stderr) = run_expecting_failure(&[a.to_str().unwrap(), bad.to_str().unwrap()]);
    assert_eq!(code, Some(1), "a load failure exits 1");
    assert!(
        stderr.contains("cannot decode") && stderr.contains("bad.wav"),
        "an undecodable file should be reported by name: {stderr}"
    );
}
