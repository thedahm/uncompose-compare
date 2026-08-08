//! M3 listening slice (issues #10, #11): the two-file compare command.
//!
//! `uncompose-compare <a> <b>` loads two audio files as candidates A and B (in
//! argument order), hashing and decoding each at startup, then serves the
//! embedded UI over the spike's guarded loopback server (#72) plus a
//! `/session` endpoint that reports the metadata the workbench needs: file
//! names, sha256, size, durations in samples and ms, sample rates, and a
//! duration-mismatch flag.
//!
//! Issue #11 adds the playback path (#74). At load each source is transcoded
//! into a lossless playback proxy — flacenc FLAC at the source sample rate and
//! 24-bit-max integer depth (16/24-bit sources pass through bit-exact; wider
//! integer and float sources quantize to 24), with a hound WAV fallback when
//! FLAC cannot represent the source (e.g. more than eight channels). Proxies
//! land in the XDG content-hash cache keyed by source sha256, so a second run
//! against unchanged files reuses them and re-transcodes nothing; the cache is
//! pruned oldest-accessed to a (configurable) 2 GiB cap at startup only, and
//! `uncompose-compare cache clear` wipes it. An `/audio/<sha256>` endpoint
//! serves each proxy, resolving strictly through the in-memory content-hash
//! table so a request never names a filesystem path.
//!
//! The #72 privacy contract from the spike still holds on every response —
//! loopback bind, ephemeral port, per-session token, Host check, blanket
//! `Cache-Control: no-store`, constant-time token compare — and covers the
//! `/session` and `/audio/<sha256>` endpoints too.
//!
//! Bad invocations fail before the server ever binds, with a clear message and
//! a non-zero exit: wrong argument count (clap), a missing or unreadable file,
//! or an undecodable format.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::fs::{File, FileTimes, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tiny_http::{Header, Method, Request, Response, Server};

/// The Vite/React stub bundle, embedded at compile time. `build.rs` guarantees
/// the folder is present and non-empty, so a build that reaches here has assets.
#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

/// The default proxy-cache cap: 2 GiB, per #72. Overridable with
/// `--cache-max-bytes` so the LRU prune is testable and tunable.
const DEFAULT_CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The v0 comparison-record JSON Schema, this repo's owned artifact (issue #16,
/// decision uncompose#65), embedded so the record endpoint validates against the
/// exact bytes committed here — no drift between the published schema and what
/// the server enforces, and no file read at conclude time.
const SCHEMA_STR: &str = include_str!("../schemas/compare/v0/uncompose.compare.schema.json");

/// The schema's `$id` — the value every written record carries in its `schema`
/// field (serving it at this URL is M6; owning it is M3).
const SCHEMA_ID: &str = "https://uncompose.org/schemas/compare/v0/uncompose.compare.schema.json";

/// uncompose-compare — load two audio files and open the listening workbench.
#[derive(Parser)]
#[command(name = "uncompose-compare", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Candidate A: the first audio file to compare.
    a: Option<PathBuf>,
    /// Candidate B: the second audio file to compare.
    b: Option<PathBuf>,

    /// Prune the proxy cache to at most this many bytes (LRU, at startup only).
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_CACHE_MAX_BYTES)]
    cache_max_bytes: u64,

    /// Write the concluded comparison record here instead of the default
    /// `<ULID>.json` in the invoking directory. An existing destination is
    /// refused, never overwritten.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Proxy-cache maintenance.
    Cache {
        #[command(subcommand)]
        action: CacheCommand,
    },
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Delete every cached playback proxy and report what was removed.
    Clear,
}

