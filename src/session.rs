//! The loaded comparison session: the two candidates in argument order, plus
//! the audio-reference → proxy table the audio endpoint resolves through (#10,
//! #11).
//!
//! Requests name a per-session audio reference — the source hash in a sighted
//! session, an opaque token in a blind one (#28) — never a filesystem path, so
//! path traversal is impossible. Bad invocations fail here — before the server
//! ever binds — with a clear message and a non-zero exit: a missing or
//! unreadable file, an undecodable format, or a pair blind mode refuses.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::audio::{decode_pcm, integrated_lufs};
use crate::cache::{Cache, Proxy};
use crate::hex;

/// One loaded audio file, with everything the record and workbench need to
/// identify and describe it. Decoded metadata is derived at load; the PCM
/// itself is not retained — it lives in the cached playback proxy (#74), which
/// the workbench fetches from `/audio/<audio_ref>`.
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
    /// The key the audio endpoint serves this candidate's proxy under. In a
    /// sighted session it is the source `sha256` (the page knows the content it
    /// asks for); in a blind session it is a per-session opaque reference, so a
    /// listener cannot decode the shuffle by hashing their own inputs (#28).
    pub audio_ref: String,
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
            // The audio endpoint resolves purely by the per-session reference
            // (#72): the page never asks for a path, only for the proxy of a
            // content it knows — in sighted mode, the source hash.
            "audio": self.audio_url(),
        })
    }

    /// The concealed view of a candidate for a blind session (#28): only the
    /// label, the shared duration, and an opaque audio reference. Name, path,
    /// sha256, size, and per-candidate technical metadata are omitted — the
    /// browser never receives identity it must not show.
    fn to_blind_json(&self) -> Value {
        json!({
            "label": self.label,
            "duration_ms": finite(self.duration_ms()),
            "audio": self.audio_url(),
        })
    }

    fn audio_url(&self) -> String {
        format!("/audio/{}", self.audio_ref)
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

/// One lane's loudness match figures (issue #30, decision uncompose#66): the
/// candidate's measured BS.1770 integrated loudness and the gain-only
/// attenuation applied to it. `gain_db` is always ≤ 0 (never boost); the
/// quietest lane gets exactly 0.0.
#[derive(Clone, Copy)]
pub struct Loudness {
    pub measured_lufs: f64,
    pub gain_db: f64,
}

pub struct Session {
    pub candidates: [Candidate; 2],
    pub proxies: HashMap<String, Proxy>,
    /// Per-lane loudness figures when `--loudness-match` is on (aligned with
    /// `candidates`), else `None` — playback is then faithful as-is (#66).
    pub loudness: Option<[Loudness; 2]>,
    /// A blind session (`--blind`): the label↔file assignment was shuffled at
    /// load and the served surface conceals every identifying detail (#28). The
    /// record still carries full identities — concealment is a session concern,
    /// not a storage one (uncompose#65).
    pub blind: bool,
}

impl Session {
    pub fn load(
        a: &Path,
        b: &Path,
        cache: &Cache,
        loudness_match: bool,
        blind: bool,
    ) -> Result<Self, LoadError> {
        let mut proxies = HashMap::new();
        // Blind mode shuffles which file the listener sees as A vs B by an
        // OS-randomness coin flip, so argument order tells them nothing (#28).
        let swap = blind && coin_flip()?;
        let (path_a, path_b) = if swap { (b, a) } else { (a, b) };
        let (ca, lufs_a) = load_candidate("A", path_a, cache, &mut proxies, loudness_match, blind)?;
        let (cb, lufs_b) = load_candidate("B", path_b, cache, &mut proxies, loudness_match, blind)?;
        let loudness = match (lufs_a, lufs_b) {
            (Some(la), Some(lb)) => Some(match_gains([la, lb])),
            _ => None,
        };
        let session = Session {
            candidates: [ca, cb],
            proxies,
            loudness,
            blind,
        };
        // Blind mode refuses, pre-bind, any pair it cannot honestly conceal:
        // identical content, or a difference the interface would have to display
        // or imply (#28). Sighted mode loads all of these with a warning instead.
        if blind {
            session.refuse_if_unblindable()?;
        }
        Ok(session)
    }

    /// The record's `mode` for this session (uncompose#65): `ab` sighted,
    /// `ab-blind-randomized` when the labels were shuffled (#28). `ab-blind`
    /// (concealed but unshuffled) stays reserved and is never written in v0.1.
    pub fn mode(&self) -> &'static str {
        if self.blind {
            "ab-blind-randomized"
        } else {
            "ab"
        }
    }

    /// Refuse a blind pair the interface cannot conceal without pretense (#28):
    /// identical content (nothing to blind-compare), or any duration, sample-
    /// rate, or channel-count mismatch (the interface would have to display or
    /// imply a difference that identifies the candidates). The message names the
    /// property and both values, in the established load-error voice.
    fn refuse_if_unblindable(&self) -> Result<(), LoadError> {
        let [a, b] = &self.candidates;
        if a.sha256 == b.sha256 {
            return Err(LoadError::BlindRefused(format!(
                "cannot blind-compare {} and {}: identical audio content (sha256 {})",
                a.path, b.path, a.sha256
            )));
        }
        // Sample rate and channel count are checked before duration: a rate
        // difference drags a duration mismatch along with it, so the more
        // specific property is named first.
        if self.sample_rate_mismatch() {
            return Err(LoadError::BlindRefused(format!(
                "cannot blind-compare {} and {}: sample-rate mismatch ({} Hz vs {} Hz)",
                a.path, b.path, a.sample_rate, b.sample_rate
            )));
        }
        if self.channel_count_mismatch() {
            return Err(LoadError::BlindRefused(format!(
                "cannot blind-compare {} and {}: channel-count mismatch ({} vs {})",
                a.path, b.path, a.channels, b.channels
            )));
        }
        if self.duration_mismatch() {
            return Err(LoadError::BlindRefused(format!(
                "cannot blind-compare {} and {}: duration mismatch ({:.0} ms vs {:.0} ms)",
                a.path,
                b.path,
                a.duration_ms(),
                b.duration_ms()
            )));
        }
        Ok(())
    }

    /// True when the two candidates differ in duration. Reported alongside the
    /// deltas so the page can warn without blocking playback of either file.
    pub fn duration_mismatch(&self) -> bool {
        let [a, b] = &self.candidates;
        a.frames != b.frames || (a.duration_ms() - b.duration_ms()).abs() > 0.5
    }

    /// True when the two candidates differ in sample rate. Like the duration
    /// mismatch, this warns the sighted listener without blocking playback — the
    /// files still play back faithfully at their own rates (#26 story 11). The
    /// two rates themselves are already in the candidate metadata.
    pub fn sample_rate_mismatch(&self) -> bool {
        let [a, b] = &self.candidates;
        a.sample_rate != b.sample_rate
    }

    /// True when the two candidates differ in channel count — same warning
    /// contract as the sample-rate mismatch (#26 story 11).
    pub fn channel_count_mismatch(&self) -> bool {
        let [a, b] = &self.candidates;
        a.channels != b.channels
    }

    /// The session metadata as JSON for the `/session` endpoint. The
    /// `loudness_match` object mirrors the record's `playback.loudness_match`
    /// shape (uncompose#66) so the workbench reads the per-lane gains it applies
    /// from the same shape the record will carry.
    pub fn to_json(&self) -> Value {
        if self.blind {
            return self.to_blind_json();
        }
        let [a, b] = &self.candidates;
        json!({
            "candidates": [a.to_json(), b.to_json()],
            "duration_mismatch": self.duration_mismatch(),
            "duration_delta_samples": (a.frames as i64 - b.frames as i64).abs(),
            "duration_delta_ms": finite((a.duration_ms() - b.duration_ms()).abs()),
            "loudness_match": self.loudness_match_json(),
            "sample_rate_mismatch": self.sample_rate_mismatch(),
            "channel_count_mismatch": self.channel_count_mismatch(),
        })
    }

    /// The concealed session payload for a blind session (#28): per candidate
    /// only the label, the shared duration, and an opaque audio reference. No
    /// name, path, sha256, size, or per-candidate technical metadata reaches the
    /// browser — the interface cannot leak what it does not have. Blind mode has
    /// already refused any mismatch, so the mismatch flags are omitted rather
    /// than reported as a uniform `false`. `loudness_match` still rides along
    /// (numbers keyed by label, no identity) so a `--blind --loudness-match`
    /// session applies the same per-lane gains.
    fn to_blind_json(&self) -> Value {
        let [a, b] = &self.candidates;
        json!({
            "blind": true,
            "candidates": [a.to_blind_json(), b.to_blind_json()],
            "loudness_match": self.blind_loudness_match_json(),
        })
    }

    /// The `loudness_match` object the record's `playback` carries and the
    /// session advertises (issue #30). Matching on: `enabled: true`, the
    /// `method` string, and a per-label map of `{measured_lufs, gain_db}`.
    /// Matching off: `{"enabled": false}` — the absence stated, not implied.
    pub fn loudness_match_json(&self) -> Value {
        match &self.loudness {
            Some(figures) => {
                let mut candidates = Map::new();
                for (c, l) in self.candidates.iter().zip(figures) {
                    candidates.insert(
                        c.label.to_string(),
                        json!({
                            "measured_lufs": finite(l.measured_lufs),
                            "gain_db": finite(l.gain_db),
                        }),
                    );
                }
                json!({
                    "enabled": true,
                    "method": "bs1770-integrated",
                    "candidates": candidates,
                })
            }
            None => json!({ "enabled": false }),
        }
    }

    /// The `loudness_match` a *blind* session may advertise (issue #32): matching
    /// on carries only `enabled`, the `method`, and per-label `gain_db` — the gain
    /// the engine must apply to play the lanes level-fair. The measured LUFS is
    /// held back: a distinctive loudness figure fingerprints a candidate, so it
    /// stays concealed until the conclude reveal. Matching off is the same
    /// `{"enabled": false}` as sighted — the absence stated, not implied.
    fn blind_loudness_match_json(&self) -> Value {
        match &self.loudness {
            Some(figures) => {
                let mut candidates = Map::new();
                for (c, l) in self.candidates.iter().zip(figures) {
                    candidates.insert(c.label.to_string(), json!({ "gain_db": finite(l.gain_db) }));
                }
                json!({
                    "enabled": true,
                    "method": "bs1770-integrated",
                    "candidates": candidates,
                })
            }
            None => json!({ "enabled": false }),
        }
    }

    /// The per-lane loudness figures for the conclude reveal (issue #32), aligned
    /// with `candidates` and finite-guarded: `(measured_lufs, gain_db)` when
    /// matching ran, else `None`. A blind session holds the measured figures back
    /// until this one irreversible event; they equal the numbers the record's
    /// `playback.loudness_match` carries.
    pub fn loudness_reveal(&self) -> Option<[(f64, f64); 2]> {
        self.loudness
            .map(|figures| figures.map(|l| (finite(l.measured_lufs), finite(l.gain_db))))
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
    /// A blind pair the interface cannot honestly conceal (#28): identical
    /// content or a duration/rate/channel mismatch. The string already names the
    /// property and both values.
    BlindRefused(String),
    /// OS randomness (`/dev/urandom`) was unavailable when shuffling the blind
    /// labels or minting an opaque audio reference (#28).
    Randomness(std::io::Error),
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
            LoadError::BlindRefused(msg) => write!(f, "{msg}"),
            LoadError::Randomness(source) => {
                write!(
                    f,
                    "cannot read OS randomness for the blind session: {source}"
                )
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Turn the two candidates' measured LUFS into per-lane match figures (issue
/// #30): every lane is attenuated by `quietest − lane` decibels, so the louder
/// lanes come down to the quietest and the quietest gets exactly 0.0. Gains are
/// always ≤ 0 (never boost, #66). A non-finite reading (a silent lane) is left
/// at 0 dB and does not drag the reference to −∞.
fn match_gains(lufs: [f64; 2]) -> [Loudness; 2] {
    let reference = lufs
        .iter()
        .copied()
        .filter(|l| l.is_finite())
        .fold(f64::INFINITY, f64::min);
    lufs.map(|measured_lufs| {
        let gain_db = if measured_lufs.is_finite() && reference.is_finite() {
            (reference - measured_lufs).min(0.0)
        } else {
            0.0
        };
        Loudness {
            measured_lufs,
            gain_db,
        }
    })
}

/// Hash, size, decode, and transcode one input into a `Candidate` plus its
/// cached playback proxy, returning the candidate's integrated loudness when
/// `measure` is set (issue #30) — measured over the decoded PCM before it is
/// dropped.
fn load_candidate(
    label: &'static str,
    path: &Path,
    cache: &Cache,
    proxies: &mut HashMap<String, Proxy>,
    measure: bool,
    blind: bool,
) -> Result<(Candidate, Option<f64>), LoadError> {
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

    // The reference the audio endpoint serves this proxy under: the source hash
    // in a sighted session (the page knows the content it asks for), a
    // per-session opaque token in a blind one, so the content hash never rides
    // in a blind response and the shuffle cannot be decoded by rehashing (#28).
    let audio_ref = if blind { opaque_ref()? } else { sha256.clone() };
    proxies.insert(audio_ref.clone(), proxy);

    // Measure loudness over the decoded PCM before it is dropped — the one place
    // the full samples are still in hand (issue #30). Only when matching is on.
    let lufs = if measure {
        Some(
            integrated_lufs(&decoded).map_err(|reason| LoadError::Undecodable {
                path: display.clone(),
                reason,
            })?,
        )
    } else {
        None
    };

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| display.clone());

    Ok((
        Candidate {
            label,
            name,
            path: display,
            sha256,
            size,
            frames: decoded.frames(),
            sample_rate: decoded.sample_rate,
            channels: decoded.channels,
            audio_ref,
        },
        lufs,
    ))
}

/// One byte of OS randomness reduced to a coin flip: the label↔file shuffle a
/// blind session opens with (#28). Linux-only (spec #1), so `/dev/urandom` is a
/// fine, dependency-free source — the same one the session token draws from.
fn coin_flip() -> Result<bool, LoadError> {
    let mut byte = [0u8; 1];
    fill_random(&mut byte)?;
    Ok(byte[0] & 1 == 1)
}

/// A per-session opaque audio reference: 16 bytes of OS randomness, hex-encoded
/// (#28). Content-derived identifiers (the source hash) never key a blind
/// session's audio, so a listener cannot decode the shuffle by hashing their
/// own inputs.
fn opaque_ref() -> Result<String, LoadError> {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes)?;
    Ok(hex(&bytes))
}

fn fill_random(buf: &mut [u8]) -> Result<(), LoadError> {
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(buf))
        .map_err(LoadError::Randomness)
}

#[cfg(test)]
mod tests {
    use super::match_gains;

    #[test]
    fn quietest_lane_is_untouched_and_louder_is_attenuated() {
        // B is quieter, so it is the reference: its gain is exactly 0, and the
        // louder A is pulled down by the difference (a negative gain).
        let [a, b] = match_gains([-11.2, -13.6]);
        assert_eq!(b.gain_db, 0.0, "the quietest lane keeps 0 dB");
        assert!((a.gain_db - (-2.4)).abs() < 1e-9, "A: {}", a.gain_db);
        assert!(
            a.gain_db < 0.0,
            "the louder lane is attenuated, never boosted"
        );
        assert_eq!(a.measured_lufs, -11.2);
        assert_eq!(b.measured_lufs, -13.6);
    }

    #[test]
    fn a_silent_lane_does_not_drag_the_reference_to_negative_infinity() {
        // A non-finite reading is left at 0 dB and excluded from the reference,
        // so the finite lane still matches to a real gain rather than −∞.
        let [a, b] = match_gains([f64::NEG_INFINITY, -14.0]);
        assert_eq!(a.gain_db, 0.0);
        assert_eq!(b.gain_db, 0.0, "the only finite lane is the reference");
    }
}
