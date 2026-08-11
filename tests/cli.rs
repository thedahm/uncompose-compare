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
//! Issue #16 adds the comparison record: `/record` assembles, validates against
//! the in-repo v0 schema, and writes it exactly once — refusing a second
//! conclude, an existing destination, and any body outside the v0 contract
//! (more than one loop region, unknown plain fields, a candidate reference the
//! session never loaded).
//!
//! Fixtures are deterministic seeded noise written as WAV at test time (per #73
//! — never committed audio), at whatever bit depth / channel count a case needs.
//! Every launch runs against a throwaway `XDG_CACHE_HOME`, so the suite never
//! writes proxies into — or prunes — the developer's real cache.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use sha2::Digest;

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
/// filled with deterministic seeded noise — the default fixture shape. Returns
/// the byte length written.
fn write_wav(path: &Path, frames: u32, sample_rate: u32, channels: u16, seed: u32) -> u64 {
    write_wav_fmt(path, frames, sample_rate, channels, 16, false, seed, 1.0);
    std::fs::metadata(path).expect("wav fixture written").len()
}

/// The RIFF/WAVE framing every fixture shares: `RIFF` + size + `WAVE`, the
/// caller's `fmt ` chunk body (plain or EXTENSIBLE), then the `data` header.
/// Sample bytes are appended by the caller.
fn riff_wave(fmt_body: &[u8], data_len: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(20 + fmt_body.len() + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    // Everything after this field: "WAVE" + the fmt chunk + the data chunk.
    buf.extend_from_slice(&(4 + 8 + fmt_body.len() as u32 + 8 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&(fmt_body.len() as u32).to_le_bytes());
    buf.extend_from_slice(fmt_body);
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    buf
}

/// A trivial LCG: deterministic fixture noise without a dependency.
fn noise(seed: u32) -> impl FnMut() -> u32 {
    let mut state = seed.wrapping_add(1);
    move || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        state
    }
}

/// A launched server plus the loopback address and session token it printed.
/// Killed on drop so a failing assertion never leaks a process. Holds the
/// fixture dir (so the files outlive the server) and, unless the test pinned its
/// own, the throwaway proxy cache this run used.
struct Serving {
    child: Child,
    addr: String,
    token: String,
    _fixtures: TempDir,
    _cache: Option<TempDir>,
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
    serving_with_cache(command, dir, cache_home)
}

/// Launch the binary with a caller-built command (so record tests can set the
/// invoking directory and `--out`), parsing the tokened URL line it prints.
fn serving_from(command: Command, dir: TempDir) -> Serving {
    serving_with_cache(command, dir, None)
}

/// As `serving_from`, isolating the proxy cache: a test that pins its own
/// `XDG_CACHE_HOME` (the cache-behavior tests, which need two runs to share one
/// cache) gets that; every other launch gets a throwaway cache dropped with the
/// server. No test ever reads or prunes the developer's real `~/.cache`.
fn serving_with_cache(mut command: Command, dir: TempDir, cache_home: Option<&Path>) -> Serving {
    let owned_cache = match cache_home {
        Some(home) => {
            command.env("XDG_CACHE_HOME", home);
            None
        }
        None => {
            let cache = TempDir::new("cache");
            command.env("XDG_CACHE_HOME", &cache.path);
            Some(cache)
        }
    };

    // stderr is left at its default (inherited from the test process) unless the
    // caller piped it — the import-failure test captures the relayed stderr.
    let mut child = command
        .stdout(Stdio::piped())
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
        _cache: owned_cache,
    }
}

/// Minimal HTTP/1.1 GET over a fresh connection with full control over the Host
/// header and an optional Cookie. Returns (status, lowercased-headers, body).
fn request(addr: &str, path: &str, host: &str, cookie: Option<&str>) -> (u16, String, Vec<u8>) {
    let cookie_line = match cookie {
        Some(c) => format!("Cookie: {c}\r\n"),
        None => String::new(),
    };
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{cookie_line}Connection: close\r\n\r\n");
    roundtrip(addr, &req)
}

/// Send a raw HTTP/1.1 request over a fresh connection and parse the response.
/// Returns (status, lowercased-headers, body).
fn roundtrip(addr: &str, req: &str) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
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
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    roundtrip(addr, &req)
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
    let hash = first_sha256(&session_json(&server));
    let wrong = "deadbeefdeadbeefdeadbeefdeadbeef";

    // Every endpoint, not just the page: presenting a wrong token is refused
    // exactly like presenting none.
    for path in ["/", "/session", &format!("/audio/{hash}")] {
        let (status, _, _) = http_get(&server.addr, &format!("{path}?token={wrong}"));
        assert_eq!(status, 403, "a wrong token must be refused at {path}");
    }
    let (status, _, _) = http_post(
        &server,
        &format!("/record?token={wrong}"),
        r#"{"result": {"preference": null}, "observations": [], "loops": []}"#,
    );
    assert_eq!(status, 403, "a wrong token must be refused at /record");
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

/// A port the OS just handed out and immediately released — free to bind, with
/// the usual (accepted) race of any "find a free port" helper.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("probe a free port")
        .local_addr()
        .expect("probe address")
        .port()
}

#[test]
fn port_flag_pins_the_port() {
    let port = free_port();
    let dir = TempDir::new("port");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 4_410, 44_100, 1, 61);
    write_wav(&b, 4_410, 44_100, 1, 62);
    let server = launch_opts(dir, &a, &b, None, &["--port", &port.to_string()]);

    assert_eq!(
        server.addr,
        format!("127.0.0.1:{port}"),
        "--port must pin the port the printed URL carries"
    );
    // …and the pinned port is really the one serving.
    let _ = session_json(&server);
}

#[test]
fn unbindable_port_is_a_clear_error() {
    // Hold the port for the duration of the run so the bind cannot succeed.
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let port = held.local_addr().unwrap().port().to_string();

    let dir = TempDir::new("port-taken");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 4_410, 44_100, 1, 63);
    write_wav(&b, 4_410, 44_100, 1, 64);

    let (code, stderr) =
        run_expecting_failure(&[a.to_str().unwrap(), b.to_str().unwrap(), "--port", &port]);
    assert_eq!(code, Some(1), "an unbindable port exits 1");
    assert!(
        stderr.contains("cannot bind") && stderr.contains(&port),
        "the failure should name the port it could not bind: {stderr}"
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
fn session_flags_sample_rate_mismatch() {
    let dir = TempDir::new("rate-mismatch");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    // Same duration (1000 ms) and channel count, differing sample rate only.
    write_wav(&a, 44_100, 44_100, 2, 3);
    write_wav(&b, 48_000, 48_000, 2, 4);
    let server = launch_with(dir, &a, &b);

    let json = session_json(&server);
    assert!(
        json.contains("\"sample_rate_mismatch\":true"),
        "differing sample rates must flag a mismatch: {json}"
    );
    // Both values are observable in the payload for the warning to name.
    assert!(
        json.contains("\"sample_rate\":44100") && json.contains("\"sample_rate\":48000"),
        "both sample rates are reported: {json}"
    );
    // A sample-rate difference is not a channel difference.
    assert!(
        json.contains("\"channel_count_mismatch\":false"),
        "matched channel counts must not flag a channel mismatch: {json}"
    );
    // Nor is it a duration difference: these two fixtures are both exactly one
    // second long, they just count samples at different rates. The listener is
    // told the one thing that is true — the rates differ — and not warned a
    // second time about a "difference" of 3900 samples that is no difference at
    // all. The frame delta itself is omitted: across rates it counts
    // incomparable units.
    let session: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert_eq!(
        session["duration_mismatch"], false,
        "equal durations at different rates are not a duration mismatch: {json}"
    );
    assert!(
        session.get("duration_delta_samples").is_none(),
        "a cross-rate frame delta is meaningless and must not be reported: {json}"
    );
}

#[test]
fn session_flags_duration_mismatch_across_sample_rates_by_elapsed_time() {
    // The other half of the rule above: when the two lanes really are different
    // lengths, a rate difference must not hide it. 44 100 frames at 44.1 kHz is
    // 1000 ms; 24 000 frames at 48 kHz is 500 ms.
    let dir = TempDir::new("rate-and-duration-mismatch");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 3);
    write_wav(&b, 24_000, 48_000, 2, 4);
    let server = launch_with(dir, &a, &b);

    let json = session_json(&server);
    let session: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert_eq!(
        session["duration_mismatch"], true,
        "a real length difference is flagged whatever the rates: {json}"
    );
    assert_eq!(
        session["duration_delta_ms"].as_f64(),
        Some(500.0),
        "the elapsed-time delta is the one that means something: {json}"
    );
}

#[test]
fn session_flags_channel_count_mismatch() {
    let dir = TempDir::new("channel-mismatch");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    // Same duration and sample rate, differing channel count only.
    write_wav(&a, 44_100, 44_100, 1, 3);
    write_wav(&b, 44_100, 44_100, 2, 4);
    let server = launch_with(dir, &a, &b);

    let json = session_json(&server);
    assert!(
        json.contains("\"channel_count_mismatch\":true"),
        "differing channel counts must flag a mismatch: {json}"
    );
    assert!(
        json.contains("\"channels\":1") && json.contains("\"channels\":2"),
        "both channel counts are reported: {json}"
    );
    assert!(
        json.contains("\"sample_rate_mismatch\":false"),
        "matched sample rates must not flag a rate mismatch: {json}"
    );
}

