// Blind + loudness-match composed flow spec (issue #32, spec #26), driven against
// the packaged binary's served page — the same page the sync matrix, the sighted
// workbench flow (issue #5), and the blind flow (issue #29) use.
//
// `--blind --loudness-match` is a level-fair blind test: the workbench reports
// only that matching is active, and every per-lane loudness figure (measured LUFS
// and gain) stays hidden until the conclude reveal — a distinctive figure would
// fingerprint a candidate. This spec certifies the observable composed slice:
// the matching-active indicator shows with no LUFS/gain number anywhere in the
// pre-conclude DOM, and only after the record is written does the reveal show the
// loudness figures alongside the label→file mapping the record on disk carries.
// Chromium only, like the blind flow (the composed surface is engine-independent;
// the cross-engine contract stays in sync.spec.mjs).
import { test, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import path from "node:path";

const dir = path.dirname(fileURLToPath(import.meta.url));

test.skip(
  ({ browserName }) => browserName !== "chromium",
  "blind loudness flow runs on Chromium; the sync contract keeps the full matrix",
);

// Its own `--blind --loudness-match` launch (global-setup runs a plain sighted
// binary), writing its record into a throwaway cwd the spec owns. The fixture
// pair (a.wav/b.wav) is same duration/rate/channels, different content — exactly
// what blind mode accepts, and loudness matching measures each lane at load.
const bin = process.env.UNCOMPOSE_BIN || "uncompose-compare";
const fixtures = path.join(dir, "fixtures");
const inputs = { a: path.join(fixtures, "a.wav"), b: path.join(fixtures, "b.wav") };

let child;
let servedUrl;
let recordDir;

test.beforeAll(async () => {
  recordDir = mkdtempSync(path.join(tmpdir(), "blind-loudness-flow-"));
  child = spawn(bin, [inputs.a, inputs.b, "--blind", "--loudness-match"], {
    cwd: recordDir,
    stdio: ["ignore", "pipe", "inherit"],
  });
  servedUrl = await new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`${bin} --blind --loudness-match printed no URL within 15s`)),
      15_000,
    );
    child.on("error", (err) =>
      reject(new Error(`failed to launch ${bin} --blind --loudness-match: ${err.message}`)),
    );
    child.on("exit", (code) =>
      reject(new Error(`${bin} --blind --loudness-match exited (code ${code}) before a URL`)),
    );
    let buf = "";
    child.stdout.on("data", (chunk) => {
      buf += chunk.toString();
      const nl = buf.indexOf("\n");
      if (nl === -1) return;
      clearTimeout(timer);
      const line = buf.slice(0, nl).trim();
      if (!line.startsWith("http://127.0.0.1:")) reject(new Error(`expected a loopback URL, got: ${line}`));
      else resolve(line);
    });
  });
});

test.afterAll(() => {
  if (child?.pid) child.kill("SIGKILL");
});

test.beforeEach(async ({ page }) => {
  await page.goto(servedUrl, { waitUntil: "load" });
  await page.getByTestId("workbench").waitFor({ state: "visible", timeout: 30_000 });
});

test("matching is announced but every per-lane figure is hidden before conclude", async ({ page }) => {
  // The indicator states matching is active and that the figures are withheld;
  // the per-lane LUFS/gain spans a sighted session shows are absent.
  const indicator = page.getByTestId("loudness-match");
  await expect(indicator).toBeVisible();
  await expect(indicator).toContainText("Loudness matching active");
  await expect(indicator).toContainText("hidden until reveal");
  await expect(page.getByTestId("loudness-A")).toHaveCount(0);
  await expect(page.getByTestId("loudness-B")).toHaveCount(0);

  // No measured figure — no "LUFS" number, no gain — anywhere in the pre-conclude
  // DOM, and no basename or path of either input either (still a blind session).
  const body = await page.locator("body").innerText();
  expect(body).not.toContain("LUFS");
  expect(body).not.toContain("a.wav");
  expect(body).not.toContain("b.wav");
  expect(body).not.toContain(fixtures);
});

test("conclude reveals the loudness figures alongside the mapping the record carries", async ({ page }) => {
  // Pin, select a region, and engrave a verdict — the flow is identical to the
  // plain blind one; only the composed concealment differs.
  const stage = page.getByTestId("stage-waveform");
  const box = await stage.boundingBox();
  await page.mouse.click(box.x + box.width * 0.5, box.y + box.height / 2);
  await page.getByTestId("composer").click();
  await page.getByTestId("composer").fill("A sits a touch forward");
  await page.getByTestId("composer").press("Enter");

  await page.getByTestId("open-verdict").click();
  await page.getByTestId("prefer-A").click();
  await page.getByTestId("confidence-4").click();
  await page.getByTestId("save-verdict").click();
  await expect(page.getByTestId("verdict-modal")).toHaveCount(0);

  // The whole DOM the listener saw before concluding carries no measured figure.
  const preReveal = await page.locator("body").innerText();
  expect(preReveal).not.toContain("LUFS");
  await expect(page.getByTestId("reveal")).toHaveCount(0);

  await expect(page.getByTestId("conclude")).toBeEnabled();
  await page.getByTestId("conclude").click();
  await expect(page.getByTestId("conclude-path")).toBeVisible();

  // The record on disk carries the full per-label loudness_match; the reveal must
  // show each label's file *and* its loudness figures, matching the record.
  const files = readdirSync(recordDir).filter((f) => f.endsWith(".json"));
  expect(files).toHaveLength(1);
  const record = JSON.parse(readFileSync(path.join(recordDir, files[0]), "utf8"));
  expect(record.mode).toBe("ab-blind-randomized");
  const lm = record.playback.loudness_match;
  expect(lm.enabled).toBe(true);
  expect(lm.method).toBe("bs1770-integrated");

  // Now — and only now — a measured figure appears, in the reveal.
  const reveal = page.getByTestId("reveal");
  await expect(reveal).toContainText("LUFS");
  for (const c of record.candidates) {
    const revealed = page.getByTestId(`reveal-${c.label}`);
    await expect(revealed).toBeVisible();
    await expect(revealed).toContainText(c.path);
    // The per-label measured figure the record holds is what the reveal renders.
    const lufs = lm.candidates[c.label].measured_lufs.toFixed(1);
    await expect(revealed).toContainText(`${lufs} LUFS`);
  }
});
