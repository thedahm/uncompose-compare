// Workbench flow spec (issue #12), driven against the *packaged binary's served
// page* — the same served page and global-setup the sync matrix uses (issue #5).
//
// Where sync.spec.mjs certifies the cross-engine audio contract, this spec
// certifies the DoD slice a listener can observe: both candidates load and
// render waveforms, `x`/lane-click switches the audible candidate at the current
// position, clicking a waveform seeks, dragging selects a loop region, and the
// waveform/loudness/spectral view toggle (issue #14) switches every display
// together while seek and region drag keep working in a non-waveform view. It
// runs on Chromium only (the flow is engine-independent; the contract, where
// engines differ, keeps the full matrix per the spec's testing decisions).
import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import { readFileSync as read } from "node:fs";
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

test("views: the toggle switches every display together (stage + lanes)", async ({ page }) => {
  // All displays start on the waveform view.
  for (const id of ["stage-waveform", "waveform-A", "waveform-B"]) {
    await expect(page.getByTestId(id)).toHaveAttribute("data-view", "waveform");
  }
  await page.getByTestId("view-spectral").click();
  // A single toggle moves the stage and both lane rows to the spectral view.
  for (const id of ["stage-waveform", "waveform-A", "waveform-B"]) {
    await expect(page.getByTestId(id)).toHaveAttribute("data-view", "spectral");
  }
  await page.getByTestId("view-loudness").click();
  for (const id of ["stage-waveform", "waveform-A", "waveform-B"]) {
    await expect(page.getByTestId(id)).toHaveAttribute("data-view", "loudness");
  }
});

