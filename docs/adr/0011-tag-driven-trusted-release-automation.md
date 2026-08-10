# ADR-0011: Tag-driven trusted release automation

Status: Accepted — 2026-08-10

The packaging chain has existed since the Track C spike (spec #1): Vite bundle →
rust-embed → maturin wheel → clean-install proof. What it has never had is a way to
*publish*. M6 (uncompose#92, stories 21–24; slice uncompose#97) closes that:
`uncompose-compare` v0.1.0 goes to PyPI, and the way it goes has to be one auditable
action a maintainer can trust six months later, holding no credential worth stealing.
The family constraint is uncompose ADR-0008, which fixes the shape for every family
package; this ADR records what this repo does with it.

## A `vX.Y.Z` tag is the only thing that publishes

`release.yml` triggers on the tag pattern and runs the release end to end — version
check, gates, build, publish — as a single run, so the audit trail is one URL rather
than a correlation exercise across four. Publishing from a manual run against the real
index was rejected: a release should be a fact in the repository's history, not an act
somebody performed.

## Cargo.toml is the version, and a tag is a claim about it

`pyproject.toml` used to declare `version = "0.1.0"` alongside Cargo's, which meant two
places to get right and no answer to "which one is the package version?" when they
disagreed. It now declares `dynamic = ["version"]`, so maturin takes the version from
`[package] version` in Cargo.toml and there is exactly one number.

`scripts/release-version.sh` reads that number and refuses a tag that disagrees, in the
first job, before anything is built. `scripts/check-wheel.sh` then checks the
*artifact* — distribution, version, manylinux tag — so however the build got its
version, a wheel that isn't the one the tag names never reaches the index. Both guards
are shell and their test (`scripts/release-checks.test.sh`) drives them as processes,
asserting exit codes and messages: the process seam this repo tests everything at.
Nothing is scaffolded around Actions internals.

The same edit removed the `Private :: Do Not Upload` classifier, which was truthful
while this was a spike and would now make PyPI reject the upload, and replaced the
spike's description with the tool's.

## Prerelease tags are refused, not translated

Cargo's semver prereleases (`0.1.0-rc.1`) and PEP 440's (`0.1.0rc1`) do not spell the
same version the same way, so a check that accepted them would have to invent a mapping
and could no longer say honestly that the tag and the package version agree. Rehearsals
publish to TestPyPI from a manual run of the same workflow (`docs/releasing.md`) — one
pipeline with a different index, rather than a second, differently-shaped path to
production.

## The gate is the repo's own CI

`release.yml` calls `ci.yml` and `packaging.yml` with `workflow_call` instead of
restating them. Both matter here: `ci.yml` is the frontend and Rust suites, and
`packaging.yml` is the clean-install proof and the three-engine sync harness — the
gates that exist precisely because this wheel carries a browser UI inside it. Restating
them would let "the release ran the full suite" drift from what the suite is.

## Trusted Publishing (OIDC), with a GitHub environment

`pypa/gh-action-pypi-publish` exchanges a short-lived OIDC token for upload rights. No
API token exists in this repository or on the release path, so there is none to leak,
rotate, or scope wrong. The publish job runs in the `pypi` environment (`testpypi` for
rehearsals) and holds `id-token: write`; every other job holds `contents: read`. The
environment is part of the identity PyPI trusts, so it is also where required reviewers
would go if a release should ever need a second pair of eyes.

PEP 740 attestations are on: signed with the same OIDC identity, they make the published
wheel name the repository, workflow, and commit that produced it. Story 23 asks for
traceability that is verifiable rather than asserted, and this is the form a stranger
can check from the index alone.

## One Linux wheel, built in the manylinux container, and no sdist

The v0.1 platform scope is Linux x86_64, so there is no matrix. The release build goes
through `PyO3/maturin-action` with `manylinux: auto`, as uncompose's own release
workflow does, rather than `scripts/build-wheel.sh`'s plain `maturin build`: on the
runner the binary is tagged against the runner's glibc, which quietly excludes every
machine older than it. The frontend bundle is still built on the runner first — the
container compiles a workspace that already has `frontend/dist` in it, so rust-embed
finds what `build.rs` insists on. `build-wheel.sh` keeps the simpler build for local and
CI use, where the point is that packaging works, not the artifact anyone installs.

No sdist is published. Here this repo departs from uncompose's release, which publishes
one: an sdist of this crate could not be built at all, because it would carry no
`frontend/dist` and `build.rs` refuses to embed an absent bundle. Even if it could, it
would ask a user's machine for a Rust toolchain *and* Node — the two things the
embedding chain exists to keep out of the install story — on platforms this release does
not support, where the absence of a wheel is the clearer answer.

## The wheel that was proven is the wheel that is published

The build job runs `scripts/clean-install-proof.sh` against the artifact it just built
and uploads that artifact; `publish` downloads exactly it and never rebuilds.

## Consequences

- Cutting a release is: bump the version in Cargo.toml, land it, push the tag.
  Everything it refuses, it refuses before publishing rather than after.
- PyPI and TestPyPI must be told which workflow they trust — a one-time human step per
  index, recorded in `docs/releasing.md`. Until it exists, `publish` fails with no
  token; the wheel is still built and proven.
- Renaming `release.yml`, or the environments, breaks publishing until the trusted
  publisher is updated to match. That is the mechanism working: the workflow's identity
  is part of the credential.
- A repeated TestPyPI rehearsal of the same version is refused by the index. The remedy
  is a version bump on the rehearsal branch, not a workaround in the pipeline.
