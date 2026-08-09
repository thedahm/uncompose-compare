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
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::audio::{decode_pcm, integrated_lufs};
use crate::cache::{Cache, Proxy};
use crate::{hex, random_bytes};

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
    /// The manifest asset id this candidate resolved from, in project mode
    /// (spec #42); `None` for a bare-file launch. Recorded on the candidate.
    pub asset: Option<String>,
    /// The manifest project ULID, in project mode; `None` for a bare-file launch.
    pub project: Option<String>,
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
    /// The measured integrated loudness, LUFS. A lane too quiet or too short for
    /// the integrated gate reads `-inf` (ebur128's convention) and is carried as
    /// such — it is serialized as JSON `null`, never as a number.
    pub measured_lufs: f64,
    pub gain_db: f64,
}

impl Loudness {
    /// The measured figure as JSON: the number when the gate produced one, else
    /// `null`. A lane below the integrated gate has *no* measurement, and a
    /// record must say so — flooring `-inf` to `0.0` would engrave the loudest
    /// possible reading for the quietest possible lane (spec story 22: what was
    /// done to playback is auditable, never a mystery).
    pub fn measured_lufs_json(&self) -> Value {
        if self.measured_lufs.is_finite() {
            json!(self.measured_lufs)
        } else {
            Value::Null
        }
    }

    /// The applied gain as JSON. Always a number: `match_gains` leaves a lane it
    /// cannot measure at exactly 0 dB, so the gain — unlike the measurement — is
    /// never absent.
    pub fn gain_db_json(&self) -> Value {
        json!(finite(self.gain_db))
    }

    /// The full per-lane figures the record and a sighted session carry.
    fn figures_json(self) -> Value {
        json!({
            "measured_lufs": self.measured_lufs_json(),
            "gain_db": self.gain_db_json(),
        })
    }

    /// The reduced per-lane figures a blind session may advertise (ADR-0007):
    /// the applied gain only.
    fn blind_figures_json(self) -> Value {
        json!({ "gain_db": self.gain_db_json() })
    }
}

/// One lane to load: the resolved file, plus the manifest facts a project launch
/// carries (spec #42) — the expected content hash to check integrity against, and
/// the asset/project ids to record. A bare-file launch leaves all three `None`.
pub struct Lane {
    pub path: PathBuf,
    /// The manifest's recorded sha256 for this asset, checked against the bytes on
    /// disk at load; `None` for a bare-file launch (nothing to check against).
    pub expected_sha256: Option<String>,
    pub asset: Option<String>,
    pub project: Option<String>,
}

impl Lane {
    /// A bare-file lane: a path with no manifest facts (bare-file mode, and the
    /// SRC lane of `--source`).
    pub fn bare(path: PathBuf) -> Lane {
        Lane {
            path,
            expected_sha256: None,
            asset: None,
            project: None,
        }
    }
}

pub struct Session {
    pub candidates: [Candidate; 2],
    /// The SRC lane: the candidates' shared source in project mode, or the
    /// explicit `--source` file in bare-file mode (spec #42). `None` when there is
    /// no source (bare-file without `--source`, `--exclude-source`, or a project
    /// pair that shares none). It joins the sample-locked graph and the loudness
    /// match group, and — unlike A/B — stays identified in a blind session.
    pub source: Option<Candidate>,
    pub proxies: HashMap<String, Proxy>,
    /// Per-lane loudness figures for A and B when `--loudness-match` is on (aligned
    /// with `candidates`), else `None` — playback is then faithful as-is (#66).
    pub loudness: Option<[Loudness; 2]>,
    /// The SRC lane's loudness figures when matching is on and a source is present.
    /// SRC joins the match group (attenuate-to-quietest including the source, #66),
    /// so the A/B gains above are relative to the quietest of all three lanes.
    pub source_loudness: Option<Loudness>,
    /// A blind session (`--blind`): the label↔file assignment was shuffled at
    /// load and the served surface conceals every identifying detail (#28). The
    /// record still carries full identities — concealment is a session concern,
    /// not a storage one (uncompose#65).
    pub blind: bool,
}

