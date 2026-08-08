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
//! Issue #11 adds the playback path (#74): the `/audio/<sha256>` endpoint serves
//! a lossless proxy (FLAC, or WAV when FLAC cannot represent the source) that
//! the binary transcoded into the XDG content-hash cache, guarded like every
//! other response. These tests read the proxy bytes back and check container,
//! bit depth, sample rate, and sample count at the boundary; that a second run
//! reuses the cache without re-transcoding; that the LRU prune sheds the
//! least-recently-accessed proxy at startup; and that `cache clear` empties the
//! cache and reports what it did.
//!
//! Fixtures are deterministic seeded noise written as WAV at test time (per #73
//! — never committed audio), at whatever bit depth / channel count a case needs.

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
    launch_opts(dir, a, b, None, &[])
}

/// Launch with full control over the cache location (`XDG_CACHE_HOME`) and extra
/// CLI flags, so cache-behavior tests run against an isolated, stable cache.
fn launch_opts(
    dir: TempDir,
    a: &Path,
    b: &Path,
    cache_home: Option<&Path>,
    extra: &[&str],
) -> Serving {
    let mut command = Command::new(BIN);
    command.arg(a).arg(b).args(extra);
    if let Some(home) = cache_home {
        command.env("XDG_CACHE_HOME", home);
    }
    serving_from(command, dir)
}

/// Launch the binary with a caller-built command (so record tests can set the
/// invoking directory and `--out`), parsing the tokened URL line it prints.
fn serving_from(mut command: Command, dir: TempDir) -> Serving {
    let mut child = command
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
    let mut body = raw[split + 4..].to_vec();

    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .expect("parse status code");

    let headers = head.to_lowercase();
    // tiny_http streams larger responses (e.g. audio proxies) with chunked
    // transfer-encoding; de-chunk so callers see the raw payload.
    if headers.contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }

    (status, headers, body)
}

/// Decode an HTTP/1.1 chunked body: repeated `<hex-len>\r\n<data>\r\n`, ending
/// with a zero-length chunk.
fn dechunk(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let nl = match raw[i..].windows(2).position(|w| w == b"\r\n") {
            Some(p) => i + p,
            None => break,
        };
        let size_line = std::str::from_utf8(&raw[i..nl]).unwrap_or("").trim();
        let size = usize::from_str_radix(size_line, 16).unwrap_or(0);
        i = nl + 2;
        if size == 0 {
            break;
        }
        out.extend_from_slice(&raw[i..i + size]);
        i += size + 2; // skip the chunk's trailing CRLF
    }
    out
}

/// GET with the correct (loopback) Host header and no cookie.
fn http_get(addr: &str, path: &str) -> (u16, String, Vec<u8>) {
    request(addr, path, addr, None)
}

/// Minimal HTTP/1.1 POST of a JSON body over a fresh connection, with full
/// control over the Host header. Returns (status, lowercased-headers, body).
fn post(addr: &str, path: &str, host: &str, body: &str) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).expect("write request");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response has header/body separator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut body = raw[split + 4..].to_vec();

    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .expect("parse status code");

    let headers = head.to_lowercase();
    if headers.contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }
    (status, headers, body)
}