fn main() {
    let cli = Cli::parse();

    if let Err(err) = run(&cli) {
        eprintln!("uncompose-compare: {err}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cache = Cache::new(cli.cache_max_bytes)?;

    // `cache clear` is a maintenance path that never binds a server.
    if let Some(Command::Cache {
        action: CacheCommand::Clear,
    }) = &cli.command
    {
        let (files, bytes) = cache.clear()?;
        if files == 0 {
            println!("cache already empty ({})", cache.dir.display());
        } else {
            println!(
                "cleared {files} proxy file{} ({bytes} bytes) from {}",
                if files == 1 { "" } else { "s" },
                cache.dir.display(),
            );
        }
        return Ok(());
    }

    // The default form needs exactly two candidate paths. clap already rejects a
    // third positional as "unexpected"; a zero/one-file invocation lands here.
    let (a, b) = match (&cli.a, &cli.b) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Err("two audio files are required\n\n\
                 Usage: uncompose-compare <A> <B>"
                .into())
        }
    };

    // Load both candidates before binding: a bad invocation must fail with a
    // clear message and a non-zero exit, never a running server. Loading also
    // transcodes each input into a cached playback proxy (#74).
    let session = Session::load(a, b, &cache)?;

    // Prune the cache once, at startup, never mid-session (#72). The proxies
    // this run just wrote/reused carry the freshest access time, so an LRU prune
    // evicts stale entries from earlier sessions before it ever touches ours.
    cache.prune()?;

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

    // The record writer: the default destination is the invoking directory
    // (named by the record's ULID at conclude time), overridden by `--out`. The
    // session's start is the record's `created_at`; `completed_at` is stamped at
    // conclude. Built before serving so a bad `--out` or an unreadable cwd fails
    // fast rather than at the end of a session.
    let recorder = Recorder::new(cli.out.clone(), std::env::current_dir()?, SystemTime::now())?;

    let url = format!("http://127.0.0.1:{port}/?token={token}");
    println!("{url}");
    std::io::stdout().flush()?;

    for request in server.incoming_requests() {
        serve(request, &token, &session, &recorder);
    }

    Ok(())
}

/// A loaded comparison session: the two candidates in argument order, plus the
/// content-hash → proxy table the audio endpoints resolve through. Requests name
/// a source hash, never a filesystem path, so path traversal is impossible.
struct Session {
    candidates: [Candidate; 2],
    proxies: HashMap<String, Proxy>,
}

/// One loaded audio file, with everything the record and workbench need to
/// identify and describe it. Decoded metadata is derived at load; the PCM
/// itself is not retained — it lives in the cached playback proxy (#74), which
/// the workbench fetches from `/audio/<sha256>`.
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

/// A cached playback proxy for one source: the on-disk file the audio endpoint
/// streams, and the container it was encoded in (so the response's Content-Type
/// is honest).
struct Proxy {
    path: PathBuf,
    container: Container,
}

/// FLAC's format ceiling on channel count; a wider source falls back to WAV.
const MAX_FLAC_CHANNELS: usize = 8;

/// The lossless container a proxy was written in: FLAC by default, WAV only when
/// FLAC cannot represent the source (per #74).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    Flac,
    Wav,
}

impl Container {
    fn ext(self) -> &'static str {
        match self {
            Container::Flac => "flac",
            Container::Wav => "wav",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Container::Flac => "audio/flac",
            Container::Wav => "audio/wav",
        }
    }
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
             \"sample_rate\":{},\"channels\":{},\"audio\":{}}}",
            json_str(self.label),
            json_str(&self.name),
            json_str(&self.path),
            json_str(&self.sha256),
            self.size,
            self.frames,
            json_num(self.duration_ms()),
            self.sample_rate,
            self.channels,
            // The audio endpoint resolves purely by source hash (#72): the page
            // never asks for a path, only for the proxy of a content it knows.
            json_str(&format!("/audio/{}", self.sha256)),
        )
    }
}

