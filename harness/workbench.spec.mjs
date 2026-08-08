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

test("help: ? toggles the keymap modal and documents the region keys", async ({ page }) => {
  await expect(page.getByTestId("help-modal")).toHaveCount(0);
  await page.keyboard.press("?");
  await expect(page.getByTestId("help-modal")).toBeVisible();
  await expect(page.getByTestId("help-modal")).toContainText("play / stop");
  // The region keys (issue #13) are documented alongside the transport keymap.
  await expect(page.getByTestId("help-modal")).toContainText("select a region");
  await expect(page.getByTestId("help-modal")).toContainText("clear the region");
  await page.keyboard.press("?");
  await expect(page.getByTestId("help-modal")).toHaveCount(0);
});

// Drag across a waveform from `from` to `to` (both track fractions), selecting a
// region; a genuine drag (well over the click threshold) never seeks.
async function dragRegion(page, testid, from, to) {
  const box = await page.getByTestId(testid).boundingBox();
  const y = box.y + box.height / 2;
  await page.mouse.move(box.x + box.width * from, y);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * to, y);
  await page.mouse.up();
}

const leftPct = (page, testid) =>
  page.getByTestId(testid).evaluate((el) => parseFloat(el.style.left) || 0);

test("region: dragging the stage paints one region across every display", async ({ page }) => {
  await expect(page.getByTestId("loop-status")).toHaveAttribute("data-region", "false");
  await dragRegion(page, "stage-waveform", 0.3, 0.6);
  // The same region renders on the stage and both lane waveforms.
  await expect(page.getByTestId("stage-waveform-region")).toBeVisible();
  await expect(page.getByTestId("waveform-A-region")).toBeVisible();
  await expect(page.getByTestId("waveform-B-region")).toBeVisible();
  await expect(page.getByTestId("loop-status")).toHaveAttribute("data-region", "true");
});

test("region: dragging a lane selects without auditioning that candidate", async ({ page }) => {
  await expect(page.getByTestId("live-lane")).toContainText("A");
  await dragRegion(page, "waveform-B", 0.3, 0.6);
  await expect(page.getByTestId("waveform-B-region")).toBeVisible();
  // A drag selects a region; it must not switch the audible candidate to B.
  await expect(page.getByTestId("live-lane")).toContainText("A");
});

test("loop: r activates/deactivates looping and u clears the region", async ({ page }) => {
  await dragRegion(page, "stage-waveform", 0.3, 0.6);
  await expect(page.getByTestId("loop-status")).toHaveAttribute("data-looping", "false");
  await page.keyboard.press("r");
  await expect(page.getByTestId("loop-status")).toHaveAttribute("data-looping", "true");
  await expect(page.getByTestId("stage-waveform-region")).toHaveAttribute("data-looping", "true");
  await page.keyboard.press("r");
  await expect(page.getByTestId("loop-status")).toHaveAttribute("data-looping", "false");
  await page.keyboard.press("u");
  await expect(page.getByTestId("loop-status")).toHaveAttribute("data-region", "false");
  await expect(page.getByTestId("stage-waveform-region")).toHaveCount(0);
});

test("loop: plays into the region, loops within it, then continues out on deactivate", async ({ page }) => {
  // A region in the back half, so there is a real span before it to play into.
  await dragRegion(page, "stage-waveform", 0.45, 0.75);
  await page.keyboard.press("Home");
  await page.keyboard.press("r");
  await expect(page.getByTestId("loop-status")).toHaveAttribute("data-looping", "true");
  await page.keyboard.press(" "); // play from the start

  // Play-into: the playhead advances from before the region into it.
  await expect.poll(() => leftPct(page, "stage-waveform-playhead"), { timeout: 8_000 })
    .toBeGreaterThan(45);
  await expect(page.getByTestId("play-toggle")).toContainText("Stop");

  // Loops: after longer than the whole ~1.5 s track it is still inside the
  // region (never ran out to the end) and still playing.
  await page.waitForTimeout(1_800);
  expect(await leftPct(page, "stage-waveform-playhead")).toBeLessThan(80);
  await expect(page.getByTestId("play-toggle")).toContainText("Stop");

  // Deactivate continues out: the playhead passes the region end.
  await page.keyboard.press("r");
  await expect.poll(() => leftPct(page, "stage-waveform-playhead"), { timeout: 8_000 })
    .toBeGreaterThan(82);
});
