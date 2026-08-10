//! Project mode (M5 slice 4, spec #42): resolve the two positionals as manifest
//! refs instead of filesystem paths, and auto-resolve the candidates' shared
//! source into the SRC lane.
//!
//! `--project <dir>` names a project root; the manifest is read directly from
//! exactly `<dir>/uncompose.project.json` (no upward walk, no plumbing command).
//! Each positional is a *ref* into that manifest, in one of two v0.1 forms:
//!
//! - a bare token is an **asset id** — the manifest asset whose `id` matches;
//! - `<name>@<derivation>` selects, among that derivation's **output** assets,
//!   the one whose file basename-stem equals `<name>`.
//!
//! A raw path refuses in project mode (the refs are manifest handles, not files).
//! No-match and ambiguity both list the derivation's outputs with id and
//! filename, so the message alone tells the listener what they could have named.
//!
//! The SRC lane is auto-resolved: the asset that is an input of *both*
//! candidates' producing derivations. Share none and the lane is simply absent —
//! not an error, and the launch states it (`main.rs`) rather than leaving the
//! listener to notice; `--exclude-source` opts out entirely. The
//! resolved files carry their manifest sha256, checked at load by the existing
//! hashing pipeline (`session.rs`), so a manifest that no longer matches the
//! bytes on disk refuses before the server binds.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The manifest filename, fixed under the project root (no upward walk).
pub const MANIFEST_NAME: &str = "uncompose.project.json";

/// One manifest asset: a content-addressed file. Its `id` is the human handle
/// (schema v0 types it as a slug), and bare-token refs match it.
pub struct Asset {
    pub id: String,
    /// The asset's file path, relative to the project root.
    pub path: String,
    pub sha256: String,
}

impl Asset {
    /// The file's basename-stem — its filename without the final extension. The
    /// `<name>` in a `<name>@<derivation>` ref matches this.
    fn stem(&self) -> String {
        Path::new(&self.path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// One derivation: a processing step with input and output assets (both by id).
pub struct Derivation {
    pub id: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// A parsed project manifest, plus the root its asset files resolve against.
pub struct Manifest {
    /// The project's ULID — recorded on each project-launched candidate.
    pub id: String,
    pub assets: Vec<Asset>,
    pub derivations: Vec<Derivation>,
    root: PathBuf,
}

/// A ref resolved to a concrete asset, plus the derivation it was selected
/// through (`Some` for the `name@derivation` form, `None` for a bare id — the
/// shared-source search then looks up the asset's producers itself).
pub struct Resolved<'a> {
    pub asset: &'a Asset,
    derivation: Option<&'a Derivation>,
}

impl Resolved<'_> {
    /// The asset's absolute file path, under the project root.
    pub fn path(&self, manifest: &Manifest) -> PathBuf {
        manifest.root.join(&self.asset.path)
    }
}

impl Manifest {
    /// Read and parse `<dir>/uncompose.project.json`. Every failure is phrased so
    /// the message alone diagnoses it — missing, unreadable, or malformed.
    pub fn load(dir: &Path) -> Result<Manifest, ProjectError> {
        let path = dir.join(MANIFEST_NAME);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProjectError::ManifestMissing {
                    path: path.display().to_string(),
                })
            }
            Err(source) => {
                return Err(ProjectError::ManifestUnreadable {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        let display = path.display().to_string();
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| {
            ProjectError::manifest_invalid(&display, format!("not valid JSON: {e}"))
        })?;

        // The project's identity lives at `project.id` (schema v0), not at the
        // top level of the manifest.
        let id = value
            .pointer("/project/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProjectError::manifest_invalid(&display, "missing string \"project.id\"")
            })?
            .to_string();

        let assets = parse_assets(&value, &display)?;
        let derivations = parse_derivations(&value, &display)?;

        Ok(Manifest {
            id,
            assets,
            derivations,
            root: dir.to_path_buf(),
        })
    }

    /// Resolve one positional ref (v0.1 grammar). A raw path refuses; a bare token
    /// is an asset id; `<name>@<derivation>` selects among that derivation's
    /// outputs by basename-stem.
    pub fn resolve(&self, token: &str) -> Result<Resolved<'_>, ProjectError> {
        if token.contains('/') || token.contains('\\') {
            return Err(ProjectError::RawPath {
                token: token.to_string(),
            });
        }

