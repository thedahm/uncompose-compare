//! M3 listening slice, first cut (issue #10): the two-file compare command.
//!
//! `uncompose-compare <a> <b>` loads two audio files as candidates A and B (in
//! argument order), hashing and decoding each at startup, then serves the
//! embedded UI over the spike's guarded loopback server (#72) plus a new
//! `/session` endpoint that reports the metadata the workbench needs: file
//! names, sha256, size, durations in samples and ms, sample rates, and a
//! duration-mismatch flag.
//!
//! The #72 privacy contract from the spike still holds on every response —
//! loopback bind, ephemeral port, per-session token, Host check, blanket
//! `Cache-Control: no-store`, constant-time token compare — and now covers the
//! `/session` endpoint too.
//!
//! Bad invocations fail before the server ever binds, with a clear message and
//! a non-zero exit: wrong argument count (clap), a missing or unreadable file,
//! or an undecodable format.

use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tiny_http::{Header, Request, Response, Server};

/// The Vite/React stub bundle, embedded at compile time. `build.rs` guarantees
/// the folder is present and non-empty, so a build that reaches here has assets.
#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

/// uncompose-compare — load two audio files and open the listening workbench.
#[derive(Parser)]
#[command(name = "uncompose-compare", version, about, long_about = None)]
struct Cli {
    /// Candidate A: the first audio file to compare.
    a: PathBuf,
    /// Candidate B: the second audio file to compare.
    b: PathBuf,
}

fn main() {
    let cli = Cli::parse();

    if let Err(err) = run(&cli) {
        eprintln!("uncompose-compare: {err}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Load both candidates before binding: a bad invocation must fail with a
    // clear message and a non-zero exit, never a running server.
    let session = Session::load(&cli.a, &cli.b)?;

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
        serve(request, &token, &session);
    }

    Ok(())
}

/// A loaded comparison session: the two candidates in argument order, ready to
/// answer the `/session` endpoint.
struct Session {
    candidates: [Candidate; 2],
}

/// One loaded audio file, with everything the record and workbench need to
/// identify and describe it. Decoded metadata is derived at load; the PCM
/// itself is not retained (playback proxies are a later issue).
struct Candidate {
    label: &'static str,
    name: String,
    path: String,
    sha256: String,
    size: u64,
    /// Total decoded frames (samples per channel) — the candidate's duration.
    frames: u64,
    sample_rate: u32,
    channels: u16,
}

impl Candidate {
    fn duration_ms(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames as f64 * 1000.0 / self.sample_rate as f64
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"label\":{},\"name\":{},\"path\":{},\"sha256\":{},\
             \"size\":{},\"frames\":{},\"duration_ms\":{},\
             \"sample_rate\":{},\"channels\":{}}}",
            json_str(self.label),
            json_str(&self.name),
            json_str(&self.path),
            json_str(&self.sha256),
            self.size,
            self.frames,
            json_num(self.duration_ms()),
            self.sample_rate,
            self.channels,
        )
    }
}

impl Session {
    fn load(a: &Path, b: &Path) -> Result<Self, LoadError> {
        Ok(Session {
            candidates: [load_candidate("A", a)?, load_candidate("B", b)?],
        })
    }

    /// True when the two candidates differ in duration. Reported alongside the
    /// deltas so the page can warn without blocking playback of either file.
    fn duration_mismatch(&self) -> bool {
        let [a, b] = &self.candidates;
        a.frames != b.frames || (a.duration_ms() - b.duration_ms()).abs() > 0.5
    }

    /// Serialize the session metadata as JSON for the `/session` endpoint.
    fn to_json(&self) -> String {
        let [a, b] = &self.candidates;
        let delta_samples = (a.frames as i64 - b.frames as i64).abs();
        let delta_ms = (a.duration_ms() - b.duration_ms()).abs();
        format!(
            "{{\"candidates\":[{},{}],\
             \"duration_mismatch\":{},\
             \"duration_delta_samples\":{},\
             \"duration_delta_ms\":{}}}",
            a.to_json(),
            b.to_json(),
            self.duration_mismatch(),
            delta_samples,
            json_num(delta_ms),
        )
    }
}

