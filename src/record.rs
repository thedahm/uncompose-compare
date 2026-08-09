//! The v0 comparison record (issue #16, decision uncompose#65): assemble it
//! from the server-authoritative fields plus the session-authored body,
//! validate it against the schema this repo owns, and write it exactly once.
//!
//! The schema is embedded so the record endpoint validates against the exact
//! bytes committed here — no drift between the published schema and what the
//! server enforces, and no file read at conclude time. The `schema` field every
//! record carries is the schema's own `$id`, read out of those same bytes
//! rather than hand-copied (the schema pins it with `const`, so a record can
//! only claim the id the schema declares).
//!
//! The write itself is atomic: the destination is reserved with `create_new`
//! (ADR-0002's existence check — an existing destination is refused, never
//! overwritten), the bytes go to a temp sibling, and a rename puts them in
//! place. A failure mid-write leaves no partial record and no reservation, so
//! the listener can fix the cause and conclude again.

use std::cell::Cell;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::random_bytes;
use crate::session::Session;

/// The v0 comparison-record JSON Schema, this repo's owned artifact.
const SCHEMA_STR: &str = include_str!("../schemas/compare/v0/uncompose.compare.schema.json");

/// The comparison-record writer: the compiled schema validator, the destination
/// policy, the session's start time, and the write-once guard. The server loop
/// is single-threaded, so a `Cell` is all "written exactly once" needs — a
/// successful write flips it, and every later conclude is refused.
pub struct Recorder {
    validator: jsonschema::Validator,
    /// The schema's `$id`, read from the embedded schema: the value every
    /// written record carries in its `schema` field.
    schema_id: String,
    /// Where a concluded record lands, and — in project mode — the handover that
    /// registers it (ADR-0002, spec #42 slice 5).
    destination: Destination,
    /// The session's `created_at`, stamped once at startup (RFC 3339, UTC).
    created_at: String,
    concluded: Cell<bool>,
}

/// Where a concluded record lands (ADR-0002, spec #42 slice 5). A standalone
/// session writes into the invoking directory and hands the record to no one; a
/// project session writes into the manifest's `evaluations/` and auto-imports it.
pub enum Destination {
    /// Standalone: `<ULID>.json` in the invoking directory `dir`, or the `--out`
    /// override (resolved against `dir` when relative). No handover.
    Standalone { out: Option<PathBuf>, dir: PathBuf },
    /// Project mode: `<root>/evaluations/<ULID>.json` (the directory created if
    /// needed), followed by the auto-import handover
    /// `uncompose-project import --project <root> <record>`. `root` is absolute so
    /// the handover's argv carries absolute paths.
    Project { root: PathBuf },
}

impl Destination {
    /// A standalone destination: the invoking directory, and an optional `--out`.
    pub fn standalone(out: Option<PathBuf>, dir: PathBuf) -> Destination {
        Destination::Standalone { out, dir }
    }

    /// A project destination rooted at (absolute) `root`: the record lands under
    /// `root/evaluations/` and is handed to `uncompose-project import`.
    pub fn project(root: PathBuf) -> Destination {
        Destination::Project { root }
    }

    /// The record's path for the minted `id` under this destination policy.
    fn path_for(&self, id: &str) -> PathBuf {
        match self {
            // `join` keeps an absolute `--out` as-is and resolves a relative one
            // against the invoking directory.
            Destination::Standalone {
                out: Some(out),
                dir,
            } => dir.join(out),
            Destination::Standalone { out: None, dir } => dir.join(format!("{id}.json")),
            Destination::Project { root } => root.join("evaluations").join(format!("{id}.json")),
        }
    }

    /// Create the parent directory this policy owns before the write. Project
    /// mode's `evaluations/` is the handover's directory, created if needed;
    /// a standalone session writes into a directory that already exists (the
    /// invoking cwd, or the `--out` parent the caller chose).
    fn prepare(&self, dest: &Path) -> Result<(), RecordError> {
        if let Destination::Project { .. } = self {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    RecordError::Io(format!("cannot create {}: {e}", parent.display()))
                })?;
            }
        }
        Ok(())
    }
}