#[test]
fn session_no_compatibility_mismatch_when_formats_match() {
    let server = launch(); // two matched fixtures (same rate, channels, duration)
    let json = session_json(&server);
    assert!(
        json.contains("\"sample_rate_mismatch\":false"),
        "matched sample rates must not flag a mismatch: {json}"
    );
    assert!(
        json.contains("\"channel_count_mismatch\":false"),
        "matched channel counts must not flag a mismatch: {json}"
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
    // A throwaway cache: even a run that fails on its arguments creates the
    // cache directory, and that must never be the developer's real one.
    let cache = TempDir::new("fail-cache");
    let out = Command::new(BIN)
        .args(args)
        .env("XDG_CACHE_HOME", &cache.path)
        .output()
        .expect("spawn binary");
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
/// stereo, filled with deterministic seeded noise scaled by `amp` (1.0 = full
/// range). A simple `fmt ` chunk (tag 1 integer, tag 3 IEEE float) — enough for
/// symphonia to decode as a source.
///
/// `amp` is what makes a loudness fixture: two files sharing a seed but written
/// at different amplitudes are the same noise, one lane quieter, so the match
/// has an unambiguous louder lane to attenuate. Scaling goes through `f64`, so
/// `amp = 1.0` reproduces the unscaled sample exactly at every depth.
#[allow(clippy::too_many_arguments)]
fn write_wav_fmt(
    path: &Path,
    frames: u32,
    sample_rate: u32,
    channels: u16,
    bits: u16,
    float: bool,
    seed: u32,
    amp: f64,
) {
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    let data_len = frames * block_align as u32;

    let mut fmt = Vec::new();
    fmt.extend_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&sample_rate.to_le_bytes());
    fmt.extend_from_slice(&byte_rate.to_le_bytes());
    fmt.extend_from_slice(&block_align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    let mut buf = riff_wave(&fmt, data_len);

    let scaled = |sample: i64| (sample as f64 * amp) as i64;
    let mut next = noise(seed);
    for _ in 0..frames {
        for _ in 0..channels {
            let s = next();
            match (float, bits) {
                (true, 32) => {
                    let f = (s as f32 / u32::MAX as f32 - 0.5) * amp as f32;
                    buf.extend_from_slice(&f.to_le_bytes());
                }
                (false, 16) => {
                    buf.extend_from_slice(&(scaled((s >> 16) as i16 as i64) as i16).to_le_bytes())
                }
                (false, 24) => {
                    // Sign-extend the low 24 bits, scale, and write the low three
                    // bytes back — the same bytes as the raw word when amp is 1.
                    let raw = ((s << 8) as i32) >> 8;
                    let value = scaled(raw as i64) as i32;
                    buf.extend_from_slice(&value.to_le_bytes()[0..3]);
                }
                (false, 32) => {
                    buf.extend_from_slice(&(scaled(s as i32 as i64) as i32).to_le_bytes())
                }
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

    let mut fmt = Vec::new();
    fmt.extend_from_slice(&0xFFFEu16.to_le_bytes()); // WAVE_FORMAT_EXTENSIBLE
    fmt.extend_from_slice(&channels.to_le_bytes());
    fmt.extend_from_slice(&sample_rate.to_le_bytes());
    fmt.extend_from_slice(&byte_rate.to_le_bytes());
    fmt.extend_from_slice(&block_align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    fmt.extend_from_slice(&22u16.to_le_bytes()); // cbSize
    fmt.extend_from_slice(&bits.to_le_bytes()); // valid bits per sample
    fmt.extend_from_slice(&((1u32 << channels) - 1).to_le_bytes()); // channel mask
                                                                    // PCM subformat GUID.
    fmt.extend_from_slice(&[
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B,
        0x71,
    ]);
    let mut buf = riff_wave(&fmt, data_len);

    let mut next = noise(seed);
    for _ in 0..frames {
        for _ in 0..channels {
            buf.extend_from_slice(&((next() >> 16) as i16).to_le_bytes());
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

/// The fields of a WAV proxy the contract cares about — the RIFF counterpart of
/// `FlacInfo`, walked chunk by chunk so the plain and EXTENSIBLE `fmt ` layouts
/// both parse, and so the frame count comes from the `data` chunk rather than a
/// fixed offset.
struct WavInfo {
    sample_rate: u32,
    channels: u16,
    bits: u16,
    total_samples: u64,
}

fn parse_wav(bytes: &[u8]) -> WavInfo {
    assert_eq!(&bytes[0..4], b"RIFF", "proxy is a RIFF/WAV stream");
    assert_eq!(&bytes[8..12], b"WAVE", "proxy is a WAVE stream");

    let (mut sample_rate, mut channels, mut bits, mut data_len) = (0u32, 0u16, 0u16, 0u32);
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = i + 8;
        if id == b"fmt " {
            channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
            sample_rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
            bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
        } else if id == b"data" {
            data_len = size as u32;
        }
        // RIFF chunks are word-aligned: an odd body carries a pad byte.
        i = body + size + size % 2;
    }

    let block_align = channels as u64 * (bits as u64 / 8);
    assert!(block_align > 0, "proxy declares a usable frame size");
    WavInfo {
        sample_rate,
        channels,
        bits,
        total_samples: data_len as u64 / block_align,
    }
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
        write_wav_fmt(&a, 44_100, 48_000, 2, bits, float, 11, 1.0);
        write_wav_fmt(&b, 44_100, 48_000, 2, bits, float, 12, 1.0);
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
    let info = parse_wav(&body);
    assert_eq!(info.bits, 16, "a 16-bit source stays 16-bit");
    assert_eq!(info.channels, 10, "every channel survives the fallback");
    assert_eq!(info.sample_rate, 44_100, "sample rate preserved");
    assert_eq!(
        info.total_samples, 22_050,
        "the fallback proxy carries the source's sample count"
    );
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
/// Spelled out here on purpose: the binary reads this value out of the embedded
/// schema, so an independent copy is what makes a silent change to the published
/// id fail a test rather than pass unnoticed.
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
    // A sighted conclude reveals nothing: it concealed nothing (ADR-0006), and
    // the page has named both files since load.
    assert!(
        response.get("reveal").is_none(),
        "a sighted conclude carries no reveal: {resp}"
    );
    let record_path = sole_record(&cwd.path);
    assert_eq!(
        reported,
        record_path.to_string_lossy(),
        "the reported path is the written file"
    );

    let record = validate_record_file(&record_path);
    // Server-authoritative fields: schema id, ULID, mode, and a playback object
    // stating loudness matching was off (issue #30 — no `--loudness-match`).
    assert_eq!(record["schema"], RECORD_SCHEMA_ID);
    assert_eq!(record["mode"], "ab");
    assert_eq!(
        record["playback"],
        serde_json::json!({ "loudness_match": { "enabled": false } })
    );
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
fn conclude_ends_the_session_and_exits_zero_in_standalone_mode() {
    // Save-and-close is one act (spec #42 slice 5, #67 res. 8): the successful
    // write concludes the session, the server shuts down, and the process exits 0.
    // This supersedes M3's serve-forever-after-conclude — a second conclude is
    // impossible because the process is gone.
    let cwd = TempDir::new("record-lifecycle");
    let mut server = launch_recording(&cwd.path, None);
    let body =
        r#"{"result": {"preference": "A", "confidence": 5}, "observations": [], "loops": []}"#;

    let (status, _, _) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(status, 200, "the conclude writes the record");

    // The process exits, and it exits 0: a standalone save is a clean close. The
    // wait() returning at all is the proof the server did not serve forever.
    let code = server.child.wait().expect("child exits").code();
    assert_eq!(code, Some(0), "a saved standalone session exits 0");

    // Exactly one record, written once.
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
fn conclude_refuses_records_outside_the_v0_contract() {
    let cwd = TempDir::new("record-contract");
    let server = launch_recording(&cwd.path, None);
    let url = format!("/record?token={}", server.token);

    for (case, body) in [
        // v0.1 records the listener's one region; multiple simultaneous loop
        // regions are out of scope, so the schema caps `loops[]` at one.
        (
            "two loop regions",
            r#"{"result": {"preference": null}, "observations": [],
                "loops": [{"start_ms": 0, "end_ms": 100}, {"start_ms": 200, "end_ms": 300}]}"#,
        ),
        // Unknown plain fields are invalid inside every object the body owns —
        // `ext` is the only way through, and it is typed.
        (
            "an unknown result field",
            r#"{"result": {"preference": null, "smuggled": 1}, "observations": [], "loops": []}"#,
        ),
        (
            "an unknown observation field",
            r#"{"result": {"preference": null},
                "observations": [{"at": "2026-08-08T10:00:00Z", "text": "hi", "smuggled": 1}],
                "loops": []}"#,
        ),
        // Candidate references must name a candidate this session loaded.
        (
            "a preference naming no loaded candidate",
            r#"{"result": {"preference": "Z", "confidence": 3}, "observations": [], "loops": []}"#,
        ),
        (
            "an observation on no loaded candidate",
            r#"{"result": {"preference": null},
                "observations": [{"at": "2026-08-08T10:00:00Z", "candidate": "Z", "text": "hi"}],
                "loops": []}"#,
        ),
    ] {
        let (status, _, resp) = http_post(&server, &url, body);
        assert_eq!(
            status,
            400,
            "[{case}] must be refused: {}",
            String::from_utf8_lossy(&resp)
        );
    }

    // A refused conclude writes nothing — and leaves the session concludable,
    // so the listener can fix the cause and try again.
    assert!(
        std::fs::read_dir(&cwd.path).unwrap().next().is_none(),
        "a refused conclude leaves no file behind"
    );
    // The same body, valid — plus a stray top-level key. The server assembles
    // the record from the fields it knows, so a top-level extra is dropped on
    // the floor rather than engraved.
    let good = r#"{"result": {"preference": "B", "confidence": 4}, "observations": [],
                   "loops": [{"start_ms": 0, "end_ms": 100}], "smuggled": 1}"#;
    let (status, _, resp) = http_post(&server, &url, good);
    assert_eq!(
        status,
        200,
        "a valid record still concludes after refusals: {}",
        String::from_utf8_lossy(&resp)
    );
    let record = validate_record_file(&sole_record(&cwd.path));
    assert!(
        record.get("smuggled").is_none(),
        "the server assembles the record from known fields only: {record}"
    );
}

// --- Issue #30: loudness matching (BS.1770 measure, static lane gains) --------

/// A 16-bit PCM WAV of seeded noise at `amp` of full range — the loudness
/// fixture shape, over the one WAV writer every other fixture uses.
fn write_wav_at(path: &Path, frames: u32, sample_rate: u32, channels: u16, seed: u32, amp: f64) {
    write_wav_fmt(path, frames, sample_rate, channels, 16, false, seed, amp);
}

/// A loudness-offset fixture pair: candidate A full-scale, candidate B the same
/// seeded noise scaled down ~6 dB, so A is unambiguously the louder lane.
fn loudness_pair(dir: &TempDir) -> (PathBuf, PathBuf) {
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav_at(&a, 44_100, 44_100, 2, 1, 1.0);
    write_wav_at(&b, 44_100, 44_100, 2, 1, 0.5);
    (a, b)
}

#[test]
fn loudness_match_session_carries_nonpositive_gains_with_quietest_at_zero() {
    let dir = TempDir::new("loudness-session");
    let (a, b) = loudness_pair(&dir);
    let server = launch_opts(dir, &a, &b, None, &["--loudness-match"]);

    let json = session_json(&server);
    let session: serde_json::Value = serde_json::from_str(&json).expect("session json");
    let lm = &session["loudness_match"];

    assert_eq!(lm["enabled"], true, "matching is on: {json}");
    assert_eq!(
        lm["method"], "bs1770-integrated",
        "the method is named: {json}"
    );

    let ga = lm["candidates"]["A"]["gain_db"].as_f64().expect("A gain");
    let gb = lm["candidates"]["B"]["gain_db"].as_f64().expect("B gain");
    // Never boost: every lane's gain is ≤ 0.
    assert!(ga <= 0.0 && gb <= 0.0, "gains never boost: A={ga} B={gb}");
    // The quietest lane (B, scaled down) is the reference, untouched at 0 dB;
    // the louder lane (A) is pulled down below it.
    assert_eq!(gb, 0.0, "the quietest lane keeps exactly 0.0: {json}");
    assert!(ga < 0.0, "the louder lane is attenuated: A={ga}");

    // Ordering, not exact figures: A measured louder than B.
    let la = lm["candidates"]["A"]["measured_lufs"]
        .as_f64()
        .expect("A lufs");
    let lb = lm["candidates"]["B"]["measured_lufs"]
        .as_f64()
        .expect("B lufs");
    assert!(la > lb, "A is measured louder than B: A={la} B={lb}");
}

#[test]
fn without_the_flag_the_session_states_matching_off() {
    let server = launch(); // no --loudness-match
    let json = session_json(&server);
    let session: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert_eq!(
        session["loudness_match"],
        serde_json::json!({ "enabled": false }),
        "off by default, and the absence is stated: {json}"
    );
}

#[test]
fn loudness_match_record_playback_validates_against_the_pinned_schema() {
    let cwd = TempDir::new("loudness-record-cwd");
    let dir = TempDir::new("loudness-record-fixtures");
    let (a, b) = loudness_pair(&dir);
    let mut command = Command::new(BIN);
    command
        .arg(&a)
        .arg(&b)
        .arg("--loudness-match")
        .current_dir(&cwd.path);
    let server = serving_from(command, dir);

    let body =
        r#"{"result": {"preference": "A", "confidence": 3}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(
        status,
        200,
        "a matched session concludes: {}",
        String::from_utf8_lossy(&resp)
    );

    // validate_record_file asserts conformance to the pinned v0 schema shape.
    let record = validate_record_file(&sole_record(&cwd.path));
    let lm = &record["playback"]["loudness_match"];
    assert_eq!(
        lm["enabled"], true,
        "the record states matching was on: {record}"
    );
    assert_eq!(lm["method"], "bs1770-integrated");
    assert_eq!(
        lm["candidates"]["B"]["gain_db"].as_f64(),
        Some(0.0),
        "the quietest lane recorded at 0.0: {record}"
    );
    assert!(
        lm["candidates"]["A"]["gain_db"].as_f64().unwrap() < 0.0,
        "the louder lane recorded attenuated: {record}"
    );
}

#[test]
fn a_lane_below_the_gate_records_a_null_measurement_not_a_zero() {
    // A silent (or sub-gate) lane reads -inf LUFS from the meter. JSON has no
    // -inf, and the two ways of writing it down are not equally honest: 0.0 is
    // the *loudest possible* figure and would be indistinguishable from a real
    // reading, while null says the true thing — the gate produced no reading.
    // The gain is still a number (exactly 0 dB: an unmeasurable lane is never
    // attenuated, and never becomes the match reference).
    let cwd = TempDir::new("silent-lane-cwd");
    let dir = TempDir::new("silent-lane-fixtures");
    let a = dir.join("silent.wav");
    let b = dir.join("noise.wav");
    write_wav_at(&a, 44_100, 44_100, 2, 1, 0.0); // digital silence
    write_wav_at(&b, 44_100, 44_100, 2, 2, 1.0);
    let mut command = Command::new(BIN);
    command
        .arg(&a)
        .arg(&b)
        .arg("--loudness-match")
        .current_dir(&cwd.path);
    let server = serving_from(command, dir);

    // The session says the same thing the record will.
    let json = session_json(&server);
    let session: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert_eq!(
        session["loudness_match"]["candidates"]["A"]["measured_lufs"],
        serde_json::Value::Null,
        "the silent lane has no reading to report: {json}"
    );

    let body =
        r#"{"result": {"preference": "B", "confidence": 3}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(
        status,
        200,
        "an unmeasurable lane still concludes: {}",
        String::from_utf8_lossy(&resp)
    );

    // validate_record_file asserts conformance to the pinned v0 schema, so this
    // also pins that the schema admits a null measurement.
    let record = validate_record_file(&sole_record(&cwd.path));
    let lm = &record["playback"]["loudness_match"];
    assert_eq!(
        lm["candidates"]["A"]["measured_lufs"],
        serde_json::Value::Null,
        "the record states the absence rather than inventing 0.0 LUFS: {record}"
    );
    assert_eq!(
        lm["candidates"]["A"]["gain_db"].as_f64(),
        Some(0.0),
        "an unmeasurable lane is left untouched: {record}"
    );
    assert_eq!(
        lm["candidates"]["B"]["gain_db"].as_f64(),
        Some(0.0),
        "the only measurable lane is the reference: {record}"
    );
    assert!(
        lm["candidates"]["B"]["measured_lufs"].as_f64().is_some(),
        "the measurable lane still reports its figure: {record}"
    );
}

#[test]
fn without_the_flag_the_record_states_matching_off() {
    let cwd = TempDir::new("loudness-off-record");
    let server = launch_recording(&cwd.path, None);
    let body = r#"{"result": {"preference": null}, "observations": [], "loops": []}"#;
    let (status, _, _) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(status, 200);
    let record = validate_record_file(&sole_record(&cwd.path));
    assert_eq!(
        record["playback"]["loudness_match"],
        serde_json::json!({ "enabled": false }),
        "matching off is stated in the record: {record}"
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

// --- Issue #28: blind sessions (shuffle, concealment, refusals, reconnection) --

/// The two files' `{sha256, size}` — the identifying strings a blind session of
/// that pair must never leak, and the ones a sighted session reports. Computed
/// here from the bytes on disk, so the expectation is an independent oracle
/// rather than whatever the binary happens to say (and no second server has to
/// be launched to learn two hashes).
fn identities(a: &Path, b: &Path) -> [(String, u64); 2] {
    let identity = |path: &Path| {
        let bytes = std::fs::read(path).expect("fixture readable");
        (
            uncompose_compare::hex(&sha2::Sha256::digest(&bytes)),
            bytes.len() as u64,
        )
    };
    [identity(a), identity(b)]
}

/// Assert that a blind response carries no identifying string of either input:
/// no basename, no fixture path, no content hash, and no decimal file size.
///
/// The opaque audio references are blanked first. A reference is 32 hex
/// characters of OS randomness and an all-digit size like `176444` is itself
/// valid hex, so scanning the raw text for one can match random noise a few
/// times in 100 000 runs and fail a session that leaked nothing. The references
/// are shown non-identifying by construction elsewhere in this file (never the
/// content hash, never content-derived), so the sweep covers everything else.
fn assert_no_identity_leak(body: &str, fixture_dir: &str, identities: &[(String, u64); 2]) {
    let swept = without_audio_refs(body);
    assert!(!swept.contains("alpha.wav"), "basename A leaked: {body}");
    assert!(!swept.contains("bravo.wav"), "basename B leaked: {body}");
    assert!(
        !swept.contains(fixture_dir),
        "the fixture path leaked: {body}"
    );
    for (sha, size) in identities {
        assert!(!swept.contains(sha.as_str()), "a sha256 leaked: {body}");
        assert!(
            !swept.contains(&size.to_string()),
            "a decimal size leaked: {body}"
        );
    }
}

/// `body` with any `/audio/<opaque reference>` replaced by a fixed placeholder.
/// Non-JSON bodies (an error string) pass through untouched — they carry no
/// reference to blank.
fn without_audio_refs(body: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    if let Some(candidates) = value["candidates"].as_array_mut() {
        for candidate in candidates {
            if candidate.get("audio").is_some() {
                candidate["audio"] = serde_json::json!("/audio/<opaque>");
            }
        }
    }
    value.to_string()
}

#[test]
fn blind_session_conceals_every_identifying_detail() {
    // Files kept alive by the test across both a sighted and a blind launch.
    let files = TempDir::new("blind-files");
    let a = files.join("alpha.wav");
    let b = files.join("bravo.wav");
    write_wav(&a, 44_100, 44_100, 2, 21);
    write_wav(&b, 44_100, 44_100, 2, 22);
    let identities = identities(&a, &b);

    let server = launch_opts(TempDir::new("blind-conceal"), &a, &b, None, &["--blind"]);
    let json = session_json(&server);

    // The blind payload states it is blind and carries only label + duration +
    // an audio reference per candidate.
    let s: serde_json::Value = serde_json::from_str(&json).expect("blind session json");
    assert_eq!(s["blind"], true, "the session states it is blind: {json}");
    for label in ["A", "B"] {
        let has = s["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["label"] == label);
        assert!(has, "blind session labels {label}: {json}");
    }
    assert!(
        json.contains("\"duration_ms\":1000"),
        "the shared duration is carried: {json}"
    );

    // The concealment-leak sweep: no basename, path, sha256 hex, or decimal size
    // of either input appears anywhere in the session payload.
    assert_no_identity_leak(&json, &files.path.to_string_lossy(), &identities);

    // The audio reference is opaque, not the content hash — and the content-hash
    // URL is not servable in blind mode.
    let audio = s["candidates"][0]["audio"].as_str().expect("audio url");
    let reference = audio.strip_prefix("/audio/").expect("an /audio/ url");
    for (sha, _) in &identities {
        assert_ne!(reference, sha, "the audio ref must not be the content hash");
        let (status, _, _) = http_get(
            &server.addr,
            &format!("/audio/{sha}?token={}", server.token),
        );
        assert_eq!(
            status, 404,
            "a content-hash URL must not serve in blind mode"
        );
    }

    // The opaque reference still serves a proxy that decodes.
    let (status, headers, bytes) =
        http_get(&server.addr, &format!("{audio}?token={}", server.token));
    assert_eq!(status, 200, "the opaque audio reference serves the proxy");
    assert!(!bytes.is_empty(), "the served proxy is non-empty");
    assert!(
        headers.contains("audio/"),
        "the proxy carries an audio content-type: {headers}"
    );
}

#[test]
fn blind_error_bodies_conceal_identity_too() {
    // Testing decision 1 sweeps "the session payload, any audio URL, or any error
    // body of a blind session". The refusal paths are the ones that could leak by
    // accident — a validation failure that echoed the record it refused would hand
    // back the very paths and hashes the session is concealing — so every error a
    // running blind session can produce is swept, not just its happy path.
    let files = TempDir::new("blind-error-files");
    let a = files.join("alpha.wav");
    let b = files.join("bravo.wav");
    write_wav(&a, 44_100, 44_100, 2, 61);
    write_wav(&b, 44_100, 44_100, 2, 62);
    let identities = identities(&a, &b);
    let fixture_dir = files.path.to_string_lossy().to_string();

    let cwd = TempDir::new("blind-error-cwd");
    let mut command = Command::new(BIN);
    command
        .arg(&a)
        .arg(&b)
        .arg("--blind")
        .current_dir(&cwd.path);
    let server = serving_from(command, TempDir::new("blind-error-hold"));
    let tokened = format!("/record?token={}", server.token);

    // An unknown audio reference: the 404 an unguessable reference resolves to.
    let (status, _, body) = http_get(
        &server.addr,
        &format!("/audio/{}?token={}", "f".repeat(32), server.token),
    );
    assert_eq!(status, 404, "an unknown reference is a 404");
    assert_no_identity_leak(&String::from_utf8_lossy(&body), &fixture_dir, &identities);

    // A missing token: the blanket refusal body.
    let (status, _, body) = http_get(&server.addr, "/session");
    assert_eq!(status, 403, "an untokened request is refused");
    assert_no_identity_leak(&String::from_utf8_lossy(&body), &fixture_dir, &identities);

    // Every conclude the endpoint can refuse: unparseable, schema-invalid, and a
    // reference to a candidate this session never loaded. None of them writes a
    // record, so the session stays concludable and each refusal is independent.
    let refused = [
        "not json at all",
        r#"{"result": {"preference": "A", "confidence": 9}, "observations": [], "loops": []}"#,
        r#"{"result": {"preference": "A", "confidence": 3}, "observations": "nope", "loops": []}"#,
        r#"{"result": {"preference": "Z", "confidence": 3}, "observations": [], "loops": []}"#,
        r#"{"result": {"preference": null}, "observations": [{"at": "2026-08-08T10:00:00Z", "candidate": "Z", "text": "?"}], "loops": []}"#,
    ];
    for body in refused {
        let (status, _, response) = http_post(&server, &tokened, body);
        let response = String::from_utf8_lossy(&response).to_string();
        assert_eq!(status, 400, "the endpoint refuses {body}: {response}");
        assert_no_identity_leak(&response, &fixture_dir, &identities);
    }

    // Nothing was written, so none of those refusals concluded the session.
    assert!(
        std::fs::read_dir(&cwd.path).unwrap().next().is_none(),
        "a refused conclude writes no record"
    );
}

/// Run a blind invocation expected to be refused pre-bind, returning
/// (exit code, stderr). The caller keeps the fixture files alive.
fn blind_refusal(a: &Path, b: &Path) -> (Option<i32>, String) {
    run_expecting_failure(&[a.to_str().unwrap(), b.to_str().unwrap(), "--blind"])
}

#[test]
fn blind_refuses_identical_content() {
    let dir = TempDir::new("blind-identical");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    // Same params and seed => byte-identical => equal sha256.
    write_wav(&a, 44_100, 44_100, 2, 5);
    write_wav(&b, 44_100, 44_100, 2, 5);
    let (code, stderr) = blind_refusal(&a, &b);
    assert_eq!(code, Some(1), "a blind refusal exits 1");
    let lc = stderr.to_lowercase();
    assert!(
        lc.contains("blind") && lc.contains("identical"),
        "identical content is named: {stderr}"
    );
}

#[test]
fn blind_refuses_duration_mismatch() {
    let dir = TempDir::new("blind-duration");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 1); // 1000 ms
    write_wav(&b, 33_075, 44_100, 2, 2); // 750 ms
    let (code, stderr) = blind_refusal(&a, &b);
    assert_eq!(code, Some(1), "a blind refusal exits 1");
    let lc = stderr.to_lowercase();
    assert!(
        lc.contains("duration") && stderr.contains("1000") && stderr.contains("750"),
        "duration mismatch names the property and both values: {stderr}"
    );
}

#[test]
fn blind_duration_refusal_names_two_distinguishable_values() {
    // A one-frame difference is still a refusal, and the message has to name two
    // values a reader can tell apart — "1000 ms vs 1000 ms" names a mismatch
    // between two equal numbers, which reads as a bug rather than a reason.
    let dir = TempDir::new("blind-duration-subms");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 1);
    write_wav(&b, 44_101, 44_100, 2, 2); // one frame longer: ~0.023 ms
    let (code, stderr) = blind_refusal(&a, &b);
    assert_eq!(code, Some(1), "a blind refusal exits 1");
    assert!(
        stderr.to_lowercase().contains("duration"),
        "the property is named: {stderr}"
    );
    assert!(
        stderr.contains("44100") && stderr.contains("44101"),
        "both values are named, distinguishably: {stderr}"
    );
}

#[test]
fn blind_refuses_sample_rate_mismatch() {
    let dir = TempDir::new("blind-rate");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    // Equal duration and channels, differing sample rate only.
    write_wav(&a, 44_100, 44_100, 2, 1);
    write_wav(&b, 48_000, 48_000, 2, 2);
    let (code, stderr) = blind_refusal(&a, &b);
    assert_eq!(code, Some(1), "a blind refusal exits 1");
    let lc = stderr.to_lowercase();
    assert!(
        lc.contains("sample-rate") && stderr.contains("44100") && stderr.contains("48000"),
        "sample-rate mismatch names the property and both values: {stderr}"
    );
}

#[test]
fn blind_refuses_channel_count_mismatch() {
    let dir = TempDir::new("blind-channels");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    // Equal duration and sample rate, differing channel count only.
    write_wav(&a, 44_100, 44_100, 1, 1);
    write_wav(&b, 44_100, 44_100, 2, 2);
    let (code, stderr) = blind_refusal(&a, &b);
    assert_eq!(code, Some(1), "a blind refusal exits 1");
    assert!(
        stderr.to_lowercase().contains("channel-count"),
        "channel-count mismatch is named: {stderr}"
    );
}

#[test]
fn blind_record_reconnects_labels_to_the_real_hashes() {
    let cwd = TempDir::new("blind-record-cwd");
    let files = TempDir::new("blind-record-files");
    let a = files.join("alpha.wav");
    let b = files.join("bravo.wav");
    write_wav(&a, 44_100, 44_100, 2, 31);
    write_wav(&b, 44_100, 44_100, 2, 32);
    let identities = identities(&a, &b);
    let input_hashes: std::collections::HashSet<String> =
        identities.iter().map(|(sha, _)| sha.clone()).collect();

    let mut command = Command::new(BIN);
    command
        .arg(&a)
        .arg(&b)
        .arg("--blind")
        .current_dir(&cwd.path);
    let server = serving_from(command, TempDir::new("blind-record-hold"));

    let body =
        r#"{"result": {"preference": "A", "confidence": 4}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(
        status,
        200,
        "a blind session concludes: {}",
        String::from_utf8_lossy(&resp)
    );

    let record = validate_record_file(&sole_record(&cwd.path));
    assert_eq!(
        record["mode"], "ab-blind-randomized",
        "the record marks the blind, randomized session: {record}"
    );

    // Labels {A, B} map bijectively onto the two real input hashes — the record
    // alone reconnects what the listener saw to what was on disk. (The shuffle
    // itself is OS randomness; we assert reconnection, not distribution.)
    let candidates = record["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    let labels: std::collections::HashSet<&str> = candidates
        .iter()
        .map(|c| c["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        std::collections::HashSet::from(["A", "B"]),
        "both labels present exactly once: {record}"
    );
    let recorded: std::collections::HashSet<String> = candidates
        .iter()
        .map(|c| c["sha256"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        recorded, input_hashes,
        "the recorded hashes are exactly the two inputs': {record}"
    );
}

#[test]
fn blind_conclude_reveals_the_mapping_matching_the_record() {
    // Reveal at conclude (issue #29): the successful conclude response carries the
    // label→file mapping the UI displays, and it is identical to what the record
    // on disk carries. The reveal is the single irreversible event's payload — the
    // browser never received identity before it.
    let cwd = TempDir::new("blind-reveal-cwd");
    let files = TempDir::new("blind-reveal-files");
    let a = files.join("alpha.wav");
    let b = files.join("bravo.wav");
    write_wav(&a, 44_100, 44_100, 2, 51);
    write_wav(&b, 44_100, 44_100, 2, 52);

    let mut command = Command::new(BIN);
    command
        .arg(&a)
        .arg(&b)
        .arg("--blind")
        .current_dir(&cwd.path);
    let server = serving_from(command, TempDir::new("blind-reveal-hold"));

    let body =
        r#"{"result": {"preference": "A", "confidence": 4}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(
        status,
        200,
        "a blind session concludes: {}",
        String::from_utf8_lossy(&resp)
    );

    let response: serde_json::Value =
        serde_json::from_slice(&resp).expect("conclude response is json");
    let reveal = response["reveal"]
        .as_array()
        .expect("the conclude response reveals the label→file mapping");
    assert_eq!(reveal.len(), 2, "both labels revealed: {response}");

    // The reveal reconnects each label to its real file, identical to the record.
    let record = validate_record_file(&sole_record(&cwd.path));
    let record_candidates = record["candidates"].as_array().unwrap();
    for label in ["A", "B"] {
        let r = reveal
            .iter()
            .find(|c| c["label"] == label)
            .unwrap_or_else(|| panic!("reveal names label {label}: {response}"));
        let rec = record_candidates
            .iter()
            .find(|c| c["label"] == label)
            .unwrap();
        assert_eq!(
            r["path"], rec["path"],
            "reveal path matches record for {label}"
        );
        assert_eq!(
            r["sha256"], rec["sha256"],
            "reveal sha256 matches record for {label}"
        );
        assert_eq!(
            r["size"], rec["size"],
            "reveal size matches record for {label}"
        );
    }
}

// --- Issue #32: blind loudness concealment (matching active, numbers hidden) --

#[test]
fn blind_loudness_conceals_measured_lufs_but_carries_applied_gains() {
    // `--blind --loudness-match` composes into a level-fair blind test: the
    // session payload carries no more than the gains the engine must apply. A
    // measured LUFS figure fingerprints a candidate, so it is held back until the
    // conclude reveal — the payload states matching is on and gives per-lane
    // gains, but no `measured_lufs` anywhere.
    let dir = TempDir::new("blind-loudness-session");
    let (a, b) = loudness_pair(&dir);
    let server = launch_opts(dir, &a, &b, None, &["--blind", "--loudness-match"]);

    let json = session_json(&server);
    let s: serde_json::Value = serde_json::from_str(&json).expect("blind session json");
    assert_eq!(s["blind"], true, "the session states it is blind: {json}");

    let lm = &s["loudness_match"];
    assert_eq!(lm["enabled"], true, "matching is active in blind: {json}");
    assert_eq!(
        lm["method"], "bs1770-integrated",
        "the method is named: {json}"
    );

    // Only the applied gains ride along — never boost, and (the shuffle hides
    // which label is which file) the quietest lane at exactly 0.0 with the louder
    // lane attenuated below it.
    let ga = lm["candidates"]["A"]["gain_db"].as_f64().expect("A gain");
    let gb = lm["candidates"]["B"]["gain_db"].as_f64().expect("B gain");
    assert!(ga <= 0.0 && gb <= 0.0, "gains never boost: A={ga} B={gb}");
    assert!(
        (ga == 0.0 && gb < 0.0) || (gb == 0.0 && ga < 0.0),
        "one lane is the 0 dB reference and the other is attenuated: A={ga} B={gb}"
    );

    // The concealment-leak sweep with matching on: no measured figure — not the
    // key, not a value — reaches any pre-conclude response.
    assert!(
        !json.contains("measured_lufs"),
        "a measured LUFS figure leaked into the blind session: {json}"
    );
}

#[test]
fn blind_loudness_reveal_and_record_carry_the_full_figures() {
    // After conclude, the reveal carries the loudness figures alongside the
    // label→file mapping, and the record's `playback.loudness_match` carries the
    // full per-label numbers as in sighted mode. The reveal is the one
    // irreversible event's payload; the browser saw no measured figure before it.
    let cwd = TempDir::new("blind-loudness-reveal-cwd");
    let files = TempDir::new("blind-loudness-reveal-files");
    let (a, b) = loudness_pair(&files);

    let mut command = Command::new(BIN);
    command
        .arg(&a)
        .arg(&b)
        .arg("--blind")
        .arg("--loudness-match")
        .current_dir(&cwd.path);
    let server = serving_from(command, TempDir::new("blind-loudness-reveal-hold"));

    let body =
        r#"{"result": {"preference": "A", "confidence": 4}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(
        status,
        200,
        "a blind matched session concludes: {}",
        String::from_utf8_lossy(&resp)
    );

    let response: serde_json::Value =
        serde_json::from_slice(&resp).expect("conclude response is json");
    let reveal = response["reveal"].as_array().expect("the reveal mapping");

    // The record carries the full loudness_match object, per-label measured and
    // applied, exactly as sighted mode.
    let record = validate_record_file(&sole_record(&cwd.path));
    assert_eq!(
        record["mode"], "ab-blind-randomized",
        "blind record: {record}"
    );
    let lm = &record["playback"]["loudness_match"];
    assert_eq!(
        lm["enabled"], true,
        "the record states matching was on: {record}"
    );
    assert_eq!(lm["method"], "bs1770-integrated");

    // The reveal reconnects each label to its file *and* its loudness figures,
    // matching the record on both.
    for label in ["A", "B"] {
        let r = reveal
            .iter()
            .find(|c| c["label"] == label)
            .unwrap_or_else(|| panic!("reveal names label {label}: {response}"));
        let rec_lm = &lm["candidates"][label];
        assert_eq!(
            r["measured_lufs"].as_f64(),
            rec_lm["measured_lufs"].as_f64(),
            "reveal measured LUFS matches record for {label}: {response}"
        );
        assert_eq!(
            r["gain_db"].as_f64(),
            rec_lm["gain_db"].as_f64(),
            "reveal gain matches record for {label}: {response}"
        );
    }
    // The blind shuffle hides which label holds the scaled file, but one lane is
    // always the 0 dB reference and the other is attenuated below it.
    let ga = lm["candidates"]["A"]["gain_db"].as_f64().unwrap();
    let gb = lm["candidates"]["B"]["gain_db"].as_f64().unwrap();
    assert!(
        (ga == 0.0 && gb < 0.0) || (gb == 0.0 && ga < 0.0),
        "one lane is the 0 dB reference and the other attenuated: A={ga} B={gb}"
    );
}

#[test]
fn sighted_mode_is_unaffected_by_the_blind_flag() {
    // A plain (no --blind) session still carries full metadata and mode "ab".
    let dir = TempDir::new("blind-sighted-unaffected");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_wav(&a, 44_100, 44_100, 2, 41);
    write_wav(&b, 44_100, 44_100, 2, 42);
    let server = launch_with(dir, &a, &b);
    let json = session_json(&server);
    assert!(
        json.contains("\"sha256\":\"") && json.contains("\"path\":\""),
        "a sighted session still carries identities: {json}"
    );
    assert!(
        !json.contains("\"blind\":true"),
        "a sighted session is not marked blind: {json}"
    );
    // The audio URL is still the content-hash URL a sighted session has always
    // served.
    let sha = first_sha256(&json);
    assert!(
        json.contains(&format!("/audio/{sha}")),
        "sighted audio is served by content hash: {json}"
    );
}

// --- Issue #40 / spec #42: project refs and the SRC lane ----------------------

/// The sha256 of a file on disk — the figure a manifest records for an asset,
/// computed here so the fixture manifest is a truthful oracle.
fn file_sha256(path: &Path) -> String {
    uncompose_compare::hex(&sha2::Sha256::digest(
        std::fs::read(path).expect("read fixture"),
    ))
}

/// Write a WAV asset at `rel` under `root` (creating parent dirs) and return its
/// sha256, so the manifest can record what the loader will hash.
fn wav_asset(root: &Path, rel: &str, seed: u32, amp: f64) -> String {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("asset dir");
    write_wav_fmt(&path, 44_100, 44_100, 2, 16, false, seed, amp);
    file_sha256(&path)
}

/// Install `script` as an executable `uncompose-project` in a fresh temp dir.
/// Returns the dir holding it (kept alive by the caller) and a `PATH` value
/// that finds it.
fn project_tool_from(script: &str) -> (TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("project-tool");
    let tool = dir.join("uncompose-project");
    std::fs::write(&tool, script).expect("write stub");
    let mut perms = std::fs::metadata(&tool).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&tool, perms).unwrap();
    let path = format!(
        "{}:{}",
        dir.path.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (dir, path)
}

/// A stub `uncompose-project` executable on a fresh PATH: project-mode pre-flight
/// requires the tool be installed, but this repo stubs its evaluation import
/// (spec #42), so a no-op executable satisfies the on-PATH check.
fn stub_project_tool() -> (TempDir, String) {
    project_tool_from("#!/bin/sh\nexit 0\n")
}

/// A stub `uncompose-project` that records the argv it received (one arg per
/// line) to `argv_log`, optionally prints `stderr_msg` to stderr, and exits
/// `code` — the fake-tool seam the slice-5 auto-import handover runs against.
/// `argv_log` is baked into the script, so the handover needs no environment
/// beyond PATH.
fn logging_project_tool(argv_log: &Path, code: i32, stderr_msg: &str) -> (TempDir, String) {
    let log = argv_log.to_string_lossy();
    let stderr_line = if stderr_msg.is_empty() {
        String::new()
    } else {
        format!("echo {stderr_msg:?} >&2\n")
    };
    project_tool_from(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {log:?}\n{stderr_line}exit {code}\n"
    ))
}

/// A project with two candidate mixes derived from one shared raw source. The two
/// outputs share the basename-stem `vocals` (in distinct directories), so
/// `vocals@deriv-a` / `vocals@deriv-b` disambiguate by derivation; both
/// derivations take `raw` as input, so `raw` is the auto-resolved SRC lane.
struct Project {
    dir: TempDir,
    id: String,
}

fn build_project() -> Project {
    let dir = TempDir::new("project");
    let id = "01PROJECTULID00000000000000".to_string();
    let raw_sha = wav_asset(&dir.path, "src/take.wav", 1, 1.0);
    let a_sha = wav_asset(&dir.path, "out/a/vocals.wav", 2, 1.0);
    let b_sha = wav_asset(&dir.path, "out/b/vocals.wav", 3, 1.0);
    let manifest = serde_json::json!({
        "schema": "https://uncompose.org/schemas/project/v0/uncompose.project.json",
        "project": {"id": id, "name": "take", "created_at": "2026-01-01T00:00:00Z"},
        "assets": [
            {"id": "raw", "path": "src/take.wav", "sha256": raw_sha},
            {"id": "mix-a", "path": "out/a/vocals.wav", "sha256": a_sha},
            {"id": "mix-b", "path": "out/b/vocals.wav", "sha256": b_sha}
        ],
        "derivations": [
            {"id": "deriv-a", "inputs": ["raw"], "outputs": ["mix-a"]},
            {"id": "deriv-b", "inputs": ["raw"], "outputs": ["mix-b"]}
        ]
    });
    std::fs::write(
        dir.join("uncompose.project.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .expect("write manifest");
    Project { dir, id }
}

/// A project where every reachable ambiguity is present: one derivation
/// outputs two files sharing the basename-stem `vocals`, and it takes *both*
/// raws as input — so the two mixes share two possible sources. The
/// counterpart to `build_project`, which is unambiguous throughout.
fn build_ambiguous_project() -> Project {
    let dir = TempDir::new("project-ambiguous");
    let id = "01AMBIGUOUSULID0000000000000".to_string();
    let raw1_sha = wav_asset(&dir.path, "src/take-1.wav", 21, 1.0);
    let raw2_sha = wav_asset(&dir.path, "src/take-2.wav", 22, 1.0);
    let a_sha = wav_asset(&dir.path, "out/a/vocals.wav", 23, 1.0);
    let b_sha = wav_asset(&dir.path, "out/b/vocals.wav", 24, 1.0);
    let manifest = serde_json::json!({
        "schema": "https://uncompose.org/schemas/project/v0/uncompose.project.json",
        "project": {"id": id, "name": "take", "created_at": "2026-01-01T00:00:00Z"},
        "assets": [
            {"id": "raw-1", "path": "src/take-1.wav", "sha256": raw1_sha},
            {"id": "raw-2", "path": "src/take-2.wav", "sha256": raw2_sha},
            {"id": "mix-a", "path": "out/a/vocals.wav", "sha256": a_sha},
            {"id": "mix-b", "path": "out/b/vocals.wav", "sha256": b_sha}
        ],
        "derivations": [
            {"id": "mix", "inputs": ["raw-1", "raw-2"], "outputs": ["mix-a", "mix-b"]}
        ]
    });
    std::fs::write(
        dir.join("uncompose.project.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .expect("write manifest");
    Project { dir, id }
}

/// Launch a project session: `uncompose-compare --project <dir> <refs…> <extra…>`
/// with the stub tool on PATH and a chosen invoking directory (the record's
/// destination). Parses the tokened URL like every other launch.
fn launch_project(
    project: &Path,
    cwd: &Path,
    refs: &[&str],
    extra: &[&str],
    path_env: &str,
) -> Serving {
    let mut command = Command::new(BIN);
    command
        .arg("--project")
        .arg(project)
        .args(refs)
        .args(extra)
        .env("PATH", path_env)
        .current_dir(cwd);
    serving_from(command, TempDir::new("project-hold"))
}

/// Run a project invocation expected to fail before binding, returning
/// (exit code, stderr). `path_env` controls whether the stub tool is findable.
fn project_failure(
    project: &Path,
    refs: &[&str],
    extra: &[&str],
    path_env: &str,
) -> (Option<i32>, String) {
    let cache = TempDir::new("project-fail-cache");
    let out = Command::new(BIN)
        .arg("--project")
        .arg(project)
        .args(refs)
        .args(extra)
        .env("XDG_CACHE_HOME", &cache.path)
        .env("PATH", path_env)
        .output()
        .expect("spawn binary");
    assert!(!out.status.success(), "expected a non-zero exit");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn project_opens_a_three_lane_session_and_records_asset_ids() {
    // The DoD: `--project . vocals@deriv-a vocals@deriv-b` opens a three-lane
    // session resolved entirely from the manifest, and the record's candidates
    // carry their asset ids.
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();
    let cwd = TempDir::new("project-record-cwd");
    let server = launch_project(
        &project.dir.path,
        &cwd.path,
        &["vocals@deriv-a", "vocals@deriv-b"],
        &[],
        &path_env,
    );

    let json = session_json(&server);
    let s: serde_json::Value = serde_json::from_str(&json).expect("session json");

    // Two comparison candidates, plus an identified SRC lane — three lanes.
    let candidates = s["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2, "A and B are the comparison candidates");
    let source = &s["source"];
    assert_eq!(
        source["label"], "SRC",
        "the shared source is the SRC lane: {json}"
    );
    assert_eq!(
        source["name"], "take.wav",
        "the SRC lane resolves the shared raw source: {json}"
    );
    assert!(source["audio"].as_str().unwrap().starts_with("/audio/"));

    // Conclude and read the record: each candidate carries its manifest asset id
    // and the project ULID. The project record lands under `<root>/evaluations/`,
    // not the invoking directory (spec #42 slice 5).
    let body =
        r#"{"result": {"preference": "A", "confidence": 4}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(
        status,
        200,
        "a project session concludes: {}",
        String::from_utf8_lossy(&resp)
    );

    assert!(
        std::fs::read_dir(&cwd.path).unwrap().next().is_none(),
        "the project record does not land in the invoking directory"
    );
    let record = validate_record_file(&sole_record(&project.dir.path.join("evaluations")));
    let rc = record["candidates"].as_array().unwrap();
    let by_label = |label: &str| rc.iter().find(|c| c["label"] == label).unwrap();
    assert_eq!(
        by_label("A")["asset"],
        "mix-a",
        "A carries its asset id: {record}"
    );
    assert_eq!(
        by_label("B")["asset"],
        "mix-b",
        "B carries its asset id: {record}"
    );
    assert_eq!(
        by_label("A")["project"],
        project.id,
        "A carries the project ULID"
    );
    assert_eq!(
        by_label("B")["project"],
        project.id,
        "B carries the project ULID"
    );
}

#[test]
fn project_bare_id_resolves_and_omits_absent_source() {
    // Bare-id refs resolve to assets, and a pair that shares no producing-
    // derivation input has no SRC lane — a stated absence, not an error. `raw`
    // has no producer; `mix-a`'s producer input is `raw` (a candidate here), so
    // the pair shares nothing.
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();
    let cwd = TempDir::new("project-bare-cwd");
    let server = launch_project(
        &project.dir.path,
        &cwd.path,
        &["raw", "mix-a"],
        &[],
        &path_env,
    );

    let json = session_json(&server);
    let s: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert_eq!(s["candidates"].as_array().unwrap().len(), 2);
    assert!(
        s.get("source").is_none(),
        "a pair sharing no source has no SRC lane: {json}"
    );
}

#[test]
fn project_exclude_source_drops_the_shared_lane() {
    // The candidates share `raw`, but `--exclude-source` opts the SRC lane out.
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();
    let cwd = TempDir::new("project-exclude-cwd");
    let server = launch_project(
        &project.dir.path,
        &cwd.path,
        &["vocals@deriv-a", "vocals@deriv-b"],
        &["--exclude-source"],
        &path_env,
    );
    let json = session_json(&server);
    let s: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert!(
        s.get("source").is_none(),
        "--exclude-source omits the SRC lane even when one is shared: {json}"
    );
}

#[test]
fn project_states_the_absent_source_lane_at_launch() {
    // Sharing no source is a stated absence, not an error (ADR-0009): the launch
    // says so on stderr — stdout's first line stays the tokened URL — and then
    // serves the two-lane session.
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();
    let cwd = TempDir::new("project-note-cwd");
    let mut command = Command::new(BIN);
    command
        .arg("--project")
        .arg(&project.dir.path)
        .args(["raw", "mix-a"])
        .env("PATH", &path_env)
        .current_dir(&cwd.path)
        .stderr(Stdio::piped());
    // `serving_from` reads the URL from stdout and leaves stderr as piped here;
    // the note is printed before the URL, so it is already readable.
    let mut server = serving_from(command, TempDir::new("project-note-hold"));
    let stderr = server.child.stderr.take().expect("child stderr");
    let mut note = String::new();
    BufReader::new(stderr)
        .read_line(&mut note)
        .expect("read the launch note");
    assert!(
        note.contains("raw") && note.contains("mix-a") && note.contains("no SRC lane"),
        "the absent SRC lane is stated at launch, naming the pair: {note:?}"
    );
}

#[test]
fn project_ambiguous_refs_refuse_and_list_the_options() {
    let project = build_ambiguous_project();
    let (_tool, path_env) = stub_project_tool();

    // A basename-stem two outputs of the derivation share: refused, listing the
    // outputs by id and filename — the same shape the no-match message uses.
    // (A duplicated ref target by bare token is unreachable now: bare tokens
    // match asset ids, the manifest's own identity.)
    let (code, stderr) =
        project_failure(&project.dir.path, &["vocals@mix", "mix-b"], &[], &path_env);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("mix-a (out/a/vocals.wav)") && stderr.contains("mix-b (out/b/vocals.wav)"),
        "a shared basename-stem is refused, listing the derivation's outputs: {stderr}"
    );
}

#[test]
fn project_ambiguous_source_refuses_with_the_exclude_hint_that_works() {
    // The two mixes come out of one derivation taking both raws, so the SRC lane
    // has two candidates: refused before binding, with the options and the way
    // out named — and that way out actually launches.
    let project = build_ambiguous_project();
    let (_tool, path_env) = stub_project_tool();
    let (code, stderr) = project_failure(&project.dir.path, &["mix-a", "mix-b"], &[], &path_env);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("raw-1") && stderr.contains("raw-2") && stderr.contains("--exclude-source"),
        "an ambiguous source lists the options and the opt-out: {stderr}"
    );

    let cwd = TempDir::new("project-ambiguous-source-cwd");
    let server = launch_project(
        &project.dir.path,
        &cwd.path,
        &["mix-a", "mix-b"],
        &["--exclude-source"],
        &path_env,
    );
    let json = session_json(&server);
    let s: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert!(
        s.get("source").is_none(),
        "the hinted --exclude-source opens the session without a SRC lane: {json}"
    );
}

#[test]
fn project_no_match_lists_the_outputs() {
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();

    // A basename no output of the derivation carries: the message lists what the
    // derivation does output (id + filename) so the listener can pick.
    let (code, stderr) = project_failure(
        &project.dir.path,
        &["nope@deriv-a", "vocals@deriv-b"],
        &[],
        &path_env,
    );
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("nope") && stderr.contains("vocals.wav") && stderr.contains("mix-a"),
        "no-match lists the derivation's outputs with id and filename: {stderr}"
    );

    // An unknown id likewise names what it could not find.
    let (code, stderr) = project_failure(&project.dir.path, &["ghost", "mix-a"], &[], &path_env);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("no asset with id") && stderr.contains("ghost"),
        "an unknown id is named: {stderr}"
    );
}

#[test]
fn project_refuses_a_raw_path() {
    // A positional that looks like a path is refused in project mode.
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();
    let (code, stderr) = project_failure(
        &project.dir.path,
        &["out/a/vocals.wav", "mix-b"],
        &[],
        &path_env,
    );
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("looks like a path") && stderr.contains("out/a/vocals.wav"),
        "a raw path is refused, naming it: {stderr}"
    );
}

#[test]
fn project_and_out_conflict() {
    // `--out` conflicts with `--project` (the project record's destination is the
    // handover's, slice 5). clap rejects the pair.
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();
    let (code, stderr) = project_failure(
        &project.dir.path,
        &["mix-a", "mix-b"],
        &["--out", "record.json"],
        &path_env,
    );
    assert_ne!(code, Some(0));
    assert!(
        stderr.to_lowercase().contains("cannot be used with"),
        "--out and --project are mutually exclusive: {stderr}"
    );
}

#[test]
fn project_hash_mismatch_refuses_naming_the_asset() {
    // The manifest records each asset's sha256; a file whose bytes have drifted
    // from that figure refuses before the server binds, naming the asset.
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();
    // Rewrite one asset's bytes (still a decodable WAV, different content) so it no
    // longer hashes to what the manifest recorded.
    write_wav_fmt(
        &project.dir.path.join("out/a/vocals.wav"),
        44_100,
        44_100,
        2,
        16,
        false,
        999,
        1.0,
    );
    let (code, stderr) = project_failure(&project.dir.path, &["mix-a", "mix-b"], &[], &path_env);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("mix-a") && stderr.contains("does not match the manifest"),
        "a drifted asset is refused, named, before binding: {stderr}"
    );
}

#[test]
fn project_without_the_tool_installed_refuses_with_the_install_hint() {
    // Pre-flight: `uncompose-project` must be on PATH, else the session — which
    // cannot be registered — never starts. A PATH without the tool fails fast.
    let project = build_project();
    let (code, stderr) = project_failure(
        &project.dir.path,
        &["mix-a", "mix-b"],
        &[],
        "/nonexistent-path-with-no-tools",
    );
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("uncompose-project") && stderr.to_lowercase().contains("install"),
        "an absent project tool fails with the install hint: {stderr}"
    );
}

#[test]
fn project_missing_manifest_refuses() {
    let empty = TempDir::new("project-empty");
    let (_tool, path_env) = stub_project_tool();
    let (code, stderr) = project_failure(&empty.path, &["a", "b"], &[], &path_env);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("no project manifest") && stderr.contains("uncompose.project.json"),
        "an absent manifest is refused by name: {stderr}"
    );
}

#[test]
fn bare_file_source_adds_the_src_lane_without_project_fields() {
    // Bare-file mode takes an explicit `--source`; the SRC lane appears, and the
    // record's candidates carry no asset/project (there is no manifest).
    let dir = TempDir::new("bare-source");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    let src = dir.join("source.wav");
    write_wav(&a, 44_100, 44_100, 2, 11);
    write_wav(&b, 44_100, 44_100, 2, 12);
    write_wav(&src, 44_100, 44_100, 2, 13);
    let cwd = TempDir::new("bare-source-cwd");
    let mut command = Command::new(BIN);
    command
        .arg(&a)
        .arg(&b)
        .arg("--source")
        .arg(&src)
        .current_dir(&cwd.path);
    let server = serving_from(command, dir);

    let json = session_json(&server);
    let s: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert_eq!(
        s["source"]["label"], "SRC",
        "the SRC lane is present: {json}"
    );
    assert_eq!(s["source"]["name"], "source.wav");

    let body = r#"{"result": {"preference": null}, "observations": [], "loops": []}"#;
    let (status, _, _) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(status, 200);
    let record = validate_record_file(&sole_record(&cwd.path));
    for c in record["candidates"].as_array().unwrap() {
        assert!(
            c.get("asset").is_none() && c.get("project").is_none(),
            "a bare-file candidate carries no manifest fields: {record}"
        );
    }
}

#[test]
fn source_lane_joins_the_loudness_match_group() {
    // The SRC lane joins the match group (attenuate-to-quietest including SRC): a
    // source quieter than both candidates becomes the 0 dB reference, and A and B
    // are both attenuated down to it.
    let dir = TempDir::new("source-loudness");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    let src = dir.join("source.wav");
    write_wav_fmt(&a, 44_100, 44_100, 2, 16, false, 1, 1.0);
    write_wav_fmt(&b, 44_100, 44_100, 2, 16, false, 2, 1.0);
    write_wav_fmt(&src, 44_100, 44_100, 2, 16, false, 1, 0.25);
    let mut command = Command::new(BIN);
    command
        .arg(&a)
        .arg(&b)
        .arg("--source")
        .arg(&src)
        .arg("--loudness-match");
    let server = serving_with_cache(command, dir, None);

    let json = session_json(&server);
    let s: serde_json::Value = serde_json::from_str(&json).expect("session json");
    let lm = &s["loudness_match"];
    assert_eq!(lm["enabled"], true, "matching is on: {json}");
    let gain = |k: &str| lm["candidates"][k]["gain_db"].as_f64().unwrap();
    assert_eq!(
        gain("SRC"),
        0.0,
        "the quietest lane (SRC) is the reference: {json}"
    );
    assert!(
        gain("A") < 0.0,
        "A is attenuated down to the source: {json}"
    );
    assert!(
        gain("B") < 0.0,
        "B is attenuated down to the source: {json}"
    );
}

#[test]
fn blind_project_conceals_ab_but_keeps_src_identified() {
    // Blind + project: the A/B candidates are concealed (no name/path/hash), while
    // the SRC lane stays identified — concealment applies to A/B only (spec #42).
    let project = build_project();
    let (_tool, path_env) = stub_project_tool();
    let cwd = TempDir::new("blind-project-cwd");
    let server = launch_project(
        &project.dir.path,
        &cwd.path,
        &["vocals@deriv-a", "vocals@deriv-b"],
        &["--blind"],
        &path_env,
    );

    let json = session_json(&server);
    let s: serde_json::Value = serde_json::from_str(&json).expect("session json");
    assert_eq!(s["blind"], true, "the session is blind: {json}");

    // A/B carry only label + duration + opaque audio — no identity.
    for c in s["candidates"].as_array().unwrap() {
        assert!(
            c.get("name").is_none() && c.get("path").is_none() && c.get("sha256").is_none(),
            "a blind candidate conceals identity: {json}"
        );
    }
    // The SRC lane is fully identified: name and content-hash audio present.
    let source = &s["source"];
    assert_eq!(source["label"], "SRC");
    assert_eq!(
        source["name"], "take.wav",
        "the SRC lane names its file: {json}"
    );
    assert!(
        source["sha256"].is_string() && source["path"].is_string(),
        "the SRC lane keeps its identity in blind mode: {json}"
    );
}

// --- Issue #41 / spec #42 slice 5: evaluations handover & conclude lifecycle --

/// Read a logged argv (one arg per line) written by `logging_project_tool`.
fn logged_argv(argv_log: &Path) -> Vec<String> {
    std::fs::read_to_string(argv_log)
        .expect("the handover ran and logged its argv")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn project_conclude_lands_in_evaluations_and_registers_with_the_pinned_argv() {
    // The DoD: a project-mode conclude writes the record to
    // `<root>/evaluations/<ulid>.json`, then invokes the pinned argv
    // `uncompose-project import --project <abs-root> <abs-record>` (absolute
    // paths), and the registration outcome rides the conclude response. Success
    // exits 0.
    let project = build_project();
    let argv_log = TempDir::new("import-argv");
    let log = argv_log.join("argv");
    let (_tool, path_env) = logging_project_tool(&log, 0, "");
    let cwd = TempDir::new("project-handover-cwd");
    let mut server = launch_project(
        &project.dir.path,
        &cwd.path,
        &["vocals@deriv-a", "vocals@deriv-b"],
        &[],
        &path_env,
    );

    let body =
        r#"{"result": {"preference": "A", "confidence": 4}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    assert_eq!(
        status,
        200,
        "a project session concludes: {}",
        String::from_utf8_lossy(&resp)
    );

    // The record landed under `<root>/evaluations/`, not the invoking directory.
    let evaluations = project.dir.path.join("evaluations");
    let record_path = sole_record(&evaluations);
    let record = validate_record_file(&record_path);
    assert!(
        record["id"].as_str().unwrap().len() == 26,
        "the record is named by its ULID: {record}"
    );

    // The stub received exactly the pinned argv, both paths absolute.
    let argv = logged_argv(&log);
    assert_eq!(
        argv,
        vec![
            "import".to_string(),
            "--project".to_string(),
            project.dir.path.to_string_lossy().into_owned(),
            record_path.to_string_lossy().into_owned(),
        ],
        "the handover runs `import --project <root> <record>`: {argv:?}"
    );
    assert!(
        Path::new(&argv[2]).is_absolute() && Path::new(&argv[3]).is_absolute(),
        "both handover paths are absolute: {argv:?}"
    );

    // The registration outcome rides the conclude response.
    let response: serde_json::Value =
        serde_json::from_slice(&resp).expect("conclude response is json");
    assert_eq!(
        response["registration"]["registered"], true,
        "a clean import registers: {response}"
    );

    // Save-and-registered is a clean close: the process exits 0.
    let code = server.child.wait().expect("child exits").code();
    assert_eq!(code, Some(0), "a saved and registered session exits 0");
}

#[test]
fn project_import_failure_keeps_the_record_relays_stderr_and_exits_nonzero() {
    // Failure semantics (#67 res. 7): the record is never the casualty. On import
    // failure the record is kept, the tool's stderr is relayed, the process exits
    // nonzero, and the exact recovery command is printed last. The registration
    // outcome (with the recovery command) also rides the conclude response.
    let project = build_project();
    let argv_log = TempDir::new("import-argv-fail");
    let log = argv_log.join("argv");
    let (_tool, path_env) = logging_project_tool(&log, 3, "import blew up: manifest locked");
    let cwd = TempDir::new("project-handover-fail-cwd");

    // Capture the process's stderr so the relay + recovery line are observable.
    let mut command = Command::new(BIN);
    command
        .arg("--project")
        .arg(&project.dir.path)
        .args(["vocals@deriv-a", "vocals@deriv-b"])
        .env("PATH", &path_env)
        .current_dir(&cwd.path)
        .stderr(Stdio::piped());
    let mut server = serving_from(command, TempDir::new("project-handover-fail-hold"));
    let child_stderr = server.child.stderr.take().expect("piped stderr");

    let body =
        r#"{"result": {"preference": "B", "confidence": 2}, "observations": [], "loops": []}"#;
    let (status, _, resp) = http_post(&server, &format!("/record?token={}", server.token), body);
    // The conclude still succeeds — the record was written; only registration failed.
    assert_eq!(
        status,
        200,
        "the record is written even when import fails: {}",
        String::from_utf8_lossy(&resp)
    );

    // The record is kept under `<root>/evaluations/`.
    let record_path = sole_record(&project.dir.path.join("evaluations"));
    let _ = validate_record_file(&record_path);

    // The registration outcome on the response says it failed, relays the stderr,
    // and offers the exact recovery command.
    let response: serde_json::Value =
        serde_json::from_slice(&resp).expect("conclude response is json");
    let registration = &response["registration"];
    assert_eq!(
        registration["registered"], false,
        "a failed import is reported unregistered: {response}"
    );
    assert!(
        registration["error"]
            .as_str()
            .unwrap()
            .contains("import blew up"),
        "the import stderr is relayed on the response: {response}"
    );
    let recovery = format!("uncompose project import {}", record_path.display());
    assert_eq!(
        registration["recovery"], recovery,
        "the response offers the exact recovery command: {response}"
    );

    // The process exits nonzero, having relayed the stderr and printed the recovery
    // command last on its own stderr.
    let code = server.child.wait().expect("child exits").code();
    assert_eq!(code, Some(1), "a failed import exits nonzero");
    let mut printed = String::new();
    BufReader::new(child_stderr)
        .read_to_string(&mut printed)
        .expect("read child stderr");
    assert!(
        printed.contains("import blew up"),
        "the import stderr is relayed to the process stderr: {printed:?}"
    );
    assert!(
        printed.trim_end().ends_with(&recovery),
        "the recovery command is printed last: {printed:?}"
    );
}
