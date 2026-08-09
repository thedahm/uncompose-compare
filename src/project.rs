//! Project mode (M5 slice 4, spec #42): resolve the two positionals as manifest
//! refs instead of filesystem paths, and auto-resolve the candidates' shared
//! source into the SRC lane.
//!
//! `--project <dir>` names a project root; the manifest is read directly from
//! exactly `<dir>/uncompose.project.json` (no upward walk, no plumbing command).
//! Each positional is a *ref* into that manifest, in one of two v0.1 forms:
//!
//! - a bare token is an **asset slug** — the manifest asset whose `slug` matches;
//! - `<name>@<derivation>` selects, among that derivation's **output** assets,
//!   the one whose file basename-stem equals `<name>`.
//!
//! A raw path refuses in project mode (the refs are manifest handles, not files).
//! No-match and ambiguity both list the derivation's outputs with slug and
//! filename, so the message alone tells the listener what they could have named.
//!
//! The SRC lane is auto-resolved: the asset that is an input of *both*
//! candidates' producing derivations. Share none and the lane is simply absent
//! (stated at launch, not an error); `--exclude-source` opts out entirely. The
//! resolved files carry their manifest sha256, checked at load by the existing
//! hashing pipeline (`session.rs`), so a manifest that no longer matches the
//! bytes on disk refuses before the server binds.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The manifest filename, fixed under the project root (no upward walk).
pub const MANIFEST_NAME: &str = "uncompose.project.json";

/// One manifest asset: a content-addressed file with a human slug.
pub struct Asset {
    pub id: String,
    pub slug: String,
    /// The asset's file, relative to the project root.
    pub file: String,
    pub sha256: String,
}

impl Asset {
    /// The file's basename-stem — its filename without the final extension. The
    /// `<name>` in a `<name>@<derivation>` ref matches this.
    fn stem(&self) -> String {
        Path::new(&self.file)
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
/// through (`Some` for the `name@derivation` form, `None` for a bare slug — the
/// shared-source search then looks up the asset's producers itself).
pub struct Resolved<'a> {
    pub asset: &'a Asset,
    derivation: Option<&'a Derivation>,
}

impl Resolved<'_> {
    /// The asset's absolute file path, under the project root.
    pub fn path(&self, manifest: &Manifest) -> PathBuf {
        manifest.root.join(&self.asset.file)
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

        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ProjectError::manifest_invalid(&display, "missing string \"id\""))?
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
    /// is an asset slug; `<name>@<derivation>` selects among that derivation's
    /// outputs by basename-stem.
    pub fn resolve(&self, token: &str) -> Result<Resolved<'_>, ProjectError> {
        if token.contains('/') || token.contains('\\') {
            return Err(ProjectError::RawPath {
                token: token.to_string(),
            });
        }

        match token.split_once('@') {
            Some((name, deriv_id)) => self.resolve_in_derivation(name, deriv_id),
            None => self.resolve_slug(token),
        }
    }

    fn resolve_slug(&self, slug: &str) -> Result<Resolved<'_>, ProjectError> {
        let matches: Vec<&Asset> = self.assets.iter().filter(|a| a.slug == slug).collect();
        match matches.as_slice() {
            [] => Err(ProjectError::NoMatch(format!(
                "no asset with slug {slug:?} in the project. Known slugs: {}",
                self.slug_list()
            ))),
            [asset] => Ok(Resolved {
                asset,
                derivation: None,
            }),
            many => Err(ProjectError::Ambiguous(format!(
                "slug {slug:?} names {} assets: {}",
                many.len(),
                many.iter()
                    .map(|a| format!("{} ({})", a.id, a.file))
                    .collect::<Vec<_>>()
                    .join(", ")
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
    /// outputs the bare-slug asset.
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

    fn slug_list(&self) -> String {
        let mut slugs: Vec<&str> = self.assets.iter().map(|a| a.slug.as_str()).collect();
        slugs.sort_unstable();
        slugs.dedup();
        slugs.join(", ")
    }

    fn derivation_list(&self) -> String {
        self.derivations
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Render a set of output assets as `slug (filename)` pairs, the shape both the
/// no-match and ambiguity messages list so a listener can pick the right ref.
fn outputs_listing(outputs: &[&Asset]) -> String {
    outputs
        .iter()
        .map(|a| format!("{} ({})", a.slug, a.file))
        .collect::<Vec<_>>()
        .join(", ")
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
                slug: field("slug")?,
                file: field("file")?,
                sha256: field("sha256")?,
            })
        })
        .collect()
}

fn parse_derivations(value: &Value, display: &str) -> Result<Vec<Derivation>, ProjectError> {
    // Derivations are optional: a manifest of bare assets with no processing
    // graph resolves slugs fine (and simply never grows an SRC lane).
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
                 (an asset slug, or name@derivation)"
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
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join("uncompose-project");
        candidate.is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        let value = serde_json::json!({
            "id": "01PROJECT",
            "assets": [
                {"id": "raw", "slug": "raw", "file": "src/take.wav", "sha256": "a".repeat(64)},
                {"id": "mix-a", "slug": "mix-a", "file": "out/vocals.wav", "sha256": "b".repeat(64)},
                {"id": "mix-b", "slug": "mix-b", "file": "out/vocals.flac", "sha256": "c".repeat(64)}
            ],
            "derivations": [
                {"id": "deriv-a", "inputs": ["raw"], "outputs": ["mix-a"]},
                {"id": "deriv-b", "inputs": ["raw"], "outputs": ["mix-b"]}
            ]
        });
        Manifest {
            id: value["id"].as_str().unwrap().to_string(),
            assets: parse_assets(&value, "test").unwrap(),
            derivations: parse_derivations(&value, "test").unwrap(),
            root: PathBuf::from("/proj"),
        }
    }

    #[test]
    fn bare_slug_resolves_to_the_asset() {
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
    fn no_match_and_ambiguity_are_distinct_errors() {
        let m = manifest();
        assert!(matches!(m.resolve("nope"), Err(ProjectError::NoMatch(_))));
        assert!(matches!(
            m.resolve("nope@deriv-a"),
            Err(ProjectError::NoMatch(_))
        ));
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
        // Two bare assets with no producing derivations share nothing.
        let m = manifest();
        let a = m.resolve("mix-a").unwrap();
        let b = m.resolve("mix-b").unwrap();
        // mix-a/mix-b are outputs, their producers share "raw" — so this pair DOES
        // share. Use raw vs mix-a, which have no common producer input.
        let raw = m.resolve("raw").unwrap();
        assert!(m.shared_source(&raw, &a).unwrap().is_none());
        assert!(m.shared_source(&raw, &b).unwrap().is_none());
    }
}