impl Session {
    fn load(a: &Path, b: &Path, cache: &Cache) -> Result<Self, LoadError> {
        let mut proxies = HashMap::new();
        let ca = load_candidate("A", a, cache, &mut proxies)?;
        let cb = load_candidate("B", b, cache, &mut proxies)?;
        Ok(Session {
            candidates: [ca, cb],
            proxies,
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

/// Hash, size, decode, and transcode one input into a `Candidate` plus its
/// cached playback proxy. The whole file is read once to hash and size it;
/// decoding then reopens it (symphonia streams from a `File`) to read the PCM,
/// which is transcoded into the content-hash cache and registered in `proxies`.
fn load_candidate(
    label: &'static str,
    path: &Path,
    cache: &Cache,
    proxies: &mut HashMap<String, Proxy>,
) -> Result<Candidate, LoadError> {
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

    let decoded = decode_pcm(path).map_err(|reason| LoadError::Undecodable {
        path: display.clone(),
        reason,
    })?;

    // Transcode into (or reuse from) the content-hash cache, keyed by the source
    // hash. A second run against unchanged files finds the proxy already there.
    let proxy = cache
        .ensure_proxy(&sha256, &decoded)
        .map_err(|reason| LoadError::Undecodable {
            path: display.clone(),
            reason,
        })?;
    proxies.insert(sha256.clone(), proxy);

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
        frames: decoded.frames(),
        sample_rate: decoded.sample_rate,
        channels: decoded.channels,
    })
}

/// The proxy bit depth for a source of `src_bits`: 16- and 24-bit integer
/// sources pass through unchanged; anything wider (32-bit int, 32/64-bit float)
/// quantizes to 24 (#74). FLAC/WAV both top out at 24-bit here.
fn target_bits(src_bits: u32) -> u32 {
    src_bits.clamp(8, 24)
}

/// Fully-decoded source PCM plus the transcode policy derived from it: the
/// interleaved samples (right-shifted into `bits`-bit integer range), the source
/// sample rate (never resampled, #74), the channel count, and the target bit
/// depth (16/24 pass through bit-exact; 32-bit int and float quantize to 24).
struct DecodedPcm {
    /// Interleaved integer samples in `bits`-bit range, ready for the encoder.
    samples: Vec<i32>,
    sample_rate: u32,
    channels: u16,
    bits: u32,
}

impl DecodedPcm {
    fn frames(&self) -> u64 {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() as u64 / self.channels as u64
        }
    }
}

/// Decode `path` fully with symphonia into interleaved integer PCM. Decoding to
/// the end (rather than trusting a header frame count) is what makes an
/// undecodable or truncated file surface here as an error instead of a wrong
/// duration or a broken proxy later.
///
/// symphonia's `SampleBuffer<i32>` normalizes every source format to the full
/// i32 range (`i16 << 16`, `i24 << 8`, float scaled to fill i32, `i32` as-is),
/// so a single right shift by `32 - bits` recovers a `bits`-bit sample: exact
/// for 16- and 24-bit integer sources, a top-bits quantization for 32-bit int
/// and float. Sample rate is carried through untouched.
fn decode_pcm(path: &Path) -> Result<DecodedPcm, String> {
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
    if channels == 0 {
        return Err("no channels".to_string());
    }

    // Target bit depth: preserve 16/24-bit integer sources bit-exact; anything
    // wider (32-bit int, 32/64-bit float) quantizes to 24 (#74). An unstated
    // depth defaults to 24, the widest we ever emit.
    let bits = target_bits(codec_params.bits_per_sample.unwrap_or(24));
    let shift = 32 - bits;

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;

    let mut samples: Vec<i32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<i32>> = None;
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
        let decoded = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // A recoverable decode hiccup skips the packet; a fatal one fails.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let buf = sample_buf.get_or_insert_with(|| {
            SampleBuffer::<i32>::new(decoded.capacity() as u64, *decoded.spec())
        });
        buf.copy_interleaved_ref(decoded);
        samples.extend(buf.samples().iter().map(|&s| s >> shift));
    }

    Ok(DecodedPcm {
        samples,
        sample_rate,
        channels,
        bits,
    })
}