/// A successful conclude: where the immutable record landed, the reveal a blind
/// session's UI displays, and — in project mode — how registration went.
///
/// The reveal is the label→file mapping (`{label, path, sha256, size}` per
/// candidate), identical to what the record carries (#29), each lane also
/// carrying its `{measured_lufs, gain_db}` when loudness matching ran (#32). A
/// sighted session reveals nothing: it never concealed anything, so `reveal` is
/// `None` and the response omits the key (ADR-0006).
///
/// `registration` is the project-mode auto-import outcome (spec #42 slice 5); a
/// standalone session hands the record to no one, so it is `None`.
pub struct Conclusion {
    pub path: String,
    pub reveal: Option<Value>,
    pub registration: Option<Registration>,
}

/// The project-mode registration outcome that rides the conclude response
/// (spec #42 slice 5, #67 res. 7), so the closing screen can show "registered"
/// or the recovery command.
pub enum Registration {
    /// `uncompose-project import` accepted the record.
    Registered,
    /// The import failed. The record is kept — it is never the casualty — so the
    /// tool's stderr is relayed and the exact recovery command is offered; no
    /// retry, no rollback.
    Failed { stderr: String, recovery: String },
}

impl Registration {
    /// The registration outcome as JSON for the conclude response.
    pub fn to_json(&self) -> Value {
        match self {
            Registration::Registered => json!({ "registered": true }),
            Registration::Failed { stderr, recovery } => json!({
                "registered": false,
                "error": stderr,
                "recovery": recovery,
            }),
        }
    }
}

impl Conclusion {
    /// The process lifecycle after this (successful) conclude: the exit code and
    /// the stderr lines to relay before exiting. Save-and-close is one act
    /// (#67 res. 8) — the process exits either way. It exits 0 when the record was
    /// saved (standalone) and, in project mode, registered; nonzero when the
    /// auto-import failed, relaying the tool's stderr and printing the exact
    /// recovery command last (#67 res. 7).
    pub fn lifecycle(&self) -> (i32, Vec<String>) {
        match &self.registration {
            None | Some(Registration::Registered) => (0, Vec::new()),
            Some(Registration::Failed { stderr, recovery }) => {
                let mut lines = vec![format!(
                    "uncompose-compare: the record was saved to {} but could not be \
                     registered with the project:",
                    self.path
                )];
                let relayed = stderr.trim_end();
                if !relayed.is_empty() {
                    lines.push(relayed.to_string());
                }
                // The exact recovery command, printed last.
                lines.push(recovery.clone());
                (1, lines)
            }
        }
    }
}

/// Why a conclude was refused, mapped to an HTTP status the UI can act on.
pub enum RecordError {
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
    pub fn status(&self) -> u16 {
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
    pub fn new(
        destination: Destination,
        created_at: SystemTime,
    ) -> Result<Recorder, Box<dyn std::error::Error + Send + Sync>> {
        let schema: Value = serde_json::from_str(SCHEMA_STR)?;
        let schema_id = schema
            .get("$id")
            .and_then(Value::as_str)
            .ok_or("the embedded comparison schema has no $id")?
            .to_string();
        let validator = jsonschema::validator_for(&schema).map_err(|e| e.to_string())?;
        Ok(Recorder {
            validator,
            schema_id,
            destination,
            created_at: rfc3339(created_at),
            concluded: Cell::new(false),
        })
    }

    /// Assemble the record from the server-authoritative fields plus the
    /// session-authored body, validate it, and write it once. The server owns
    /// `schema`/`id`/timestamps/`candidates`/`mode`/`playback` (so hashes and
    /// paths can't be forged from the browser); the body supplies only `result`,
    /// `observations`, `loops`, and `context`.
    ///
    /// On success returns where the record landed plus the reveal — the
    /// label→file mapping (`{label, path, sha256, size}` per candidate, identical
    /// to what was written), plus each lane's `{measured_lufs, gain_db}` when
    /// loudness matching ran (#32). A blind session (#29) concealed identity — and,
    /// composed with matching, the measured loudness figures (#32) — from the
    /// browser until now; the successful write is the one irreversible event, so
    /// the reveal rides its response and nothing before it.
    pub fn conclude(&self, session: &Session, body: &str) -> Result<Conclusion, RecordError> {
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
                let mut candidate = json!({
                    "label": c.label,
                    "path": c.path,
                    "sha256": c.sha256,
                    "size": c.size,
                });
                // Project mode (spec #42): a candidate resolved from the manifest
                // carries its asset id and the project ULID, in the slots the v0
                // schema already declares. A bare-file candidate carries neither.
                if let Some(asset) = &c.asset {
                    candidate["asset"] = json!(asset);
                }
                if let Some(project) = &c.project {
                    candidate["project"] = json!(project);
                }
                candidate
            })
            .collect();