/// POST to the record endpoint over the tokened loopback Host.
fn http_post(server: &Serving, path: &str, body: &str) -> (u16, String, Vec<u8>) {
    post(&server.addr, path, &server.addr, body)
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

// --- Issue #11: proxy pipeline, content-hash cache, audio endpoints ---------

/// Write a WAV of arbitrary bit depth and integer/float sample format, mono or
/// stereo, filled with deterministic seeded noise. A simple `fmt ` chunk (tag 1
/// integer, tag 3 IEEE float) — enough for symphonia to decode as a source.
fn write_wav_fmt(
    path: &Path,
    frames: u32,
    sample_rate: u32,
    channels: u16,
    bits: u16,
    float: bool,
    seed: u32,
) {
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    let data_len = frames * block_align as u32;

    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    let mut state = seed.wrapping_add(1);
    let mut next = || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        state
    };
    for _ in 0..frames {
        for _ in 0..channels {
            let s = next();
            match (float, bits) {
                (true, 32) => {
                    let f = s as f32 / u32::MAX as f32 - 0.5;
                    buf.extend_from_slice(&f.to_le_bytes());
                }
                (false, 16) => buf.extend_from_slice(&((s >> 16) as i16).to_le_bytes()),
                (false, 24) => buf.extend_from_slice(&s.to_le_bytes()[0..3]),
                (false, 32) => buf.extend_from_slice(&(s as i32).to_le_bytes()),
                other => panic!("unsupported fixture format {other:?}"),
            }
        }
    }

    std::fs::write(path, &buf).expect("write wav fixture");
}

/// Write a WAVE_FORMAT_EXTENSIBLE 16-bit PCM WAV with `channels` channels — used
/// to make a source FLAC cannot represent (> 8 channels), forcing the WAV
/// fallback proxy.
fn write_wav_extensible(path: &Path, frames: u32, sample_rate: u32, channels: u16, seed: u32) {
    let bits = 16u16;
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    let data_len = frames * block_align as u32;

    let mut buf = Vec::new();
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + 24 + data_len).to_le_bytes()); // fmt is 40 bytes (16+24)
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&40u32.to_le_bytes());
    buf.extend_from_slice(&0xFFFEu16.to_le_bytes()); // WAVE_FORMAT_EXTENSIBLE
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    buf.extend_from_slice(&22u16.to_le_bytes()); // cbSize
    buf.extend_from_slice(&bits.to_le_bytes()); // valid bits per sample
    buf.extend_from_slice(&((1u32 << channels) - 1).to_le_bytes()); // channel mask
                                                                    // PCM subformat GUID.
    buf.extend_from_slice(&[
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B,
        0x71,
    ]);
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    let mut state = seed.wrapping_add(1);
    for _ in 0..frames {
        for _ in 0..channels {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            buf.extend_from_slice(&((state >> 16) as i16).to_le_bytes());
        }
    }
    std::fs::write(path, &buf).expect("write extensible wav fixture");
}

/// The first candidate's sha256 as it appears in the `/session` JSON — also the
/// key its audio proxy resolves under (`/audio/<sha256>`).
fn first_sha256(json: &str) -> String {
    let chunk = json
        .split("\"sha256\":\"")
        .nth(1)
        .expect("session json has a sha256");
    chunk[..64].to_string()
}

/// FLAC STREAMINFO, the fields the proxy contract cares about: sample rate,
/// channels, bits per sample, total sample count. Parsed straight from the bytes
/// so the test needs no FLAC decoder.
struct FlacInfo {
    sample_rate: u32,
    channels: u32,
    bits: u32,
    total_samples: u64,
}

fn parse_flac(bytes: &[u8]) -> FlacInfo {
    assert_eq!(&bytes[0..4], b"fLaC", "proxy is a FLAC stream");
    // 4-byte "fLaC" + 4-byte metadata-block header, then STREAMINFO content.
    // The packed 64-bit field sits 10 bytes into STREAMINFO (after the block
    // sizes and frame sizes): rate(20) | channels(3) | bps(5) | samples(36).
    let si = 8 + 10;
    let packed = u64::from_be_bytes(bytes[si..si + 8].try_into().unwrap());
    FlacInfo {
        sample_rate: ((packed >> 44) & 0xF_FFFF) as u32,
        channels: (((packed >> 41) & 0x7) + 1) as u32,
        bits: (((packed >> 36) & 0x1F) + 1) as u32,
        total_samples: packed & 0xF_FFFF_FFFF,
    }
}