/// A load failure, phrased so the message alone diagnoses it (a #28 concern):
/// which file, and whether it could not be read or could not be decoded.
#[derive(Debug)]
enum LoadError {
    Unreadable {
        path: String,
        source: std::io::Error,
    },
    Undecodable {
        path: String,
        reason: String,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Unreadable { path, source } => {
                write!(f, "cannot read {path}: {source}")
            }
            LoadError::Undecodable { path, reason } => {
                write!(f, "cannot decode {path}: {reason}")
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Hash, size, and decode one input into a `Candidate`. The whole file is read
/// once to hash and size it; decoding then reopens it (symphonia streams from a
/// `File`) to count frames and read the sample rate.
fn load_candidate(label: &'static str, path: &Path) -> Result<Candidate, LoadError> {
    let display = path.display().to_string();

    let mut file = File::open(path).map_err(|source| LoadError::Unreadable {
        path: display.clone(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| LoadError::Unreadable {
            path: display.clone(),
            source,
        })?;

    let size = bytes.len() as u64;
    let sha256 = hex(&Sha256::digest(&bytes));

    let decoded = decode_metadata(path).map_err(|reason| LoadError::Undecodable {
        path: display.clone(),
        reason,
    })?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| display.clone());

    Ok(Candidate {
        label,
        name,
        path: display,
        sha256,
        size,
        frames: decoded.frames,
        sample_rate: decoded.sample_rate,
        channels: decoded.channels,
    })
}

/// The decoded shape we keep: how long, how fast, how wide.
struct DecodedMeta {
    frames: u64,
    sample_rate: u32,
    channels: u16,
}

/// Decode `path` fully with symphonia to count frames and read the format's
/// sample rate and channel count. Decoding to the end (rather than trusting a
/// header frame count) is what makes an undecodable or truncated file surface
/// here as an error instead of a wrong duration later.
fn decode_metadata(path: &Path) -> Result<DecodedMeta, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| e.to_string())?;

    let mut format = probed.format;
    // Only the track id and codec params outlive this borrow of `format`;
    // `next_packet` below needs `format` mutable again.
    let track = format
        .default_track()
        .ok_or_else(|| "no audio track".to_string())?;
    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| "unknown sample rate".to_string())?;
    let channels = codec_params.channels.map(|c| c.count() as u16).unwrap_or(0);

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;

    let mut frames: u64 = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // A clean end of stream is the loop's exit, not a failure.
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => frames += buf.frames() as u64,
            // A recoverable decode hiccup skips the packet; a fatal one fails.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }

    Ok(DecodedMeta {
        frames,
        sample_rate,
        channels,
    })
}

/// Resolve the request against the session endpoint and the embedded bundle,
/// enforcing the #72 contract (Host check, then token) before serving anything.
fn serve(request: Request, token: &str, session: &Session) {
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
        let response = Response::from_string(session.to_json())
            .with_header(header("Content-Type", "application/json"))
            .with_header(header("Cache-Control", "no-store"));
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

/// A per-session token: 16 bytes of OS randomness, hex-encoded. The spike is
/// Linux-only (spec #1), so `/dev/urandom` is a fine, dependency-free source.
fn session_token() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}

/// Lowercase hex-encode a byte slice.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// JSON-encode a string as a quoted, escaped literal. File names and paths can
/// contain quotes, backslashes, and control characters, so the escaping is not
/// optional.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON-encode a finite float; a non-finite value degrades to `0` rather than
/// emitting invalid JSON (`NaN`/`Infinity`).
fn json_num(n: f64) -> String {
    if n.is_finite() {
        format!("{n}")
    } else {
        "0".to_string()
    }
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