        let id = new_ulid().map_err(|e| RecordError::Io(e.to_string()))?;

        // The reveal is the trusted candidates the record carries — the mapping the
        // UI shows post-conclude cannot diverge from what was written — plus, when
        // loudness matching ran (issue #32), each lane's measured LUFS and applied
        // gain. A blind session concealed the measured figures until now; they are
        // shown together with the identities at this one irreversible event. Only a
        // blind session has anything to reveal (ADR-0006).
        let reveal = session.blind.then(|| {
            let mut reveal = candidates.clone();
            if let Some(figures) = session.loudness_reveal() {
                for (entry, lane) in reveal.iter_mut().zip(figures) {
                    entry["measured_lufs"] = lane.measured_lufs_json();
                    entry["gain_db"] = lane.gain_db_json();
                }
            }
            Value::Array(reveal)
        });

        let mut record = json!({
            "schema": self.schema_id,
            "id": id,
            "created_at": self.created_at,
            "completed_at": rfc3339(SystemTime::now()),
            "candidates": candidates,
            // Sighted `ab`, or `ab-blind-randomized` when the labels were
            // shuffled (#28). The candidates above already carry the shuffled
            // label mapped to each real path/sha256/size, so the record alone
            // reconnects what the listener saw to what was on disk.
            "mode": session.mode(),
            // Playback is server-authoritative (issue #30): the loudness match,
            // measured and applied server-side, is recorded here — never taken
            // from the browser's posted body.
            "playback": { "loudness_match": session.loudness_match_json() },
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
        // The schema types candidate references as free strings (it has to: the
        // labels are a runtime fact). The server knows which labels exist, so it
        // is the one place a reference to a candidate that was never loaded can
        // be caught before it is engraved.
        check_candidate_references(&record, &session.labels())?;

        let dest = self.destination.path_for(&id);
        self.destination.prepare(&dest)?;
        self.write_once(&dest, &record)?;

        // Only a completed write concludes the session; that write is the one,
        // and it stands whatever the handover does next — the record is never the
        // casualty (#67 res. 7).
        self.concluded.set(true);

        // Project mode: hand the written record to `uncompose-project import`. Its
        // outcome rides the conclude response; a failure keeps the record and is
        // relayed, never rolled back.
        let registration = match &self.destination {
            Destination::Project { root } => Some(import(root, &dest)),
            Destination::Standalone { .. } => None,
        };

        Ok(Conclusion {
            path: dest.display().to_string(),
            reveal,
            registration,
        })
    }

    /// Write `record` to `dest` atomically, exactly once.
    ///
    /// `create_new` reserves the destination — that is ADR-0002's existence
    /// check, and it refuses an existing record without reading or truncating
    /// it. The bytes themselves land in a temp sibling and are renamed over the
    /// reservation, so `dest` only ever contains a whole record: a failure
    /// part-way through leaves neither a partial file nor a reservation that
    /// would make every retry look like an overwrite.
    fn write_once(&self, dest: &Path, record: &Value) -> Result<(), RecordError> {
        let serialized =
            serde_json::to_string_pretty(record).map_err(|e| RecordError::Io(e.to_string()))?;

        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
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

        let tmp = temp_sibling(dest);
        match write_all_synced(&tmp, serialized.as_bytes())
            .and_then(|_| std::fs::rename(&tmp, dest))
        {
            Ok(()) => Ok(()),
            Err(e) => {
                // Undo both halves so the listener can fix the cause (a full
                // disk, a vanished directory) and conclude again.
                let _ = std::fs::remove_file(&tmp);
                let _ = std::fs::remove_file(dest);
                Err(RecordError::Io(format!(
                    "cannot write {}: {e}",
                    dest.display()
                )))
            }
        }
    }
}

/// Hand the written record to `uncompose-project import --project <root>
/// <record>` (the pinned argv, spec #42 slice 5). Both paths are absolute. A
/// clean exit registers; any nonzero exit — or a tool that would not run —
/// relays the failure and offers the exact recovery command, the record kept.
fn import(root: &Path, record: &Path) -> Registration {
    let recovery = format!("uncompose project import {}", record.display());
    match Command::new("uncompose-project")
        .arg("import")
        .arg("--project")
        .arg(root)
        .arg(record)
        .output()
    {
        Ok(out) if out.status.success() => Registration::Registered,
        Ok(out) => Registration::Failed {
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            recovery,
        },
        Err(e) => Registration::Failed {
            stderr: format!("could not run `uncompose-project import`: {e}"),
            recovery,
        },
    }
}

/// Write `bytes` to `path` and flush them to the filesystem before the caller
/// renames it into place — a rename of unsynced bytes is atomic in name only.
fn write_all_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()
}

