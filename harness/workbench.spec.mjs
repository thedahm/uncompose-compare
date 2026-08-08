// Workbench flow spec (issue #12), driven against the *packaged binary's served
// page* — the same served page and global-setup the sync matrix uses (issue #5).
//
// Where sync.spec.mjs certifies the cross-engine audio contract, this spec
// certifies the DoD slice a listener can observe: both candidates load and
// render waveforms, `x`/lane-click switches the audible candidate at the current
// position, and clicking a waveform seeks. It runs on Chromium only (the flow is
// engine-independent; the contract, where engines differ, keeps the full matrix
// per the spec's testing decisions).
import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { URL_FILE } from "./global-setup.mjs";

const SERVED_URL = readFileSync(URL_FILE, "utf8").trim();

test.skip(
  ({ browserName }) => browserName !== "chromium",
  "workbench flow runs on Chromium; the sync contract keeps the full matrix",
);

test.beforeEach(async ({ page }) => {
  await page.goto(SERVED_URL, { waitUntil: "load" });
  // The workbench mounts once both proxies are fetched and decoded.
  await page.getByTestId("workbench").waitFor({ state: "visible", timeout: 30_000 });
});

test("load: both candidates render waveforms in the stage and lane rows", async ({ page }) => {
  await expect(page.getByTestId("stage-waveform")).toBeVisible();
  await expect(page.getByTestId("waveform-A")).toBeVisible();
  await expect(page.getByTestId("waveform-B")).toBeVisible();
  // Every waveform draws onto its own canvas from decoded PCM.
  await expect(page.getByTestId("waveform-A").locator("canvas")).toBeVisible();
  await expect(page.getByTestId("waveform-B").locator("canvas")).toBeVisible();
  // A is audible at load; its lane carries the live ● marker.
  await expect(page.getByTestId("live-lane")).toContainText("A");
  await expect(page.getByTestId("live-marker-A")).toContainText("●");
});

test("switch: pressing x moves the live candidate from A to B", async ({ page }) => {
  await expect(page.getByTestId("live-lane")).toContainText("A");
  await page.keyboard.press("x");
  await expect(page.getByTestId("live-lane")).toContainText("B");
  await expect(page.getByTestId("live-marker-B")).toContainText("●");
});

test("switch: clicking a lane auditions that candidate", async ({ page }) => {
  await page.getByTestId("lane-B").click();
  await expect(page.getByTestId("live-lane")).toContainText("B");
  await expect(page.getByTestId("lane-B")).toHaveAttribute("data-live", "true");
});

test("seek: clicking the stage waveform moves the playhead without playing", async ({ page }) => {
  await expect(page.getByTestId("transport-position")).toContainText("0:00.000");
  const stage = page.getByTestId("stage-waveform");
  const box = await stage.boundingBox();
  // Click near the middle of the ~1.5 s track; the readout should leave zero.
  await page.mouse.click(box.x + box.width * 0.5, box.y + box.height / 2);
  await expect(page.getByTestId("transport-position")).not.toContainText("0:00.000 /");
  // The playhead overlay tracks the click position.
  const left = await page
    .getByTestId("stage-waveform-playhead")
    .evaluate((el) => el.style.left);
  expect(parseFloat(left)).toBeGreaterThan(10);
});

test("help: ? toggles the keymap modal", async ({ page }) => {
  await expect(page.getByTestId("help-modal")).toHaveCount(0);
  await page.keyboard.press("?");
  await expect(page.getByTestId("help-modal")).toBeVisible();
  await expect(page.getByTestId("help-modal")).toContainText("play / stop");
  await page.keyboard.press("?");
  await expect(page.getByTestId("help-modal")).toHaveCount(0);
});