/// The bit depth (and container = WAV) of a `fmt `-chunk WAV proxy, read from the
/// header without decoding.
fn parse_wav_bits(bytes: &[u8]) -> u16 {
    assert_eq!(&bytes[0..4], b"RIFF", "proxy is a RIFF/WAV stream");
    assert_eq!(&bytes[8..12], b"WAVE", "proxy is a WAVE stream");
    // In both plain and EXTENSIBLE `fmt ` layouts, bits-per-sample is at
    // offset 34 (12 RIFF/WAVE + 8 chunk header + 14 into the fmt body).
    u16::from_le_bytes(bytes[34..36].try_into().unwrap())
}

/// Fetch the proxy bytes for a hash over the tokened loopback, asserting the
/// shared response guarantees.
fn audio_bytes(server: &Serving, hash: &str) -> (u16, String, Vec<u8>) {
    http_get(
        &server.addr,
        &format!("/audio/{hash}?token={}", server.token),
    )
}

#[test]
fn audio_endpoint_serves_decodable_flac_proxy() {
    let server = launch(); // two 1-second 44.1k stereo 16-bit fixtures
    let json = session_json(&server);
    let hash = first_sha256(&json);
    // The session advertises the proxy under /audio/<sha256>.
    assert!(
        json.contains(&format!("\"audio\":\"/audio/{hash}\"")),
        "session should point at the audio endpoint: {json}"
    );

    let (status, headers, body) = audio_bytes(&server, &hash);
    assert_eq!(status, 200, "the audio proxy should be served to a token");
    assert!(
        headers.contains("cache-control: no-store"),
        "the proxy must be uncached like every response: {headers}"
    );
    assert!(
        headers.contains("content-type: audio/flac"),
        "a FLAC proxy carries an audio/flac type: {headers}"
    );

    // Verify container, bit depth, sample rate, and sample count at the boundary.
    let info = parse_flac(&body);
    assert_eq!(info.sample_rate, 44_100, "sample rate is never resampled");
    assert_eq!(info.channels, 2, "channel count is preserved");
    assert_eq!(info.bits, 16, "a 16-bit source stays 16-bit (bit-exact)");
    assert_eq!(
        info.total_samples, 44_100,
        "the proxy holds every source frame"
    );
}

#[test]
fn audio_endpoint_bit_depth_policy() {
    // One case per source depth: 16/24-bit integer pass through; 32-bit int and
    // 32-bit float quantize to 24. Sample rate is untouched throughout.
    for (tag, bits, float, expect) in [
        ("i16", 16u16, false, 16u32),
        ("i24", 24, false, 24),
        ("i32", 32, false, 24),
        ("f32", 32, true, 24),
    ] {
        let dir = TempDir::new(tag);
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_wav_fmt(&a, 44_100, 48_000, 2, bits, float, 11);
        write_wav_fmt(&b, 44_100, 48_000, 2, bits, float, 12);
        let server = launch_with(dir, &a, &b);

        let hash = first_sha256(&session_json(&server));
        let (status, headers, body) = audio_bytes(&server, &hash);
        assert_eq!(status, 200, "[{tag}] proxy served");
        assert!(
            headers.contains("content-type: audio/flac"),
            "[{tag}] integer/float sources within FLAC's reach stay FLAC: {headers}"
        );
        let info = parse_flac(&body);
        assert_eq!(info.bits, expect, "[{tag}] proxy bit depth");
        assert_eq!(info.sample_rate, 48_000, "[{tag}] sample rate preserved");
        assert_eq!(info.total_samples, 44_100, "[{tag}] sample count preserved");
    }
}

#[test]
fn audio_endpoint_wav_fallback_for_unrepresentable_source() {
    // FLAC tops out at 8 channels; a 10-channel source falls back to WAV.
    let dir = TempDir::new("wavfallback");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav_extensible(&a, 22_050, 44_100, 10, 5);
    write_wav_extensible(&b, 22_050, 44_100, 10, 6);
    let server = launch_with(dir, &a, &b);

    let hash = first_sha256(&session_json(&server));
    let (status, headers, body) = audio_bytes(&server, &hash);
    assert_eq!(status, 200, "the WAV-fallback proxy should be served");
    assert!(
        headers.contains("content-type: audio/wav"),
        "a 10-channel source falls back to a WAV proxy: {headers}"
    );
    assert_eq!(parse_wav_bits(&body), 16, "a 16-bit source stays 16-bit");
}