/// The XDG content-hash proxy cache. Proxies are named `<source-sha256>.<ext>`,
/// so a source's proxy is found by hash alone — no request path ever reaches the
/// filesystem. Pruned LRU (by access time) to `max_bytes`, at startup only.
struct Cache {
    dir: PathBuf,
    max_bytes: u64,
}

impl Cache {
    /// Locate the cache under `$XDG_CACHE_HOME/uncompose-compare` (falling back
    /// to `$HOME/.cache/...`, Linux-only per the v0.1 scope) and ensure it
    /// exists.
    fn new(max_bytes: u64) -> Result<Cache, Box<dyn std::error::Error + Send + Sync>> {
        let base = match std::env::var_os("XDG_CACHE_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => {
                let home = std::env::var_os("HOME").ok_or("neither XDG_CACHE_HOME nor HOME set")?;
                PathBuf::from(home).join(".cache")
            }
        };
        let dir = base.join("uncompose-compare");
        std::fs::create_dir_all(&dir)?;
        Ok(Cache { dir, max_bytes })
    }

    /// Return the proxy for `hash`, transcoding it into the cache if absent.
    /// A cache hit rewrites nothing — it only bumps the file's access time so the
    /// LRU prune keeps files this session used — so re-running on unchanged
    /// inputs performs no re-transcode.
    fn ensure_proxy(&self, hash: &str, pcm: &DecodedPcm) -> Result<Proxy, String> {
        // The container choice is a pure function of the source, so it is stable
        // across runs: the same source always maps to the same cache filename.
        let container = if pcm.channels as usize > MAX_FLAC_CHANNELS {
            Container::Wav
        } else {
            Container::Flac
        };
        let path = self.dir.join(format!("{hash}.{}", container.ext()));

        if path.exists() {
            // Mark the reuse without touching the file's contents or mtime: only
            // the access time moves, which is what the LRU prune orders by.
            let _ = File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_times(FileTimes::new().set_accessed(SystemTime::now())));
            return Ok(Proxy { path, container });
        }

        let encoded = match container {
            Container::Flac => encode_flac(pcm)?,
            Container::Wav => encode_wav(pcm)?,
        };

        // Write to a private temp sibling then rename, so a crash mid-encode
        // never leaves a half-written proxy the next run would trust. The temp
        // name is unique per process (the final name is content-addressed, so
        // two runs transcoding the same source race only to replace it with
        // identical bytes).
        let tmp = self.dir.join(format!(
            "{hash}.{}.{}.tmp",
            container.ext(),
            std::process::id()
        ));
        std::fs::write(&tmp, &encoded).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(Proxy { path, container })
    }

    /// Prune the cache to `max_bytes`, evicting least-recently-accessed proxies
    /// first. Called once at startup; never mid-session (#72).
    fn prune(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        let mut total: u64 = 0;
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let meta = match entry.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let atime = meta.accessed().unwrap_or(SystemTime::UNIX_EPOCH);
            total += meta.len();
            entries.push((entry.path(), meta.len(), atime));
        }

        if total <= self.max_bytes {
            return Ok(());
        }

        // Oldest access first: those are the entries the prune sheds.
        entries.sort_by_key(|(_, _, atime)| *atime);
        for (path, len, _) in entries {
            if total <= self.max_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
        Ok(())
    }

    /// Delete every proxy in the cache, returning (file count, byte total)
    /// removed so `cache clear` can report what it did.
    fn clear(&self) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
        let mut files = 0u64;
        let mut bytes = 0u64;
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let meta = match entry.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let len = meta.len();
            if std::fs::remove_file(entry.path()).is_ok() {
                files += 1;
                bytes += len;
            }
        }
        Ok((files, bytes))
    }
}

