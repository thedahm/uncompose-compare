//! The loaded comparison session: the two candidates in argument order, plus
//! the content-hash → proxy table the audio endpoint resolves through (#10,
//! #11).
//!
//! Requests name a source hash, never a filesystem path, so path traversal is
//! impossible. Bad invocations fail here — before the server ever binds — with a
//! clear message and a non-zero exit: a missing or unreadable file, or an
//! undecodable format.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::audio::decode_pcm;
use crate::cache::{Cache, Proxy};
use crate::hex;

/// One loaded audio file, with everything the record and workbench need to
/// identify and describe it. Decoded metadata is derived at load; the PCM
/// itself is not retained — it lives in the cached playback proxy (#74), which
/// the workbench fetches from `/audio/<sha256>`.
pub struct Candidate {
    pub label: &'static str,
    pub name: String,
    pub path: String,
    pub sha256: String,
    pub size: u64,
    /// Total decoded frames (samples per channel) — the candidate's duration.
    pub frames: u64,
    pub sample_rate: u32,
    pub channels: u16,
}

impl Candidate {
    pub fn duration_ms(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames as f64 * 1000.0 / self.sample_rate as f64
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "label": self.label,
            "name": self.name,
            "path": self.path,
            "sha256": self.sha256,
            "size": self.size,
            "frames": self.frames,
            "duration_ms": finite(self.duration_ms()),
            "sample_rate": self.sample_rate,
            "channels": self.channels,
            // The audio endpoint resolves purely by source hash (#72): the page
            // never asks for a path, only for the proxy of a content it knows.
            "audio": format!("/audio/{}", self.sha256),
        })
    }
}

/// A non-finite float would serialize as JSON `null`; degrade it to 0 so the
/// page always gets a number where it expects one.
fn finite(n: f64) -> f64 {
    if n.is_finite() {
        n
    } else {
        0.0
    }
}

pub struct Session {
    pub candidates: [Candidate; 2],
    pub proxies: HashMap<String, Proxy>,
}

impl Session {
    pub fn load(a: &Path, b: &Path, cache: &Cache) -> Result<Self, LoadError> {
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
    pub fn duration_mismatch(&self) -> bool {
        let [a, b] = &self.candidates;
        a.frames != b.frames || (a.duration_ms() - b.duration_ms()).abs() > 0.5
    }

    /// The session metadata as JSON for the `/session` endpoint.
    pub fn to_json(&self) -> Value {
        let [a, b] = &self.candidates;
        json!({
            "candidates": [a.to_json(), b.to_json()],
            "duration_mismatch": self.duration_mismatch(),
            "duration_delta_samples": (a.frames as i64 - b.frames as i64).abs(),
            "duration_delta_ms": finite((a.duration_ms() - b.duration_ms()).abs()),
        })
    }

    /// The labels this session actually loaded — the only candidate references a
    /// record may name.
    pub fn labels(&self) -> [&'static str; 2] {
        [self.candidates[0].label, self.candidates[1].label]
    }
}

/// A load failure, phrased so the message alone diagnoses it (a #28 concern):
/// which file, and whether it could not be read or could not be decoded.
#[derive(Debug)]
pub enum LoadError {
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