#[test]
fn audio_endpoint_refuses_missing_token_and_bad_host() {
    let server = launch();
    let hash = first_sha256(&session_json(&server));

    let (status, headers, _) = http_get(&server.addr, &format!("/audio/{hash}"));
    assert_eq!(status, 403, "the audio endpoint needs the token too");
    assert!(
        headers.contains("cache-control: no-store"),
        "even the audio refusal must be uncached: {headers}"
    );

    let (status, _, _) = request(
        &server.addr,
        &format!("/audio/{hash}?token={}", server.token),
        "evil.example.com",
        None,
    );
    assert_eq!(status, 403, "the audio endpoint honors the Host check too");
}

#[test]
fn audio_endpoint_unknown_hash_is_404() {
    let server = launch();
    // A well-formed but unknown hash resolves to nothing — no path ever reaches
    // the filesystem, so this is a plain 404, not a traversal foothold.
    let (status, _, _) = audio_bytes(&server, &"a".repeat(64));
    assert_eq!(status, 404, "an unknown content hash is not found");

    // A path-traversal attempt is likewise just an unknown key.
    let (status, _, _) = http_get(
        &server.addr,
        &format!("/audio/../../etc/passwd?token={}", server.token),
    );
    assert_eq!(status, 404, "the audio endpoint never resolves a path");
}

#[test]
fn second_run_reuses_cache_without_retranscoding() {
    let cache_home = TempDir::new("cache-reuse");
    let fixtures = TempDir::new("reuse-fixtures");
    let a = fixtures.join("a.wav");
    let b = fixtures.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 21);
    write_wav(&b, 44_100, 44_100, 2, 22);

    // First run populates the cache.
    let hash_a = {
        let server = launch_opts(TempDir::new("reuse-1"), &a, &b, Some(&cache_home.path), &[]);
        first_sha256(&session_json(&server))
    };

    let proxy = cache_home
        .path
        .join("uncompose-compare")
        .join(format!("{hash_a}.flac"));
    assert!(
        proxy.exists(),
        "first run should write the proxy: {proxy:?}"
    );
    let mtime_first = std::fs::metadata(&proxy).unwrap().modified().unwrap();

    // Second run against the same files must not rewrite the proxy.
    {
        let server = launch_opts(TempDir::new("reuse-2"), &a, &b, Some(&cache_home.path), &[]);
        let _ = session_json(&server);
    }
    let mtime_second = std::fs::metadata(&proxy).unwrap().modified().unwrap();
    assert_eq!(
        mtime_first, mtime_second,
        "a second run against unchanged files must not re-transcode"
    );
}

#[test]
fn startup_prune_evicts_least_recently_used() {
    let cache_home = TempDir::new("cache-prune");
    let cache_dir = cache_home.path.join("uncompose-compare");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Plant a large, old proxy that a tight cap must evict.
    let stale = cache_dir.join("staaaaale.flac");
    std::fs::write(&stale, vec![0u8; 5_000_000]).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_accessed(old))
        .unwrap();

    let fixtures = TempDir::new("prune-fixtures");
    let a = fixtures.join("a.wav");
    let b = fixtures.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 31);
    write_wav(&b, 44_100, 44_100, 2, 32);

    // A 1 MB cap: the two fresh proxies fit, the 5 MB stale file cannot.
    let server = launch_opts(
        TempDir::new("prune-run"),
        &a,
        &b,
        Some(&cache_home.path),
        &["--cache-max-bytes", "1000000"],
    );
    let hash_a = first_sha256(&session_json(&server));
    drop(server); // ensure prune (at startup) has already run

    assert!(
        !stale.exists(),
        "the least-recently-accessed proxy should be evicted"
    );
    let fresh = cache_dir.join(format!("{hash_a}.flac"));
    assert!(
        fresh.exists(),
        "this session's freshly-used proxy must survive the prune"
    );
}