        match token.split_once('@') {
            Some((name, deriv_id)) => self.resolve_in_derivation(name, deriv_id),
            None => self.resolve_id(token),
        }
    }

    fn resolve_id(&self, id: &str) -> Result<Resolved<'_>, ProjectError> {
        // Ids are the manifest's identity and unique in anything
        // uncompose-project writes; the many-branch is defence against a
        // hand-edited manifest, phrased like every other ambiguity.
        let matches: Vec<&Asset> = self.assets.iter().filter(|a| a.id == id).collect();
        match matches.as_slice() {
            [] => Err(ProjectError::NoMatch(format!(
                "no asset with id {id:?} in the project. Known ids: {}",
                self.id_list()
            ))),
            [asset] => Ok(Resolved {
                asset,
                derivation: None,
            }),
            many => Err(ProjectError::Ambiguous(format!(
                "id {id:?} names {} assets: {}",
                many.len(),
                asset_listing(many, |a| a.id.as_str())
            ))),
        }
    }

    fn resolve_in_derivation(
        &self,
        name: &str,
        deriv_id: &str,
    ) -> Result<Resolved<'_>, ProjectError> {
        let derivation = self
            .derivations
            .iter()
            .find(|d| d.id == deriv_id)
            .ok_or_else(|| {
                ProjectError::NoMatch(format!(
                    "no derivation {deriv_id:?} in the project. Known derivations: {}",
                    self.derivation_list()
                ))
            })?;

        let outputs: Vec<&Asset> = derivation
            .outputs
            .iter()
            .filter_map(|id| self.asset(id))
            .collect();
        let matches: Vec<&Asset> = outputs
            .iter()
            .copied()
            .filter(|a| a.stem() == name)
            .collect();
        match matches.as_slice() {
            [] => Err(ProjectError::NoMatch(format!(
                "no output of derivation {deriv_id:?} has basename {name:?}. Outputs: {}",
                outputs_listing(&outputs)
            ))),
            [asset] => Ok(Resolved {
                asset,
                derivation: Some(derivation),
            }),
            many => Err(ProjectError::Ambiguous(format!(
                "{} outputs of derivation {deriv_id:?} share basename {name:?}: {}",
                many.len(),
                outputs_listing(many)
            ))),
        }
    }

    /// The shared source of two resolved candidates: the asset that is an input of
    /// *both* candidates' producing derivations. `None` when they share none (a
    /// stated absence, not an error); ambiguous when they share more than one.
    pub fn shared_source(
        &self,
        a: &Resolved<'_>,
        b: &Resolved<'_>,
    ) -> Result<Option<&Asset>, ProjectError> {
        let inputs_a = self.producer_inputs(a);
        let inputs_b = self.producer_inputs(b);
        let exclude = [a.asset.id.as_str(), b.asset.id.as_str()];
        let mut shared: Vec<&str> = inputs_a
            .iter()
            .map(String::as_str)
            .filter(|id| inputs_b.iter().any(|i| i == id) && !exclude.contains(id))
            .collect();
        shared.sort_unstable();
        shared.dedup();

        match shared.as_slice() {
            [] => Ok(None),
            [id] => Ok(self.asset(id)),
            many => Err(ProjectError::SourceAmbiguous(format!(
                "candidates {} and {} share {} possible sources: {}. Pass --exclude-source to omit the SRC lane.",
                a.asset.id,
                b.asset.id,
                many.len(),
                many.join(", ")
            ))),
        }
    }

    /// The input asset ids of a resolved candidate's producing derivations: the
    /// explicit derivation for a `name@derivation` ref, else every derivation that
    /// outputs the bare-id asset.
    fn producer_inputs(&self, resolved: &Resolved<'_>) -> Vec<String> {
        let producers: Vec<&Derivation> = match resolved.derivation {
            Some(d) => vec![d],
            None => self
                .derivations
                .iter()
                .filter(|d| d.outputs.iter().any(|o| o == &resolved.asset.id))
                .collect(),
        };
        producers
            .iter()
            .flat_map(|d| d.inputs.iter().cloned())
            .collect()
    }

    fn asset(&self, id: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.id == id)
    }

    fn id_list(&self) -> String {
        let mut ids: Vec<&str> = self.assets.iter().map(|a| a.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.join(", ")
    }

    fn derivation_list(&self) -> String {
        self.derivations
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Render assets as `<lead> (filename)` pairs — the one listing shape every
/// resolution error uses, so a listener reads the same thing everywhere. `lead`
/// is the asset id today; the parenthesised path is what tells entries apart
/// when the ids themselves collided.
fn asset_listing(assets: &[&Asset], lead: impl Fn(&Asset) -> &str) -> String {
    assets
        .iter()
        .map(|a| format!("{} ({})", lead(a), a.path))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A derivation's outputs, listed by the id a ref would name.
fn outputs_listing(outputs: &[&Asset]) -> String {
    asset_listing(outputs, |a| a.id.as_str())
}

fn parse_assets(value: &Value, display: &str) -> Result<Vec<Asset>, ProjectError> {
    let array = value
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| ProjectError::manifest_invalid(display, "missing array \"assets\""))?;
    array
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let field = |name: &str| {
                a.get(name)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| {
                        ProjectError::manifest_invalid(
                            display,
                            format!("assets[{i}] missing string {name:?}"),
                        )
                    })
            };
            Ok(Asset {
                id: field("id")?,
                path: field("path")?,
                sha256: field("sha256")?,
            })
        })
        .collect()
}

