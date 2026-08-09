// Playwright global setup: launch the *pip-installed* uncompose-compare binary
// and capture the tokened loopback URL it prints, so every engine in the matrix
// drives the same served page — the one the wheel actually ships (issue #5,
// acceptance: "harness target is the pip-installed binary").
//
// The binary under test is resolved from UNCOMPOSE_BIN (the clean-install
// pipeline points this at $VENV/bin/uncompose-compare); it falls back to
// `uncompose-compare` on PATH for a locally-installed wheel. We deliberately do
// NOT fall back to `cargo run`: the whole point is to exercise the packaged
// artifact.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { spawnServed } from "./serve.mjs";

const dir = path.dirname(fileURLToPath(import.meta.url));
export const URL_FILE = path.join(dir, ".served-url");
export const PID_FILE = path.join(dir, ".server-pid");

export default async function globalSetup() {
  const bin = process.env.UNCOMPOSE_BIN || "uncompose-compare";

  // The binary is the two-file compare command (issue #10): it loads exactly
  // two audio files as candidates A and B before serving. Point it at the
  // seeded-noise fixtures gen-fixtures.mjs writes (run before Playwright in
  // sync-harness.sh). The harness drives the served page's `window.__uncomposeSync`
  // seam, not these particular candidates, but the binary needs them to launch.
  const fixtures = path.join(dir, "fixtures");
  const args = [path.join(fixtures, "a.wav"), path.join(fixtures, "b.wav")];
  const { child, url: served } = spawnServed(bin, args);
  const url = await served;

  writeFileSync(URL_FILE, url);
  writeFileSync(PID_FILE, String(child.pid));
  // Let the process outlive setup; global-teardown kills it by pid.
  child.unref();
  console.log(`[harness] serving packaged binary at ${url}`);
}