#[test]
fn cache_clear_empties_and_reports() {
    let cache_home = TempDir::new("cache-clear");
    let fixtures = TempDir::new("clear-fixtures");
    let a = fixtures.join("a.wav");
    let b = fixtures.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 41);
    write_wav(&b, 44_100, 44_100, 2, 42);

    // Populate the cache.
    {
        let server = launch_opts(
            TempDir::new("clear-run"),
            &a,
            &b,
            Some(&cache_home.path),
            &[],
        );
        let _ = session_json(&server);
    }
    let cache_dir = cache_home.path.join("uncompose-compare");
    let before = std::fs::read_dir(&cache_dir).unwrap().count();
    assert!(before >= 2, "two candidates should populate the cache");

    // `cache clear` wipes it and reports what it did.
    let out = Command::new(BIN)
        .args(["cache", "clear"])
        .env("XDG_CACHE_HOME", &cache_home.path)
        .output()
        .expect("run cache clear");
    assert!(out.status.success(), "cache clear should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("cleared") && stdout.contains("proxy"),
        "cache clear should report what it removed: {stdout}"
    );

    let after = std::fs::read_dir(&cache_dir).unwrap().count();
    assert_eq!(after, 0, "the cache should be empty after clear");
}

// --- Issue #16: the comparison record — conclude, validate, write once --------

/// The schema's `$id`, which every written record carries in its `schema` field.
const RECORD_SCHEMA_ID: &str =
    "https://uncompose.org/schemas/compare/v0/uncompose.compare.schema.json";

/// Launch the binary for a record test: two matched fixtures, a chosen invoking
/// directory (the default record destination), and an optional `--out`.
fn launch_recording(cwd: &Path, out: Option<&Path>) -> Serving {
    let dir = TempDir::new("record-fixtures");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 71);
    write_wav(&b, 44_100, 44_100, 2, 72);
    let mut command = Command::new(BIN);
    command.arg(&a).arg(&b).current_dir(cwd);
    if let Some(o) = out {
        command.arg("--out").arg(o);
    }
    serving_from(command, dir)
}

/// Read a written record, assert it validates against the in-repo v0 schema (the
/// repo-owned artifact used server-side), and return it for field checks.
fn validate_record_file(path: &Path) -> serde_json::Value {
    let bytes = std::fs::read(path).expect("read record file");
    let record: serde_json::Value = serde_json::from_slice(&bytes).expect("record is valid JSON");
    let schema_src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas/compare/v0/uncompose.compare.schema.json"),
    )
    .expect("read the committed schema");
    let schema: serde_json::Value =
        serde_json::from_str(&schema_src).expect("schema is valid JSON");
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let errors: Vec<String> = validator
        .iter_errors(&record)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "record must validate against the v0 schema: {errors:?}\n{record}"
    );
    record
}

/// The lone `*.json` record in a directory, or a panic if there isn't exactly one.
fn sole_record(dir: &Path) -> PathBuf {
    let mut records: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    assert_eq!(records.len(), 1, "expected exactly one record in {dir:?}");
    records.pop().unwrap()
}