/// Encode interleaved integer PCM as FLAC at the source sample rate and target
/// bit depth (flacenc, #74).
fn encode_flac(pcm: &DecodedPcm) -> Result<Vec<u8>, String> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| format!("flac config: {e}"))?;
    let source = flacenc::source::MemSource::from_samples(
        &pcm.samples,
        pcm.channels as usize,
        pcm.bits as usize,
        pcm.sample_rate as usize,
    );
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| format!("flac encode: {e}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("flac serialize: {e}"))?;
    Ok(sink.as_slice().to_vec())
}

/// Encode interleaved integer PCM as WAV (hound) — the fallback for sources FLAC
/// cannot represent, at the source sample rate and target bit depth (#74).
fn encode_wav(pcm: &DecodedPcm) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: pcm.channels,
        sample_rate: pcm.sample_rate,
        bits_per_sample: pcm.bits as u16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut buf, spec).map_err(|e| format!("wav writer: {e}"))?;
        for &s in &pcm.samples {
            writer
                .write_sample(s)
                .map_err(|e| format!("wav sample: {e}"))?;
        }
        writer
            .finalize()
            .map_err(|e| format!("wav finalize: {e}"))?;
    }
    Ok(buf.into_inner())
}

/// The comparison-record writer (issue #16): the compiled schema validator, the
/// destination policy, the session's start time, and the write-once guard. The
/// server loop is single-threaded, so a `Cell` is all "written exactly once"
/// needs — a successful write flips it, and every later conclude is refused.
struct Recorder {
    validator: jsonschema::Validator,
    /// The `--out` override, if any; otherwise the record lands in `dir`.
    out: Option<PathBuf>,
    /// The invoking directory: the default destination and the base a relative
    /// `--out` resolves against.
    dir: PathBuf,
    /// The session's `created_at`, stamped once at startup (RFC 3339, UTC).
    created_at: String,
    concluded: Cell<bool>,
}

/// Why a conclude was refused, mapped to an HTTP status the UI can act on.
enum RecordError {
    /// A second conclude, or any conclude after a successful write (#65).
    AlreadyConcluded,
    /// The POST body was not the JSON the endpoint expects.
    BadRequest(String),
    /// The assembled record failed schema validation.
    Invalid(String),
    /// The destination already exists — refused, never overwritten.
    Overwrite(String),
    /// The record could not be written for some other reason.
    Io(String),
}