impl Session {
    pub fn load(
        a: Lane,
        b: Lane,
        source: Option<Lane>,
        cache: &Cache,
        loudness_match: bool,
        blind: bool,
    ) -> Result<Self, LoadError> {
        let mut proxies = HashMap::new();
        // Blind mode shuffles which file the listener sees as A vs B by an
        // OS-randomness coin flip, so argument order tells them nothing (#28). The
        // SRC lane is never shuffled — it is the reference, and stays identified.
        let swap = blind && coin_flip()?;
        let (lane_a, lane_b) = if swap { (b, a) } else { (a, b) };
        let (ca, lufs_a) =
            load_candidate("A", &lane_a, cache, &mut proxies, loudness_match, blind)?;
        let (cb, lufs_b) =
            load_candidate("B", &lane_b, cache, &mut proxies, loudness_match, blind)?;
        // The SRC lane loads identified (blind = false): its audio reference is the
        // content hash, its metadata rides the payload — concealment is A/B only.
        let (source, lufs_src) = match source {
            Some(sl) => {
                let (c, l) =
                    load_candidate("SRC", &sl, cache, &mut proxies, loudness_match, false)?;
                (Some(c), l)
            }
            None => (None, None),
        };
        let (loudness, source_loudness) = match (lufs_a, lufs_b) {
            // Matching on: A and B are always measured (a below-gate lane reads
            // -inf, carried as such). SRC joins the reference search when present.
            (Some(la), Some(lb)) => {
                let (ab, src) = match_gains(la, lb, lufs_src);
                (Some(ab), src)
            }
            _ => (None, None),
        };
        let session = Session {
            candidates: [ca, cb],
            source,
            proxies,
            loudness,
            source_loudness,
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
            // Both values, at a precision that can actually tell them apart: a
            // one-frame difference is a real refusal, and `{:.0} ms` would print
            // it as two equal numbers. The frame counts are directly comparable
            // here — a rate mismatch was already refused above.
            return Err(LoadError::BlindRefused(format!(
                "cannot blind-compare {} and {}: duration mismatch ({:.3} ms vs {:.3} ms; {} frames vs {} frames)",
                a.path,
                b.path,
                a.duration_ms(),
                b.duration_ms(),
                a.frames,
                b.frames
            )));
        }
        Ok(())
    }

    /// True when the two candidates differ in duration. Reported alongside the
    /// deltas so the page can warn without blocking playback of either file.
    ///
    /// At equal sample rates the frame counts *are* the duration, so any frame
    /// difference counts — that is the sample-exact check the workbench wants.
    /// Across different rates a frame delta means nothing (44 100 frames and
    /// 48 000 frames are both one second), so only the elapsed time decides; the
    /// rate difference itself is reported by `sample_rate_mismatch` (#26 story
    /// 11), and a listener is told the one thing that is true rather than warned
    /// twice, once wrongly.
    pub fn duration_mismatch(&self) -> bool {
        let [a, b] = &self.candidates;
        let frames_differ = a.sample_rate == b.sample_rate && a.frames != b.frames;
        frames_differ || (a.duration_ms() - b.duration_ms()).abs() > 0.5
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
        let mut meta = json!({
            "candidates": [a.to_json(), b.to_json()],
            "duration_mismatch": self.duration_mismatch(),
            "duration_delta_ms": finite((a.duration_ms() - b.duration_ms()).abs()),
            "loudness_match": self.loudness_match_json(),
            "sample_rate_mismatch": self.sample_rate_mismatch(),
            "channel_count_mismatch": self.channel_count_mismatch(),
        });
        // A sample delta is only a delta when the two lanes count samples at the
        // same rate; across rates it is arithmetic on incomparable units, so it
        // is omitted rather than reported as a number that means nothing.
        if !self.sample_rate_mismatch() {
            meta["duration_delta_samples"] = json!((a.frames as i64 - b.frames as i64).abs());
        }
        // The SRC lane, when present: a third identified lane (spec #42), the same
        // shape as an A/B candidate so the workbench renders it alongside them.
        if let Some(source) = &self.source {
            meta["source"] = source.to_json();
        }
        meta
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
        let mut meta = json!({
            "blind": true,
            "candidates": [a.to_blind_json(), b.to_blind_json()],
            "loudness_match": self.blind_loudness_match_json(),
        });
        // Concealment is A/B only (spec #42): the SRC lane is the shared reference
        // both candidates are compared against, and revealing it discloses nothing
        // about which candidate is which. It rides the blind payload fully
        // identified — the same shape a sighted session serves it.
        if let Some(source) = &self.source {
            meta["source"] = source.to_json();
        }
        meta
    }