fn parse_derivations(value: &Value, display: &str) -> Result<Vec<Derivation>, ProjectError> {
    // Derivations are optional: a manifest of bare assets with no processing
    // graph resolves bare ids fine (and simply never grows an SRC lane).
    let Some(array) = value.get("derivations") else {
        return Ok(Vec::new());
    };
    let array = array.as_array().ok_or_else(|| {
        ProjectError::manifest_invalid(display, "\"derivations\" is not an array")
    })?;
    array
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let id = d
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    ProjectError::manifest_invalid(
                        display,
                        format!("derivations[{i}] missing string \"id\""),
                    )
                })?;
            let ids = |name: &str| -> Result<Vec<String>, ProjectError> {
                match d.get(name) {
                    None => Ok(Vec::new()),
                    Some(v) => v
                        .as_array()
                        .ok_or_else(|| {
                            ProjectError::manifest_invalid(
                                display,
                                format!("derivations[{i}].{name} is not an array"),
                            )
                        })?
                        .iter()
                        .map(|e| {
                            e.as_str().map(str::to_string).ok_or_else(|| {
                                ProjectError::manifest_invalid(
                                    display,
                                    format!("derivations[{i}].{name} holds a non-string"),
                                )
                            })
                        })
                        .collect(),
                }
            };
            Ok(Derivation {
                id,
                inputs: ids("inputs")?,
                outputs: ids("outputs")?,
            })
        })
        .collect()
}

/// A project-resolution failure, phrased so the message alone diagnoses it.
#[derive(Debug)]
pub enum ProjectError {
    ManifestMissing {
        path: String,
    },
    ManifestUnreadable {
        path: String,
        source: std::io::Error,
    },
    ManifestInvalid {
        path: String,
        reason: String,
    },
    /// A positional that looks like a filesystem path — refused in project mode,
    /// where the positionals are manifest refs.
    RawPath {
        token: String,
    },
    /// A ref that resolved to no asset.
    NoMatch(String),
    /// A ref that resolved to more than one asset.
    Ambiguous(String),
    /// The shared source is ambiguous (more than one candidate asset).
    SourceAmbiguous(String),
}

