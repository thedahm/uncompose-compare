# Sync harness (issue #73 → #5), run against the packaged binary

Copied (not submoduled) from `thedahm/uncompose` branch
`wayfinder/73-sync-spike` and re-pointed at the **pip-installed
`uncompose-compare` binary's served page**, closing spike #5 of the Track C
packaging pipeline (spec #1).

## What changed from the reference spike

The reference #73 spike rendered the whole Web Audio graph inside
`page.evaluate` on `about:blank` — it tested the *contract*, not any shipped
page. Here:

- The dual-source crossfade plumbing lives in the **served page**
  (`frontend/src/sync.ts`, exposed as `window.__uncomposeSync`), so the thing
  under test is what the wheel actually ships.
- `global-setup.mjs` launches the packaged binary (from `UNCOMPOSE_BIN`, else
  `uncompose-compare` on PATH — **never** `cargo run`), captures its tokened
  loopback URL, and every engine drives that page.
- The spec navigates that URL (the `?token=` seeds the #72 cookie the bundle
  request carries), hands the page the seeded-noise fixtures, and asserts the
  three contract points on what the page renders.

## What it proves

On Chromium, Firefox, and WebKit:

1. **Zero-offset render** — cross-correlation of the post-switch output against
   the expected candidate peaks at exactly lag 0.
2. **Decode-count canary** — WAV and FLAC fixtures decode to exactly 66150
   frames, 44100 Hz, 2 channels.
3. **Crossfade bound** — output is float-bit-identical to the live candidate
   outside the 10 ms fade window.

## Run it locally

```sh
# 1. Build + pip-install the wheel into a fresh venv (from the repo root):
bash scripts/pipeline.sh
#    ...or install any wheel and put its bin on PATH.

# 2. Point the harness at that installed binary and run the matrix:
cd harness
npm install
npx playwright install --with-deps chromium firefox webkit
node gen-fixtures.mjs                       # FLAC via the bundled ffmpeg-static
UNCOMPOSE_BIN=/path/to/venv/bin/uncompose-compare npx playwright test
```

`scripts/sync-harness.sh` wires steps 1–2 together the way CI runs them.

## Notes

- Fixtures are deterministic seeded noise (`gen-fixtures.mjs`); noise, not sine,
  so the correlation peak is unambiguous. FLAC is encoded by the bundled
  `ffmpeg-static` binary (a devDependency), so fixture generation needs no
  system ffmpeg or apt; it stays build-time only and never ships in the wheel.
- WebKit needs extra system libs off-Ubuntu; on `ubuntu-latest`
  `npx playwright install --with-deps` covers it (the #73 finding). CI pins the
  matrix to `ubuntu-latest` for exactly this reason.