impl RecordError {
    fn status(&self) -> u16 {
        match self {
            RecordError::BadRequest(_) | RecordError::Invalid(_) => 400,
            RecordError::AlreadyConcluded | RecordError::Overwrite(_) => 409,
            RecordError::Io(_) => 500,
        }
    }
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordError::AlreadyConcluded => {
                write!(f, "this session has already been concluded")
            }
            RecordError::BadRequest(m) => write!(f, "{m}"),
            RecordError::Invalid(m) => write!(f, "record does not conform to the schema: {m}"),
            RecordError::Overwrite(m) => write!(f, "{m}"),
            RecordError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl Recorder {
    fn new(
        out: Option<PathBuf>,
        dir: PathBuf,
        created_at: SystemTime,
    ) -> Result<Recorder, Box<dyn std::error::Error + Send + Sync>> {
        let schema: Value = serde_json::from_str(SCHEMA_STR)?;
        let validator = jsonschema::validator_for(&schema).map_err(|e| e.to_string())?;
        Ok(Recorder {
            validator,
            out,
            dir,
            created_at: rfc3339(created_at),
            concluded: Cell::new(false),
        })
    }

    /// Assemble the record from the server-authoritative fields plus the
    /// session-authored body, validate it, and write it once. The server owns
    /// `schema`/`id`/timestamps/`candidates`/`mode`/`playback` (so hashes and
    /// paths can't be forged from the browser); the body supplies only `result`,
    /// `observations`, `loops`, and `context`.
    fn conclude(&self, session: &Session, body: &str) -> Result<String, RecordError> {
        // Refuse a second conclude before doing any work.
        if self.concluded.get() {
            return Err(RecordError::AlreadyConcluded);
        }

        let posted: Value = serde_json::from_str(body)
            .map_err(|e| RecordError::BadRequest(format!("record is not valid JSON: {e}")))?;

        // Candidates come from the trusted load, never the request: this is what
        // makes the record's hashes provably match the inputs.
        let candidates: Vec<Value> = session
            .candidates
            .iter()
            .map(|c| {
                json!({
                    "label": c.label,
                    "path": c.path,
                    "sha256": c.sha256,
                    "size": c.size,
                })
            })
            .collect();

        let id = new_ulid().map_err(|e| RecordError::Io(e.to_string()))?;

        let mut record = json!({
            "schema": SCHEMA_ID,
            "id": id,
            "created_at": self.created_at,
            "completed_at": rfc3339(SystemTime::now()),
            "candidates": candidates,
            "mode": "ab",
            "playback": {},
            "loops": posted.get("loops").cloned().unwrap_or_else(|| json!([])),
            "observations": posted.get("observations").cloned().unwrap_or_else(|| json!([])),
            "result": posted.get("result").cloned().unwrap_or(Value::Null),
        });
        // Context is optional; carry it through only when the listener wrote one.
        if let Some(context) = posted.get("context") {
            record["context"] = context.clone();
        }

        // Validate the assembled record against the owned schema, surfacing the
        // first few errors so a refused conclude says why.
        if let Err(msg) = validate(&self.validator, &record) {
            return Err(RecordError::Invalid(msg));
        }

        let dest = self.destination(&id);
        // `create_new` refuses an existing destination atomically — the record is
        // written exactly once and never overwrites what is already there.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    RecordError::Overwrite(format!(
                        "refusing to overwrite an existing record at {}",
                        dest.display()
                    ))
                } else {
                    RecordError::Io(format!("cannot write {}: {e}", dest.display()))
                }
            })?;
        let serialized =
            serde_json::to_string_pretty(&record).map_err(|e| RecordError::Io(e.to_string()))?;
        file.write_all(serialized.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .map_err(|e| RecordError::Io(format!("cannot write {}: {e}", dest.display())))?;

        // Only a completed write concludes the session; that write is the one.
        self.concluded.set(true);
        Ok(dest.display().to_string())
    }

    /// The record destination: `--out` when set (resolved against the invoking
    /// directory if relative), else `<ULID>.json` in the invoking directory.
    fn destination(&self, id: &str) -> PathBuf {
        match &self.out {
            // `join` keeps an absolute `--out` as-is and resolves a relative
            // one against the invoking directory.
            Some(out) => self.dir.join(out),
            None => self.dir.join(format!("{id}.json")),
        }
    }
}

/// Validate `instance` against the compiled schema, joining the first few error
/// messages (a fully-wrong record can produce many) into one line.
fn validate(validator: &jsonschema::Validator, instance: &Value) -> Result<(), String> {
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|e| e.to_string())
        .take(5)
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// A JSON response for the record endpoint, uncached like every other response.
fn record_response(status: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
}

/// Mint a ULID: 48 bits of wall-clock milliseconds (most-significant, so ids
/// sort by time) followed by 80 bits of OS randomness, Crockford base32 in 26
/// chars. Enough to give each record a unique, sortable, machine-minted id (the
/// human slug lives on the manifest side, #63) without a crate.
fn new_ulid() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mut rand = [0u8; 10];
    File::open("/dev/urandom")?.read_exact(&mut rand)?;
    let mut rnd: u128 = 0;
    for &b in &rand {
        rnd = (rnd << 8) | b as u128;
    }
    let value = ((ms & 0xFFFF_FFFF_FFFF) << 80) | rnd;
    Ok(crockford32(value))
}

/// Encode a u128 as 26 Crockford-base32 characters (5 bits each, most
/// significant first). The top character carries only the value's high 3 bits.
fn crockford32(mut value: u128) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut buf = [0u8; 26];
    for slot in buf.iter_mut().rev() {
        *slot = ALPHABET[(value & 0x1f) as usize];
        value >>= 5;
    }
    String::from_utf8(buf.to_vec()).expect("crockford alphabet is ascii")
}

