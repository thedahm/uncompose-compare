// Blind workbench flow spec (issue #29, spec #26), driven against the *packaged
// binary's served page* — the same served page the sync matrix and the sighted
// workbench flow use (issue #5).
//
// Where workbench.spec.mjs certifies the sighted DoD slice, this spec certifies
// the blind one a listener can observe: launch `--blind`, and the page shows only
// the anonymous labels A and B with no filename, path, hash, or size of either
// input anywhere in the DOM; the identifying lane rows are gone, replaced by two
// anonymous switch buttons that switch by click and by `x`; pins, region, and the
// verdict flow work exactly as sighted; and only after the record is written does
// the UI reveal which file each label was — a mapping identical to the record on
// disk. It runs on Chromium only (the flow is engine-independent; the cross-engine
// contract keeps the full matrix in sync.spec.mjs).
import { test, expect } from "@playwright/test";
import { mkdtempSync, readFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { spawnServed } from "./serve.mjs";

const dir = path.dirname(fileURLToPath(import.meta.url));

test.skip(
  ({ browserName }) => browserName !== "chromium",
  "blind workbench flow runs on Chromium; the sync contract keeps the full matrix",
);

// The blind server is its own launch: global-setup runs the sighted binary, so a
// blind session needs a separate `--blind` process. It writes its record into a
// throwaway cwd the spec owns, so `conclude` lands somewhere the test can read
// back. The fixture pair (a.wav/b.wav) is same duration/rate/channels, different
// content — exactly what blind mode accepts.
const bin = process.env.UNCOMPOSE_BIN || "uncompose-compare";
const fixtures = path.join(dir, "fixtures");
const inputs = { a: path.join(fixtures, "a.wav"), b: path.join(fixtures, "b.wav") };

let child;
let servedUrl;
let recordDir;

test.beforeAll(async () => {
  recordDir = mkdtempSync(path.join(tmpdir(), "blind-flow-"));
  const served = spawnServed(bin, [inputs.a, inputs.b, "--blind"], { cwd: recordDir });
  child = served.child;
  servedUrl = await served.url;
});

test.afterAll(() => {
  if (child?.pid) child.kill("SIGKILL");
});

test.beforeEach(async ({ page }) => {
  await page.goto(servedUrl, { waitUntil: "load" });
  await page.getByTestId("workbench").waitFor({ state: "visible", timeout: 30_000 });
});

test("leak-free: the DOM shows anonymous labels and no identifying detail before conclude", async ({ page }) => {
  // The identifying lane rows are gone; two anonymous switch buttons stand in.
  await expect(page.getByTestId("lanes")).toHaveCount(0);
  await expect(page.getByTestId("waveform-A")).toHaveCount(0);
  await expect(page.getByTestId("waveform-B")).toHaveCount(0);
  await expect(page.getByTestId("switch-A")).toBeVisible();
  await expect(page.getByTestId("switch-B")).toBeVisible();
  // The stage still shows the live candidate's waveform under its anonymous label.
  await expect(page.getByTestId("stage-waveform")).toBeVisible();
  await expect(page.getByTestId("live-lane")).toContainText("A");

  // No basename or path of either input appears anywhere in the DOM.
  const body = await page.locator("body").innerText();
  expect(body).not.toContain("a.wav");
  expect(body).not.toContain("b.wav");
  expect(body).not.toContain(fixtures);
});

test("switch: the anonymous buttons and `x` both move the live candidate", async ({ page }) => {
  await expect(page.getByTestId("live-lane")).toContainText("A");
  await expect(page.getByTestId("switch-A")).toHaveAttribute("data-live", "true");

  // Clicking the B button auditions B without ever naming a file.
  await page.getByTestId("switch-B").click();
  await expect(page.getByTestId("live-lane")).toContainText("B");
  await expect(page.getByTestId("switch-B")).toHaveAttribute("data-live", "true");
  await expect(page.getByTestId("live-marker-B")).toContainText("●");

  // `x` still switches, exactly as in sighted mode.
  await page.keyboard.press("x");
  await expect(page.getByTestId("live-lane")).toContainText("A");
  await expect(page.getByTestId("switch-A")).toHaveAttribute("data-live", "true");
});

test("conclude: reveals the label→file mapping matching the written record", async ({ page }) => {
  // Pin, select a region, and engrave a verdict with confidence — the flow is
  // identical to sighted; only the identities are concealed until now.
  const stage = page.getByTestId("stage-waveform");
  const box = await stage.boundingBox();
  await page.mouse.click(box.x + box.width * 0.5, box.y + box.height / 2);
  await page.getByTestId("composer").click();
  await page.getByTestId("composer").fill("cleaner top end on A");
  await page.getByTestId("composer").press("Enter");

  await page.getByTestId("open-verdict").click();
  await page.getByTestId("prefer-A").click();
  await page.getByTestId("confidence-4").click();
  await page.getByTestId("save-verdict").click();
  await expect(page.getByTestId("verdict-modal")).toHaveCount(0);

  // Capture the whole DOM the listener saw *before* concluding: it must carry no
  // identity — not the basenames, not the path, hash, or size the record holds.
  const preReveal = await page.locator("body").innerText();
  expect(preReveal).not.toContain("a.wav");
  expect(preReveal).not.toContain("b.wav");
  await expect(page.getByTestId("reveal")).toHaveCount(0);

  await expect(page.getByTestId("conclude")).toBeEnabled();
  await page.getByTestId("conclude").click();
  await expect(page.getByTestId("conclude-path")).toBeVisible();

  // The record on disk carries full identities; the reveal must match it exactly,
  // and none of those identities may have been in the pre-conclude DOM.
  const files = readdirSync(recordDir).filter((f) => f.endsWith(".json"));
  expect(files).toHaveLength(1);
  const record = JSON.parse(readFileSync(path.join(recordDir, files[0]), "utf8"));
  expect(record.mode).toBe("ab-blind-randomized");

  for (const c of record.candidates) {
    // Nothing identifying leaked before the write.
    expect(preReveal).not.toContain(c.sha256);
    expect(preReveal).not.toContain(String(c.size));
    // The reveal shows this label's real file, identical to the record's path.
    const revealed = page.getByTestId(`reveal-${c.label}`);
    await expect(revealed).toBeVisible();
    await expect(revealed).toContainText(c.path);
  }
});