    /// The `loudness_match` object the record's `playback` carries and the
    /// session advertises (issue #30). Matching on: `enabled: true`, the
    /// `method` string, and a per-label map of `{measured_lufs, gain_db}`.
    pub fn loudness_match_json(&self) -> Value {
        self.loudness_match_json_with(Loudness::figures_json)
    }

    /// The `loudness_match` a *blind* session may advertise (issue #32): matching
    /// on carries only `enabled`, the `method`, and per-label `gain_db` — the gain
    /// the engine must apply to play the lanes level-fair. The measured LUFS is
    /// held back: a distinctive loudness figure fingerprints a candidate, so it
    /// stays concealed until the conclude reveal.
    fn blind_loudness_match_json(&self) -> Value {
        self.loudness_match_json_with(Loudness::blind_figures_json)
    }

    /// Shared scaffolding for the two `loudness_match` projections above: the
    /// sighted/record shape and the blind one differ only in what each lane
    /// carries, so `lane` supplies the per-label payload. Matching off is
    /// `{"enabled": false}` in both — the absence stated, not implied.
    fn loudness_match_json_with(&self, lane: impl Fn(Loudness) -> Value) -> Value {
        match self.loudness {
            Some(figures) => {
                let mut candidates = Map::new();
                for (c, l) in self.candidates.iter().zip(figures) {
                    candidates.insert(c.label.to_string(), lane(l));
                }
                // SRC joins the match group (spec #42, #66): its own attenuation is
                // recorded alongside A/B's so the record documents what was done to
                // every playback lane. In a blind session `lane` is the gain-only
                // projection, so SRC's measured figure stays concealed too — showing
                // it would let a listener derive A/B's measured LUFS from the gains
                // when SRC is the quietest reference.
                if let (Some(src), Some(sl)) = (&self.source, self.source_loudness) {
                    candidates.insert(src.label.to_string(), lane(sl));
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
    /// with `candidates`: the measured `Loudness` per lane when matching ran, else
    /// `None`. The named struct travels, not a bare pair — the reveal and the
    /// record label the same two numbers, so the pairing stays nominal rather than
    /// riding on tuple order. A blind session holds the measured figures back
    /// until this one irreversible event; they equal the numbers the record's
    /// `playback.loudness_match` carries.
    pub fn loudness_reveal(&self) -> Option<[Loudness; 2]> {
        self.loudness
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
    /// The file decoded, but `--loudness-match` could not measure it (issue #30).
    /// Its own variant so the message diagnoses the flag rather than blaming the
    /// file for a corruption it does not have.
    Unmeasurable {
        path: String,
        reason: String,
    },
    /// A blind pair the interface cannot honestly conceal (#28): identical
    /// content or a duration/rate/channel mismatch. The string already names the
    /// property and both values.
    BlindRefused(String),
    /// A project-mode asset whose bytes on disk no longer hash to the sha256 the
    /// manifest records (spec #42). Refused before binding, naming the asset.
    HashMismatch {
        asset: String,
        path: String,
        expected: String,
        actual: String,
    },
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
            LoadError::Unmeasurable { path, reason } => {
                write!(
                    f,
                    "cannot measure the loudness of {path} for --loudness-match: {reason}"
                )
            }
            LoadError::BlindRefused(msg) => write!(f, "{msg}"),
            LoadError::HashMismatch {
                asset,
                path,
                expected,
                actual,
            } => write!(
                f,
                "asset {asset} ({path}) does not match the manifest: \
                 expected sha256 {expected}, found {actual}"
            ),
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

/// Turn the lanes' measured LUFS into per-lane match figures (issue #30): every
/// lane is attenuated by `quietest − lane` decibels, so the louder lanes come
/// down to the quietest and the quietest gets exactly 0.0. Gains are always ≤ 0
/// (never boost, #66). A non-finite reading (a silent lane) is left at 0 dB and
/// does not drag the reference to −∞.
///
/// The SRC lane, when present, joins the reference search (spec #42, #66):
/// attenuate-to-quietest across A, B, *and* the source, so A/B are level-fair
/// against the reference they are compared to. It returns its own `Loudness`
/// alongside the A/B pair.
fn match_gains(a: f64, b: f64, source: Option<f64>) -> ([Loudness; 2], Option<Loudness>) {
    let reference = [Some(a), Some(b), source]
        .into_iter()
        .flatten()
        .filter(|l| l.is_finite())
        .fold(f64::INFINITY, f64::min);
    let lane = |measured_lufs: f64| {
        let gain_db = if measured_lufs.is_finite() && reference.is_finite() {
            (reference - measured_lufs).min(0.0)
        } else {
            0.0
        };
        Loudness {
            measured_lufs,
            gain_db,
        }
    };
    ([lane(a), lane(b)], source.map(lane))
}

/// Hash, size, decode, and transcode one input into a `Candidate` plus its
/// cached playback proxy, returning the candidate's integrated loudness when
/// `measure` is set (issue #30) — measured over the decoded PCM before it is
/// dropped.
fn load_candidate(
    label: &'static str,
    lane: &Lane,
    cache: &Cache,
    proxies: &mut HashMap<String, Proxy>,
    measure: bool,
    blind: bool,
) -> Result<(Candidate, Option<f64>), LoadError> {
    let path = lane.path.as_path();
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

    // Integrity (spec #42): in project mode the manifest records each asset's
    // sha256. The resolved file is hashed here by the existing pipeline, so a
    // manifest that no longer matches the bytes on disk refuses before the server
    // binds — naming the asset that drifted.
    if let Some(expected) = &lane.expected_sha256 {
        if expected != &sha256 {
            return Err(LoadError::HashMismatch {
                asset: lane.asset.clone().unwrap_or_else(|| display.clone()),
                path: display.clone(),
                expected: expected.clone(),
                actual: sha256,
            });
        }
    }

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
            integrated_lufs(&decoded).map_err(|reason| LoadError::Unmeasurable {
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
            asset: lane.asset.clone(),
            project: lane.project.clone(),
            frames: decoded.frames(),
            sample_rate: decoded.sample_rate,
            channels: decoded.channels,
            audio_ref,
        },
        lufs,
    ))
}

/// One byte of OS randomness reduced to a coin flip: the label↔file shuffle a
/// blind session opens with (#28). The bytes come from the crate's one
/// `random_bytes` reader — the same source the session token and the record ULID
/// draw from.
fn coin_flip() -> Result<bool, LoadError> {
    let mut byte = [0u8; 1];
    random_bytes(&mut byte).map_err(LoadError::Randomness)?;
    Ok(byte[0] & 1 == 1)
}

/// A per-session opaque audio reference: 16 bytes of OS randomness, hex-encoded
/// (#28). Content-derived identifiers (the source hash) never key a blind
/// session's audio, so a listener cannot decode the shuffle by hashing their
/// own inputs.
fn opaque_ref() -> Result<String, LoadError> {
    let mut bytes = [0u8; 16];
    random_bytes(&mut bytes).map_err(LoadError::Randomness)?;
    Ok(hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::match_gains;

    #[test]
    fn quietest_lane_is_untouched_and_louder_is_attenuated() {
        // B is quieter, so it is the reference: its gain is exactly 0, and the
        // louder A is pulled down by the difference (a negative gain).
        let ([a, b], src) = match_gains(-11.2, -13.6, None);
        assert!(src.is_none(), "no source lane when none is passed");
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
        let ([a, b], _) = match_gains(f64::NEG_INFINITY, -14.0, None);
        assert_eq!(a.gain_db, 0.0);
        assert_eq!(b.gain_db, 0.0, "the only finite lane is the reference");
    }

    #[test]
    fn the_source_lane_joins_the_reference_search() {
        // SRC is the quietest lane, so it becomes the 0 dB reference and both A
        // and B are attenuated down to it (spec #42, #66).
        let ([a, b], src) = match_gains(-11.2, -13.6, Some(-16.0));
        let src = src.expect("a source lane");
        assert_eq!(src.gain_db, 0.0, "the quietest lane (SRC) is the reference");
        assert!(
            (a.gain_db - (-16.0 - -11.2)).abs() < 1e-9,
            "A: {}",
            a.gain_db
        );
        assert!(
            (b.gain_db - (-16.0 - -13.6)).abs() < 1e-9,
            "B: {}",
            b.gain_db
        );
    }
}