#[test]
fn conclude_writes_valid_record_with_matching_hashes() {
    let cwd = TempDir::new("record-default");
    let server = launch_recording(&cwd.path, None);
    let session = session_json(&server);
    let sha_a = first_sha256(&session);

    let body = r#"{
        "result": {"preference": "A", "confidence": 4, "criterion": "clarity", "summary": "A cleaner"},
        "observations": [
            {"at": "2026-08-08T10:00:00Z", "position_ms": 1200, "loop": 0, "candidate": "A", "text": "bright"},
            {"at": "2026-08-08T10:01:00Z", "text": "difference only in the chorus"}
        ],
        "loops": [{"start_ms": 1000, "end_ms": 3000}],
        "context": "picking a master"
    }"#;
    let (status, headers, resp) =
        http_post(&server, &format!("/record?token={}", server.token), body);
    let resp = String::from_utf8_lossy(&resp);
    assert_eq!(status, 200, "a valid conclude is accepted: {resp}");
    assert!(
        headers.contains("cache-control: no-store"),
        "the record response is uncached like every other: {headers}"
    );

    // The UI is told where the record landed, and it landed there.
    let response: serde_json::Value = serde_json::from_str(&resp).expect("record response is JSON");
    let reported = response["path"].as_str().expect("response reports a path");
    let record_path = sole_record(&cwd.path);
    assert_eq!(
        reported,
        record_path.to_string_lossy(),
        "the reported path is the written file"
    );

    let record = validate_record_file(&record_path);
    // Server-authoritative fields: schema id, ULID, mode, empty playback.
    assert_eq!(record["schema"], RECORD_SCHEMA_ID);
    assert_eq!(record["mode"], "ab");
    assert_eq!(record["playback"], serde_json::json!({}));
    assert_eq!(
        record["id"].as_str().map(|s| s.len()),
        Some(26),
        "id is a 26-char ULID: {record}"
    );
    assert!(
        record["created_at"].as_str().unwrap().ends_with('Z')
            && record["completed_at"].as_str().unwrap().ends_with('Z'),
        "timestamps are RFC3339 UTC: {record}"
    );

    // The record's hashes provably match the loaded inputs (they come from the
    // trusted load, not the request body).
    let candidates = record["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0]["label"], "A");
    assert_eq!(candidates[0]["sha256"], sha_a);
    assert!(candidates[0]["path"].is_string() && candidates[0]["size"].is_u64());

    // Session-authored fields survive the round trip in the order made.
    assert_eq!(record["result"]["preference"], "A");
    assert_eq!(record["result"]["confidence"], 4);
    assert_eq!(record["observations"].as_array().unwrap().len(), 2);
    assert_eq!(record["observations"][0]["text"], "bright");
    assert_eq!(record["observations"][0]["candidate"], "A");
    assert_eq!(record["loops"][0]["start_ms"], 1000);
    assert_eq!(record["context"], "picking a master");
}

#[test]
fn conclude_records_no_preference_as_null() {
    let cwd = TempDir::new("record-nopref");
    let server = launch_recording(&cwd.path, None);

    // "No preference" is a valid outcome: a null preference and no confidence.
    let body = r#"{"result": {"preference": null}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(
        status,
        200,
        "no-preference is a valid conclude: {}",
        String::from_utf8_lossy(&resp)
    );

    let record = validate_record_file(&sole_record(&cwd.path));
    assert!(
        record["result"]["preference"].is_null(),
        "no preference: {record}"
    );
    assert!(
        record["result"].get("confidence").is_none(),
        "no confidence without a preference: {record}"
    );
}

#[test]
fn conclude_enforces_confidence_iff_preference() {
    let cwd = TempDir::new("record-conf");
    let server = launch_recording(&cwd.path, None);

    // A chosen preference with no confidence is refused by schema validation.
    let (status, _, resp) = http_post(
        &server,
        &format!("/record?token={}", server.token),
        r#"{"result": {"preference": "B"}, "observations": [], "loops": []}"#,
    );
    assert_eq!(
        status,
        400,
        "a preference needs a confidence: {}",
        String::from_utf8_lossy(&resp)
    );

    // A null preference carrying a confidence is likewise refused.
    let (status, _, _) = http_post(
        &server,
        &format!("/record?token={}", server.token),
        r#"{"result": {"preference": null, "confidence": 3}, "observations": [], "loops": []}"#,
    );
    assert_eq!(status, 400, "no preference must omit confidence");

    // Neither refusal wrote anything: the session is not concluded.
    assert!(
        std::fs::read_dir(&cwd.path).unwrap().next().is_none(),
        "a refused conclude writes no record"
    );
}