/// A temp name beside `dest`, so the rename that follows stays on one
/// filesystem. Unique per process: two concurrent writers never share it.
fn temp_sibling(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "record.json".to_string());
    let dir = dest.parent().unwrap_or(Path::new("."));
    dir.join(format!(".{name}.{}.tmp", std::process::id()))
}

/// Reject a record whose candidate references name something this session never
/// loaded: `result.preference` (null, or a label) and each observation's
/// optional `candidate` (a label). Everything else candidate-related is
/// server-authoritative; this closes the one hole the schema cannot.
fn check_candidate_references(record: &Value, labels: &[&str]) -> Result<(), RecordError> {
    let known = |v: &Value| v.as_str().is_some_and(|s| labels.contains(&s));

    if let Some(preference) = record.pointer("/result/preference") {
        if !preference.is_null() && !known(preference) {
            return Err(RecordError::Invalid(format!(
                "result.preference must be null or one of the candidate labels {labels:?}, got {preference}"
            )));
        }
    }
    if let Some(observations) = record["observations"].as_array() {
        for (i, observation) in observations.iter().enumerate() {
            if let Some(candidate) = observation.get("candidate") {
                if !known(candidate) {
                    return Err(RecordError::Invalid(format!(
                        "observations[{i}].candidate must be one of the candidate labels {labels:?}, got {candidate}"
                    )));
                }
            }
        }
    }
    Ok(())
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
    random_bytes(&mut rand)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with(result: Value, observations: Value) -> Value {
        json!({ "result": result, "observations": observations })
    }

    #[test]
    fn candidate_references_must_name_a_loaded_candidate() {
        let labels = ["A", "B"];

        let ok = record_with(
            json!({"preference": "B", "confidence": 3}),
            json!([{"at": "2026-08-08T10:00:00Z", "candidate": "A", "text": "bright"}]),
        );
        assert!(check_candidate_references(&ok, &labels).is_ok());

        // No preference is an honest outcome, not a dangling reference.
        let none = record_with(json!({"preference": null}), json!([]));
        assert!(check_candidate_references(&none, &labels).is_ok());

        let bad_preference = record_with(json!({"preference": "Z", "confidence": 3}), json!([]));
        assert!(check_candidate_references(&bad_preference, &labels).is_err());

        let bad_observation = record_with(
            json!({"preference": null}),
            json!([{"at": "2026-08-08T10:00:00Z", "candidate": "Z", "text": "?"}]),
        );
        assert!(check_candidate_references(&bad_observation, &labels).is_err());
    }
}