impl ProjectError {
    fn manifest_invalid(path: &str, reason: impl Into<String>) -> ProjectError {
        ProjectError::ManifestInvalid {
            path: path.to_string(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjectError::ManifestMissing { path } => {
                write!(f, "no project manifest at {path}")
            }
            ProjectError::ManifestUnreadable { path, source } => {
                write!(f, "cannot read the project manifest {path}: {source}")
            }
            ProjectError::ManifestInvalid { path, reason } => {
                write!(f, "invalid project manifest {path}: {reason}")
            }
            ProjectError::RawPath { token } => write!(
                f,
                "{token:?} looks like a path, but --project takes manifest refs \
                 (an asset id, or name@derivation)"
            ),
            ProjectError::NoMatch(msg)
            | ProjectError::Ambiguous(msg)
            | ProjectError::SourceAmbiguous(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ProjectError {}

/// True when an executable named `uncompose-project` is findable on `PATH`. Its
/// absence fails a project launch fast: a session that cannot be registered with
/// the project tool never starts (spec #42).
pub fn uncompose_project_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join("uncompose-project").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_json(value: Value) -> Manifest {
        Manifest {
            id: value["project"]["id"].as_str().unwrap().to_string(),
            assets: parse_assets(&value, "test").unwrap(),
            derivations: parse_derivations(&value, "test").unwrap(),
            root: PathBuf::from("/proj"),
        }
    }

    fn manifest() -> Manifest {
        from_json(serde_json::json!({
            "project": {"id": "01PROJECT", "name": "take", "created_at": "2026-01-01T00:00:00Z"},
            "assets": [
                {"id": "raw", "path": "src/take.wav", "sha256": "a".repeat(64)},
                {"id": "mix-a", "path": "out/vocals.wav", "sha256": "b".repeat(64)},
                {"id": "mix-b", "path": "out/vocals.flac", "sha256": "c".repeat(64)}
            ],
            "derivations": [
                {"id": "deriv-a", "inputs": ["raw"], "outputs": ["mix-a"]},
                {"id": "deriv-b", "inputs": ["raw"], "outputs": ["mix-b"]}
            ]
        }))
    }

    /// A manifest where each reachable ambiguity is present: one derivation
    /// outputs two files sharing the basename-stem `vocals`, and it takes
    /// *both* raws as input — so the two mixes share two possible sources.
    fn ambiguous_manifest() -> Manifest {
        from_json(serde_json::json!({
            "project": {"id": "01AMBIGUOUS", "name": "take", "created_at": "2026-01-01T00:00:00Z"},
            "assets": [
                {"id": "raw-1", "path": "src/take-1.wav", "sha256": "a".repeat(64)},
                {"id": "raw-2", "path": "src/take-2.wav", "sha256": "b".repeat(64)},
                {"id": "mix-a", "path": "out/a/vocals.wav", "sha256": "c".repeat(64)},
                {"id": "mix-b", "path": "out/b/vocals.wav", "sha256": "d".repeat(64)}
            ],
            "derivations": [
                {"id": "mix", "inputs": ["raw-1", "raw-2"], "outputs": ["mix-a", "mix-b"]}
            ]
        }))
    }

    #[test]
    fn a_bare_id_resolves_to_the_asset() {
        let m = manifest();
        let r = m.resolve("raw").unwrap();
        assert_eq!(r.asset.id, "raw");
        assert_eq!(r.path(&m), PathBuf::from("/proj/src/take.wav"));
    }

    #[test]
    fn name_at_derivation_selects_by_basename_stem() {
        let m = manifest();
        // Both derivations output an asset whose stem is "vocals"; the derivation
        // disambiguates which one.
        let a = m.resolve("vocals@deriv-a").unwrap();
        let b = m.resolve("vocals@deriv-b").unwrap();
        assert_eq!(a.asset.id, "mix-a");
        assert_eq!(b.asset.id, "mix-b");
    }

    #[test]
    fn a_raw_path_refuses_in_project_mode() {
        let m = manifest();
        assert!(matches!(
            m.resolve("out/vocals.wav"),
            Err(ProjectError::RawPath { .. })
        ));
    }

    #[test]
    fn a_ref_matching_nothing_is_a_no_match() {
        let m = manifest();
        assert!(matches!(m.resolve("nope"), Err(ProjectError::NoMatch(_))));
        assert!(matches!(
            m.resolve("nope@deriv-a"),
            Err(ProjectError::NoMatch(_))
        ));
        // An unknown derivation is a no-match too, named as such.
        assert!(matches!(
            m.resolve("vocals@nope"),
            Err(ProjectError::NoMatch(_))
        ));
    }

    #[test]
    fn outputs_sharing_a_basename_stem_are_ambiguous_and_listed() {
        // Two outputs of one derivation share the basename-stem `vocals`, which
        // the `<name>@<derivation>` form cannot tell apart. (Duplicate asset
        // ids are not a reachable ambiguity: the id is the manifest's identity
        // and uncompose-project keeps it unique.)
        let m = ambiguous_manifest();
        let Err(ProjectError::Ambiguous(msg)) = m.resolve("vocals@mix") else {
            panic!("two outputs sharing a basename-stem are ambiguous");
        };
        assert!(
            msg.contains("mix-a (out/a/vocals.wav)") && msg.contains("mix-b (out/b/vocals.wav)"),
            "the colliding outputs are listed by id and filename: {msg}"
        );
    }

    #[test]
    fn two_shared_inputs_are_an_ambiguous_source() {
        // Both mixes come out of the one derivation that takes both raws, so the
        // SRC lane has two candidates — refused with the options and the way out.
        let m = ambiguous_manifest();
        let a = m.resolve("mix-a").unwrap();
        let b = m.resolve("mix-b").unwrap();
        let Err(ProjectError::SourceAmbiguous(msg)) = m.shared_source(&a, &b) else {
            panic!("sharing two inputs is an ambiguous source");
        };
        assert!(
            msg.contains("raw-1") && msg.contains("raw-2") && msg.contains("--exclude-source"),
            "the options and the opt-out are named: {msg}"
        );
    }

    #[test]
    fn shared_source_is_the_common_input() {
        let m = manifest();
        let a = m.resolve("vocals@deriv-a").unwrap();
        let b = m.resolve("vocals@deriv-b").unwrap();
        let src = m.shared_source(&a, &b).unwrap().expect("a shared source");
        assert_eq!(src.id, "raw");
    }

    #[test]
    fn no_shared_source_is_absent_not_an_error() {
        // `raw` has no producing derivation, so pairing it with either mix shares
        // no producer input — the SRC lane is simply absent.
        let m = manifest();
        let raw = m.resolve("raw").unwrap();
        let a = m.resolve("mix-a").unwrap();
        let b = m.resolve("mix-b").unwrap();
        assert!(m.shared_source(&raw, &a).unwrap().is_none());
        assert!(m.shared_source(&raw, &b).unwrap().is_none());
    }
}