test("views: seek and region drag work in a non-waveform view", async ({ page }) => {
  await page.getByTestId("view-spectral").click();
  await expect(page.getByTestId("stage-waveform")).toHaveAttribute("data-view", "spectral");

  // Seek: clicking the spectral stage moves the playhead just like the waveform.
  const stage = page.getByTestId("stage-waveform");
  const box = await stage.boundingBox();
  await page.mouse.click(box.x + box.width * 0.5, box.y + box.height / 2);
  await expect(page.getByTestId("transport-position")).not.toContainText("0:00.000 /");
  expect(await leftPct(page, "stage-waveform-playhead")).toBeGreaterThan(10);

  // Region drag: selecting on the spectral view paints the region everywhere,
  // and the region survives switching to a third view.
  await dragRegion(page, "stage-waveform", 0.3, 0.6);
  await expect(page.getByTestId("stage-waveform-region")).toBeVisible();
  await expect(page.getByTestId("waveform-A-region")).toBeVisible();
  await expect(page.getByTestId("waveform-B-region")).toBeVisible();
  await expect(page.getByTestId("loop-status")).toHaveAttribute("data-region", "true");
  await page.getByTestId("view-loudness").click();
  await expect(page.getByTestId("stage-waveform-region")).toBeVisible();
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

// Observation ledger (issue #15). Locators match by testid prefix because the
// per-entry ids are runtime UUIDs the test can't know ahead of time.
const entries = (page) => page.locator('[data-testid^="ledger-entry-"]');
const carets = (page) => page.locator('[data-testid^="stage-waveform-caret-"]');

// Seek into the middle of the track (so a pin's caret sits well inside the
// waveform), type a note in the composer, and submit it (shift tags both).
async function pinViaComposer(page, text, { shift = false } = {}) {
  const stage = page.getByTestId("stage-waveform");
  const box = await stage.boundingBox();
  await page.mouse.click(box.x + box.width * 0.5, box.y + box.height / 2);
  await page.getByTestId("composer").click();
  await page.getByTestId("composer").fill(text);
  await page.getByTestId("composer").press(shift ? "Shift+Enter" : "Enter");
}

test("pin: enter pins on the live candidate and draws a ▼ caret on the stage", async ({ page }) => {
  await expect(page.getByTestId("ledger-empty")).toBeVisible();
  await pinViaComposer(page, "boxy low end");
  // The observation lands in the chronological ledger with its text.
  await expect(entries(page)).toHaveCount(1);
  await expect(entries(page).first()).toContainText("boxy low end");
  // A is live, so the caret is tagged to A (▼).
  await expect(carets(page)).toHaveCount(1);
  await expect(carets(page).first()).toHaveAttribute("data-caret-candidate", "A");
  await expect(carets(page).first()).toContainText("▼");
});

test("pin: shift+enter pins on both candidates (◆ caret)", async ({ page }) => {
  await pinViaComposer(page, "both muddy here", { shift: true });
  await expect(carets(page).first()).toHaveAttribute("data-caret-candidate", "both");
  await expect(carets(page).first()).toContainText("◆");
});

test("ledger: clicking text edits it in place", async ({ page }) => {
  await pinViaComposer(page, "first take");
  const entry = entries(page).first();
  await entry.getByText("first take").click();
  const input = page.locator('[data-testid^="ledger-text-input-"]');
  await expect(input).toBeVisible();
  await input.fill("edited take");
  await input.press("Enter");
  await expect(entry).toContainText("edited take");
});

test("ledger: each entry deletes", async ({ page }) => {
  await pinViaComposer(page, "to be removed");
  await expect(entries(page)).toHaveCount(1);
  await page.locator('[data-testid^="ledger-delete-"]').first().click();
  await expect(entries(page)).toHaveCount(0);
  await expect(carets(page)).toHaveCount(0);
});

test("ledger: ctrl+z / ctrl+shift+z undo and redo ledger changes", async ({ page }) => {
  await pinViaComposer(page, "note one");
  await pinViaComposer(page, "note two");
  await expect(entries(page)).toHaveCount(2);
  // Blur the composer so the ctrl keys reach the transport keymap, not the input.
  await page.getByTestId("hello").click();
  await page.keyboard.press("Control+z");
  await expect(entries(page)).toHaveCount(1);
  await expect(entries(page).first()).toContainText("note one");
  await page.keyboard.press("Control+Shift+z");
  await expect(entries(page)).toHaveCount(2);
  await expect(entries(page).nth(1)).toContainText("note two");
});

test("caret: hover and click connect a caret to its ledger entry both ways", async ({ page }) => {
  await pinViaComposer(page, "linked note");
  const caret = carets(page).first();
  const entry = entries(page).first();
  // Hovering the caret highlights the ledger entry.
  await caret.hover();
  await expect(entry).toHaveAttribute("data-active", "true");
  // Hovering the ledger entry highlights the caret.
  await entry.hover();
  await expect(caret).toHaveAttribute("data-active", "true");
  // Clicking the caret finds (selects) the entry.
  await page.getByTestId("hello").hover();
  await expect(entry).toHaveAttribute("data-active", "false");
  await caret.click();
  await expect(entry).toHaveAttribute("data-active", "true");
});

test("help: ? documents the observation and ledger keys", async ({ page }) => {
  await page.keyboard.press("?");
  await expect(page.getByTestId("help-modal")).toContainText("pin an observation");
  await expect(page.getByTestId("help-modal")).toContainText("focus the composer");
  await expect(page.getByTestId("help-modal")).toContainText("undo / redo");
});

// Verdict & conclude (issue #16) — the record end of the DoD slice. This runs
// against the packaged binary's served page, so a conclude actually writes a
// record to disk (in the binary's invoking directory); the spec reads it back
// and asserts it matches what the session did.
async function setVerdict(page, { prefer, confidence, criterion, summary, context } = {}) {
  await page.getByTestId("open-verdict").click();
  await expect(page.getByTestId("verdict-modal")).toBeVisible();
  if (prefer) await page.getByTestId(`prefer-${prefer}`).click();
  if (confidence) await page.getByTestId(`confidence-${confidence}`).click();
  if (criterion) await page.getByTestId("verdict-criterion").fill(criterion);
  if (summary) await page.getByTestId("verdict-summary-input").fill(summary);
  if (context) await page.getByTestId("verdict-context").fill(context);
  await page.getByTestId("save-verdict").click();
  await expect(page.getByTestId("verdict-modal")).toHaveCount(0);
}

test("verdict: preferring a candidate needs a confidence before it can save", async ({ page }) => {
  await page.getByTestId("open-verdict").click();
  await page.getByTestId("prefer-A").click();
  // A preferred candidate with no confidence yet cannot be saved (schema needs it).
  await expect(page.getByTestId("save-verdict")).toBeDisabled();
  await page.getByTestId("confidence-4").click();
  await expect(page.getByTestId("save-verdict")).toBeEnabled();
});

test("verdict: no preference is a valid, savable outcome with no stars", async ({ page }) => {
  await page.getByTestId("open-verdict").click();
  await page.getByTestId("prefer-none").click();
  // No preference needs no confidence.
  await expect(page.getByTestId("confidence-stars")).toHaveCount(0);
  await expect(page.getByTestId("save-verdict")).toBeEnabled();
  await page.getByTestId("save-verdict").click();
  await expect(page.getByTestId("verdict-summary")).toContainText("No preference");
});

test("verdict: saved confidence shows color-coded stars on the preferred lane row", async ({ page }) => {
  await setVerdict(page, { prefer: "A", confidence: 4 });
  const stars = page.getByTestId("verdict-stars-A");
  await expect(stars).toBeVisible();
  await expect(stars).toContainText("★");
  // The other lane carries no verdict stars.
  await expect(page.getByTestId("verdict-stars-B")).toHaveCount(0);
  // Save engraves but stays editable: the modal reopens.
  await page.getByTestId("open-verdict").click();
  await expect(page.getByTestId("verdict-modal")).toBeVisible();
  await expect(page.getByTestId("prefer-A")).toHaveAttribute("aria-pressed", "true");
});

test("conclude: writes a record that matches the session and reports where it landed", async ({ page }) => {
  // A full DoD slice: pin an observation, select a region, decide a verdict,
  // then conclude and assert the on-disk record matches.
  await pinViaComposer(page, "chorus is cleaner on A");
  await dragRegion(page, "stage-waveform", 0.3, 0.6);
  await setVerdict(page, {
    prefer: "A",
    confidence: 5,
    criterion: "clarity",
    summary: "A wins in the chorus",
    context: "picking a master",
  });

  await expect(page.getByTestId("conclude")).toBeEnabled();
  await page.getByTestId("conclude").click();
  await expect(page.getByTestId("conclude-path")).toBeVisible();

  // The UI reports the path; read that record back and assert it is the session.
  const recordPath = await page.getByTestId("conclude-path").locator("code").innerText();
  const record = JSON.parse(read(recordPath, "utf8"));
  expect(record.schema).toContain("compare/v0");
  expect(record.mode).toBe("ab");
  expect(record.id).toHaveLength(26);
  expect(record.candidates).toHaveLength(2);
  expect(record.candidates[0].label).toBe("A");
  expect(record.candidates[0].sha256).toMatch(/^[0-9a-f]{64}$/);
  expect(record.result).toMatchObject({ preference: "A", confidence: 5, criterion: "clarity" });
  expect(record.observations[0].text).toBe("chorus is cleaner on A");
  expect(record.loops).toHaveLength(1);
  expect(record.context).toBe("picking a master");

  // A second conclude is refused: once written, the button is spent.
  await expect(page.getByTestId("conclude")).toBeDisabled();
});
