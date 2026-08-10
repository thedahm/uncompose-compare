# Releasing `uncompose-compare`

A release is one push of a `vX.Y.Z` tag. Everything after that — gates, wheel, PyPI —
is [`.github/workflows/release.yml`](../.github/workflows/release.yml), and nothing
else publishes. The reasoning behind that shape is
[ADR-0011](adr/0011-tag-driven-trusted-release-automation.md).

## One-time setup (a human with PyPI access)

The workflow holds no credentials, so PyPI has to be told which workflow it trusts.
On <https://pypi.org/manage/project/uncompose-compare/settings/publishing/>, add a
GitHub publisher:

| Field            | Value               |
| ---------------- | ------------------- |
| Owner            | `thedahm`           |
| Repository       | `uncompose-compare` |
| Workflow name    | `release.yml`       |
| Environment name | `pypi`              |

Repeat on <https://test.pypi.org> with the environment name `testpypi` (register the
project there first if it does not exist, via a pending publisher with the same four
fields). The two environments already exist in the repository's settings with no
protection rules; adding required reviewers to `pypi` later turns a release into a
two-person action without touching the workflow.

No API token is ever created. If one exists on the account from an earlier hand-rolled
release, revoke it — this pipeline cannot use it and nothing else should.

## Rehearsing

Actions → **release** → *Run workflow*, from any branch:

- `index: none` (the default) runs every job except `publish`: the tag check, the CI
  and packaging suites, the wheel build, the wheel checks, and the toolchain-free
  clean-install proof.
- `index: testpypi` also publishes the built wheel to TestPyPI.

Rehearse from a branch that carries the version you intend to ship. TestPyPI refuses a
version it has already seen, so a second rehearsal of the same number needs a version
bump on the branch — which is the truthful failure, not an obstacle to work around.

## Cutting a release

1. Set the version in `Cargo.toml` (`[package] version`) and commit the `Cargo.lock`
   update with it. That is the only place a version is written; `pyproject.toml` takes
   it from there.
2. Land it on `main` and let CI go green.
3. Tag the merge commit and push the tag:

   ```sh
   git tag -a v0.1.0 -m "uncompose-compare v0.1.0"
   git push origin v0.1.0
   ```

4. Watch the run. It publishes to PyPI on its own; there is no approval step unless one
   is added to the `pypi` environment.

## What refuses, and why

- **The tag disagrees with `Cargo.toml`** — `scripts/release-version.sh` fails the first
  job. Fix the version or move the tag; nothing was built, and nothing was published.
- **Not `vX.Y.Z`** — prerelease and dev tags are refused rather than translated, because
  Cargo's semver prereleases and PEP 440's do not spell the same version the same way.
  Rehearse on TestPyPI instead.
- **CI or packaging fails** — the gate jobs *are*
  [`ci.yml`](../.github/workflows/ci.yml) and
  [`packaging.yml`](../.github/workflows/packaging.yml), so a tag that does not pass the
  frontend, Rust, clean-install, and sync-harness suites cannot ship.
- **The wheel is not the one the tag names, or is not manylinux** —
  `scripts/check-wheel.sh` fails the build job. PyPI rejects bare `linux_x86_64` wheels,
  so this catches at build time what would otherwise be a tag with no release behind it.

Both guards are tested by `scripts/release-checks.test.sh`, which CI runs on every
change.

## After a release

- `pip install uncompose-compare==X.Y.Z` in a clean environment with no Node and no
  Rust; `uncompose-compare a.wav b.wav` serves the workbench, and with the root CLI
  installed so does `uncompose compare a.wav b.wav`.
- The PyPI page shows the release as *Verified* with attestations naming this
  repository, `release.yml`, and the commit the tag points at. That is the traceability
  claim, checkable by anyone.
- The family-wide steps for a coupled release — refreshing the website's schema copies,
  the docs-alone checklist — live in the
  [`thedahm/uncompose`](https://github.com/thedahm/uncompose) release checklist.
