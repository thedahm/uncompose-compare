// Cross-engine sync harness (issue #66, spike #73), copied from
// thedahm/uncompose `wayfinder/73-sync-spike` and re-pointed at the *packaged
// binary's served page* (issue #5).
//
// The reference spike ran the whole Web Audio graph inside `page.evaluate` on
// about:blank. Here the graph plumbing lives in the served page (frontend
// `sync.ts`, exposed as `window.__uncomposeSync`), and this spec:
//   1. navigates the wheel-installed binary's tokened URL (from global-setup),
//   2. hands the page the seeded-noise fixtures as base64,
//   3. asserts the three contract points on what the page renders.
// So packaging is proven not to break the sync contract the harness certifies.
import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { URL_FILE } from "./global-setup.mjs";

const dir = path.dirname(fileURLToPath(import.meta.url));
const b64 = (f) => readFileSync(path.join(dir, "fixtures", f)).toString("base64");
const FIXTURES = {
  "a.wav": b64("a.wav"),
  "b.wav": b64("b.wav"),
  "a.flac": b64("a.flac"),
  "b.flac": b64("b.flac"),
};

const SR = 44100;
const FRAMES = 66150;
const SWITCH_SAMPLE = 30870; // 0.7 s
const FADE_SAMPLES = 441; // 10 ms

const SERVED_URL = readFileSync(URL_FILE, "utf8").trim();

let result;
test.beforeAll(async ({ browser }) => {
  const page = await browser.newPage();
  // Load the packaged binary's served page. The `?token=` seeds the cookie the
  // page's sub-resource (bundle) request then carries — the #72 contract.
  await page.goto(SERVED_URL, { waitUntil: "load" });
  // The bundle registers the harness seam on load; wait for it, then drive it.
  await page.waitForFunction(() => typeof window.__uncomposeSync === "function");
  const t0 = Date.now();
  result = await page.evaluate(
    (args) => window.__uncomposeSync(args),
    { fixtures: FIXTURES, SR, FRAMES, SWITCH_SAMPLE, FADE_SAMPLES },
  );
  result.elapsedMs = Date.now() - t0;
  await page.close();
});

test("decode-count canary: FLAC and WAV decode to exact sample counts", () => {
  console.log(JSON.stringify(result, null, 2));
  for (const name of ["a.wav", "b.wav", "a.flac", "b.flac"]) {
    const info = result.decodeInfo[name];
    expect(info.error, `${name} decode error`).toBeUndefined();
    expect(info.length, `${name} sample count`).toBe(FRAMES);
    expect(info.sampleRate, `${name} sample rate`).toBe(SR);
    expect(info.channels, `${name} channels`).toBe(2);
  }
});

test("zero-offset render: cross-correlation peak at lag 0", () => {
  expect(result.renderSkipped).toBeFalsy();
  expect(result.correlation.bestLag).toBe(0);
  expect(result.correlation.peakRatio).toBeGreaterThan(2);
});

test("crossfade bound: bit-identical output outside the 10 ms window", () => {
  expect(result.renderSkipped).toBeFalsy();
  for (const id of result.identity) {
    expect(
      id.preMismatch,
      `ch${id.ch} pre-switch mismatches (first at ${id.firstPre}, maxDiff ${id.maxDiff})`,
    ).toBe(0);
    expect(
      id.postMismatch,
      `ch${id.ch} post-fade mismatches (first at ${id.firstPost}, maxDiff ${id.maxDiff})`,
    ).toBe(0);
  }
});