#[test]
fn second_conclude_is_refused() {
    let cwd = TempDir::new("record-twice");
    let server = launch_recording(&cwd.path, None);
    let body =
        r#"{"result": {"preference": "A", "confidence": 5}, "observations": [], "loops": []}"#;

    let (first, _, _) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(first, 200, "the first conclude writes the record");

    let (second, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(second, 409, "a second conclude is refused");
    assert!(
        String::from_utf8_lossy(&resp).contains("already"),
        "the refusal explains itself: {}",
        String::from_utf8_lossy(&resp)
    );
    // Still exactly one record: the write happened exactly once.
    let _ = sole_record(&cwd.path);
}

#[test]
fn conclude_refuses_to_overwrite_existing_destination() {
    let cwd = TempDir::new("record-overwrite-cwd");
    let out_dir = TempDir::new("record-overwrite-out");
    let out = out_dir.join("record.json");
    std::fs::write(&out, b"do not clobber me").unwrap();

    let server = launch_recording(&cwd.path, Some(&out));
    let body =
        r#"{"result": {"preference": "A", "confidence": 3}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(status, 409, "an existing --out destination is refused");
    assert!(
        String::from_utf8_lossy(&resp).contains("overwrite"),
        "the refusal names the overwrite: {}",
        String::from_utf8_lossy(&resp)
    );
    // The pre-existing file is untouched.
    assert_eq!(std::fs::read(&out).unwrap(), b"do not clobber me");
}

#[test]
fn conclude_honors_out_override() {
    let cwd = TempDir::new("record-out-cwd");
    let out_dir = TempDir::new("record-out-dest");
    let out = out_dir.join("verdict.json");

    let server = launch_recording(&cwd.path, Some(&out));
    let body =
        r#"{"result": {"preference": "B", "confidence": 2}, "observations": [], "loops": []}"#;
    let (status, _, _) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(status, 200, "--out is a valid destination");

    // The record is at --out, and not in the invoking directory.
    assert!(out.exists(), "the record is written to --out");
    let record = validate_record_file(&out);
    assert_eq!(record["result"]["preference"], "B");
    assert!(
        std::fs::read_dir(&cwd.path).unwrap().next().is_none(),
        "--out means nothing lands in the invoking directory"
    );
}

#[test]
fn abandoned_session_writes_no_record() {
    let cwd = TempDir::new("record-abandoned");
    {
        // Launch, load the session, but never conclude — then drop (kill) it.
        let server = launch_recording(&cwd.path, None);
        let _ = session_json(&server);
    }
    assert!(
        std::fs::read_dir(&cwd.path).unwrap().next().is_none(),
        "an abandoned session leaves no record file"
    );
}

#[test]
fn record_endpoint_enforces_the_privacy_contract() {
    let cwd = TempDir::new("record-guard");
    let server = launch_recording(&cwd.path, None);
    let body = r#"{"result": {"preference": null}, "observations": [], "loops": []}"#;

    // No token: refused, and uncached like every refusal.
    let (status, headers, _) = http_post(&server, "/record", body);
    assert_eq!(status, 403, "the record endpoint needs the token");
    assert!(
        headers.contains("cache-control: no-store"),
        "refusal is uncached: {headers}"
    );

    // Bad Host: refused (DNS-rebinding guard).
    let (status, _, _) = post(
        &server.addr,
        &format!("/record?token={}", server.token),
        "evil.example.com",
        body,
    );
    assert_eq!(status, 403, "the record endpoint honors the Host check");

    // Nothing was written by a refused request.
    assert!(
        std::fs::read_dir(&cwd.path).unwrap().next().is_none(),
        "a refused conclude writes no record"
    );
}