/// Format a `SystemTime` as an RFC 3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) —
/// the `date-time` the schema wants for `created_at`/`completed_at`, without a
/// calendar crate.
fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Days-since-Unix-epoch → (year, month, day), Howard Hinnant's `civil_from_days`
/// (proleptic Gregorian, valid across the range any real timestamp lands in).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d)
}

/// Resolve the request against the session endpoint and the embedded bundle,
/// enforcing the #72 contract (Host check, then token) before serving anything.
fn serve(mut request: Request, token: &str, session: &Session, recorder: &Recorder) {
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

    // The record endpoint: an explicit conclude POSTs the concluded session
    // here. The server validates it against the owned schema, stamps completion,
    // and writes the immutable record exactly once (issue #16). A second conclude
    // — or any conclude after a successful write — is refused.
    if path == "record" && *request.method() == Method::Post {
        let mut body = String::new();
        if request.as_reader().read_to_string(&mut body).is_err() {
            let _ = request.respond(record_response(
                400,
                &json!({"error": "unreadable request body"}),
            ));
            return;
        }
        let (status, payload) = match recorder.conclude(session, &body) {
            Ok(path) => (200, json!({ "path": path })),
            Err(e) => (e.status(), json!({ "error": e.to_string() })),
        };
        let _ = request.respond(record_response(status, &payload));
        return;
    }

    // Audio proxy endpoint: resolve strictly through the content-hash table, so a
    // request names a source hash we already loaded — never a filesystem path.
    // An unknown hash is a 404, not a chance to read arbitrary files.
    if let Some(hash) = path.strip_prefix("audio/") {
        let response = match session.proxies.get(hash) {
            Some(proxy) => match std::fs::read(&proxy.path) {
                Ok(data) => Response::from_data(data)
                    .with_header(header("Content-Type", proxy.container.content_type()))
                    .with_header(header("Cache-Control", "no-store")),
                // A proxy pruned out from under us resolves like an unknown hash.
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

/// A 404 for an unknown asset or content hash — like every other response,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_bits_preserves_16_and_24_but_caps_wider() {
        assert_eq!(target_bits(16), 16, "16-bit passes through");
        assert_eq!(target_bits(24), 24, "24-bit passes through");
        assert_eq!(target_bits(32), 24, "32-bit int quantizes to 24");
        assert_eq!(target_bits(64), 24, "64-bit float quantizes to 24");
        assert_eq!(target_bits(8), 8, "8-bit stays 8");
    }

    /// symphonia normalizes every source to the full i32 range; the decode loop
    /// then recovers the target-bit sample with `>> (32 - bits)`. Prove that
    /// recovers 16- and 24-bit sources bit-exact and takes the top 24 bits of a
    /// 32-bit source.
    #[test]
    fn shift_recovers_source_bits() {
        // A 16-bit sample lands in i32 as `orig << 16`; >> 16 recovers it.
        let orig16: i32 = -12_345;
        assert_eq!((orig16 << 16) >> (32 - target_bits(16)), orig16);

        // A 24-bit sample lands as `orig << 8`; >> 8 recovers it.
        let orig24: i32 = 3_000_000; // within +/- 2^23
        assert_eq!((orig24 << 8) >> (32 - target_bits(24)), orig24);

        // A full-range i32 (32-bit source) keeps its top 24 bits.
        let full: i32 = 0x7FAB_CDEF;
        assert_eq!(full >> (32 - target_bits(32)), full >> 8);
    }

    #[test]
    fn container_metadata_is_honest() {
        assert_eq!(Container::Flac.ext(), "flac");
        assert_eq!(Container::Flac.content_type(), "audio/flac");
        assert_eq!(Container::Wav.ext(), "wav");
        assert_eq!(Container::Wav.content_type(), "audio/wav");
    }
}
