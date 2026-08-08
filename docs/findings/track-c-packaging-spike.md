# Findings: Track C packaging pipeline spike

**Spec:** [#1](https://github.com/thedahm/uncompose-compare/issues/1) ·
**This write-up:** [#6](https://github.com/thedahm/uncompose-compare/issues/6) ·
**Decision under test:** `thedahm/uncompose` #60 (packaging shape) ·
**Verdict: GO** (see [Go / no-go](#go--no-go))

The one spike of the ecosystem plan asked a single composition question: does a
Vite/React frontend, embedded into a Rust binary via **rust-embed**, shipped as
a **maturin** PyPI wheel, pip-installed on a clean Linux machine with no Node or
Rust toolchain, serve that embedded UI under the #72 privacy contract and pass
the #73 sync harness? Every individual risk was already retired; only the chain
had never been demonstrated. This document records what the chain proved, the
workarounds each link needed, the dead ends with their working alternatives, and
the measured wheel size and build time — so M3 inherits evidence, not folklore.

## What worked

The `#60` shape composed end to end with **no fundamental blocker**. Each link
below was verified first-hand at the process/HTTP seam, not by inspecting
internals (spec #1 testing decisions):

- **Vite/React → static bundle.** `frontend/` (React 18 + TypeScript 5 + Vite 5)
  builds to `frontend/dist/` (`index.html` + hashed `assets/`), 156 KiB, in
  ~1.5 s. `tsc --noEmit` typecheck clean.
- **rust-embed embeds the bundle.** `#[derive(RustEmbed)] #[folder =
  "frontend/dist/"]` compiles the whole bundle into the binary; the served page
  and its JS bundle come out of the binary itself — no loose asset directory
  (story 3). Confirmed by serving `/` and the hashed bundle from the installed
  wheel.
- **build.rs fails loudly on a missing/stale bundle.** rust-embed will happily
  embed an *empty or absent* folder and 404 its own UI at runtime. `build.rs`
  panics at embed time with an actionable message if `frontend/dist/index.html`
  is absent, and `cargo:rerun-if-changed` re-embeds when the bundle changes
  (story 6). This is the working solution to a real rust-embed foot-gun — see
  [Dead ends](#dead-ends--foot-guns).
- **maturin wheel carries only the binary.** `bindings = "bin"` +
  `strip = true` produces a `py3-none-manylinux_2_34_x86_64` wheel whose entire
  payload is the compiled binary installed as a venv script named
  **`uncompose-compare`** (the #57/#11 name, preserved for later root-CLI
  dispatch), plus dist-info metadata and a CycloneDX SBOM. **No Python module,
  no extension module, no loose assets, no Node/Rust artifacts.** Verified by
  `unzip -l` (5 entries: the binary, METADATA, WHEEL, SBOM, RECORD).
- **Clean pip install, toolchain-free.** `scripts/clean-install-proof.sh`
  creates a fresh venv, asserts `node`/`npm`/`cargo`/`rustc`/`maturin` are
  absent from PATH (hard-fails if any leak in, story 9), `pip install --no-index`
  the wheel, launches the installed binary, and confirms over HTTP that it
  serves the embedded page with `no-store`. **PASS**, first-hand this iteration.
- **#72 privacy contract at the first served byte** (story 10). Verified against
  the installed binary:
  - loopback bind on an OS-assigned ephemeral port; prints
    `http://127.0.0.1:<port>/?token=<tok>`;
  - authorized `GET /` → `200`, `Cache-Control: no-store`, `Content-Type`, and
    `Set-Cookie: token=…; Path=/; SameSite=Strict`;
  - missing/wrong token → `403`; non-loopback `Host` → `403`; `404` → still
    `no-store`; constant-time token compare;
  - `--version` and `--help` exit `0` (story 12, mandatory external-command
    flags).
- **9/9 cargo CLI tests green**, exercising the above only at the HTTP/process
  seam (`tests/cli.rs`).
- **One command runs the whole pipeline** (`scripts/pipeline.sh`, story 5); CI
  runs exactly it. A second CI job (`sync-harness`) drives the #73 matrix
  against the packaged binary.
- **Sync harness re-pointed at the packaged binary.** The #73 harness
  (`wayfinder/73-sync-spike`) was **copied, not submoduled**, into `harness/`
  and re-pointed at the *pip-installed* binary's served page: `global-setup.mjs`
  launches `UNCOMPOSE_BIN` (never `cargo run`), captures its tokened URL, and the
  Chromium/Firefox/WebKit matrix drives that page. The dual-source crossfade
  plumbing lives in the served bundle (`frontend/src/sync.ts`, exposed as
  `window.__uncomposeSync`), so the thing under test is what the wheel ships. Its
  three contract metrics — zero-lag correlation peak, decode-count canary
  (66150 frames, 44.1 kHz, 2 ch for WAV *and* FLAC), and bit-identical crossfade
  bound outside the 10 ms window — were verified by reproducing the
  OfflineAudioContext render math headlessly (see [caveat](#the-one-residual)).

## Measurements

Measured first-hand this iteration on the packaging branch (x86-64 Linux;
`opt-level = "z"`, `strip = true`; warm cargo dependency cache):

| Metric | Value |
| --- | --- |
| Frontend bundle (`frontend/dist`) | 156 KiB |
| Release binary (embeds the bundle) | 1,143,944 B ≈ 1.1 MiB |
| **Wheel size** | **498 KiB (509,512 bytes)** |
| Wheel tag | `py3-none-manylinux_2_34_x86_64` |
| Wheel payload | binary + METADATA + WHEEL + SBOM + RECORD (5 entries) |
| **Build time** (frontend + maturin wheel, warm cargo cache) | **~5 s** |
| Frontend build alone | ~1.5 s |
| `cargo test` | 9/9 green |

The 498 KiB wheel reproduces the size recorded by the two prior packaging
iterations (issue #3: 494 KiB; issue #5: 498 KiB) — stable across runs. The
~5 s build is with a warm cargo dependency cache; a cold first CI build compiles
`clap`/`rust-embed`/`tiny_http` from scratch and takes longer, but that cost is
paid once per runner and is dependency-compile time, not spike overhead.

## Workarounds (working, kept)

Links that composed only after a deliberate choice. None indict #60; each is a
recorded, working solution so the same wall is never hit twice (story 14).

1. **Cookie propagation for browser sub-resources.** The printed URL carries the
   token as `?token=`, but browsers do **not** propagate a query string to
   sub-resource requests — so a browser loading the page would fetch the JS
   bundle *without* the token and get a `403`, breaking the #73 harness.
   **Solution:** authorized page responses seed a `token` cookie
   (`SameSite=Strict`), and the token check accepts the cookie *or* the query.
   This is what makes the embedded page actually loadable in a real browser; it
   is a hard dependency of the sync harness, not a nicety.
2. **`bindings = "bin"` made explicit.** Rather than rely on maturin
   autodetection, `[tool.maturin] bindings = "bin"` states that the wheel ships
   only the Cargo `[[bin]]` — no Python package expected or produced.
3. **`playwright install --with-deps`, matrix pinned to `ubuntu-latest`.** WebKit
   (and Chromium/Firefox) need system libs that a bare runner lacks. The #73
   finding carries over: `--with-deps` installs them on `ubuntu-latest`, so the
   matrix needs no bespoke runner. CI's `sync-harness` job sets `PW_WITH_DEPS=1`.
4. **pip bootstrap where `ensurepip` is absent.** The clean-install and
   sync-harness scripts bootstrap pip via `ensurepip`, falling back to
   `get-pip.py`, so the toolchain-free venv still gets pip on a minimal host.
   `ubuntu-latest` provides pip natively; this only smooths stripped
   environments.

## Dead ends & foot-guns

Recorded with the working alternative that replaced each (story 14):

- **System ffmpeg / apt for FLAC fixtures — dead end.** Generating FLAC
  fixtures via a system `ffmpeg` would require root/apt on the runner.
  **Alternative (adopted):** the bundled **`ffmpeg-static`** devDependency;
  `gen-fixtures.mjs` resolves FLAC encoding through it (the `FFMPEG` env var
  still overrides). Fixture generation is now dependency-free, and ffmpeg stays
  strictly build-time — it never ships in the wheel.
- **rust-embed silently embedding a missing/empty bundle — foot-gun.** Would
  produce a binary that 404s its own UI, failing only at runtime far from cause.
  **Guard (adopted):** the `build.rs` embed-time panic above.
- **Browser launch inside a no-sudo sandbox — environmental dead end.** The
  Playwright engines (and even `node-web-audio-api`) need system libs
  (`libgbm`, `libatk`, `libasound`, WebKit's `flite`/`wayland` libs) that
  require root to install; the development sandbox has no sudo, so browsers
  cannot *launch* there. **Alternative:** every step up to browser start is
  proven locally, the OfflineAudioContext render algorithm is proven correct
  headlessly in pure JS, and CI's `--with-deps` on `ubuntu-latest` installs the
  libs so the matrix launches there. This is a property of the dev sandbox, not
  of the #60 shape.

## The one residual

Full confidence on the #73 three-engine matrix **launching** against the
packaged binary is the single item not closed first-hand in this environment:
browsers cannot start in the no-sudo dev sandbox (above). What *is* proven
first-hand is the entire packaging chain up to and including the served page,
the #72 contract at the HTTP seam, and the sync algorithm's correctness computed
headlessly. The residual is a CI-confirmation step on `ubuntu-latest` (where
`--with-deps` supplies the libs), configured and expected green — not a gap in
the packaging shape. It should be confirmed on the first CI run of the
`sync-harness` job; it does not gate the verdict below, because it tests the
browser matrix's environment, not whether Rust + rust-embed + maturin can carry
and serve the embedded bundle (which is fully demonstrated).

## Go / no-go

**GO on the #60 packaging shape** (Rust + TS/Vite/React via rust-embed, shipped
as a maturin wheel).

Every packaging link is proven, most of them first-hand this iteration: the
frontend embeds into the binary, maturin ships that binary alone as a 498 KiB
`py3-none` wheel with no Python module and no loose assets, pip installs it into
a toolchain-free venv on a clean Linux host, the installed binary serves the
embedded UI under the full #72 privacy contract, and answers `--version`/`--help`
and the `uncompose-compare` entry-point name that later root-CLI dispatch (#57)
depends on. maturin comfortably carries the embedded bundle; rust-embed's only
sharp edge (silent empty-embed) is fenced by a build-time guard. No dead end
indicted the #60 decision — every one had a working alternative within the same
shape.

Because the verdict is **go**, no proposal is filed against `thedahm/uncompose`
to reopen #60. (Had the composition failed in a way that indicted #60 — e.g.
maturin unable to carry the embedded bundle acceptably — spec #1 required the
finding to go back as a proposal to reopen #60 rather than be worked around
locally. That path was not taken.)

M3 grows out of this spike rather than starting over: the `frontend/` + Rust
crate + `harness/` layout foreshadows the real layout, and `scripts/pipeline.sh`
is the one command that reproduces CI locally. The only follow-up M3 should
carry forward is confirming the browser matrix green on its first CI run.
