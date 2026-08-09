// SRC-lane workbench flow spec (issue #40, spec #42), driven against the
// *packaged binary's served page* like the sighted, blind, and project flows
// (issue #5).
//
// Where sync.spec.mjs certifies what the third lane costs the audio contract
// (nothing: zero offset and the crossfade bound both survive it), this spec
// certifies what a listener sees. Two sessions run side by side, both launched
// with an explicit `--source` (bare-file mode's SRC lane):
//
//   - sighted: three lanes render, the source is named and auditionable by
//     click, and `x` stays the A/B toggle — SRC can never win, so it is not a
//     switch target (ADR-0009);
//   - blind: the same source lane stays fully identified while A and B keep
//     their concealment — the source is the anchor a blind listener compares
//     against, and revealing it discloses nothing about which candidate is which.
//
// Chromium only, like the other flow specs; the cross-engine contract lives in
// sync.spec.mjs.
import { test, expect } from "@playwright/test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { spawnServed } from "./serve.mjs";

const dir = path.dirname(fileURLToPath(import.meta.url));

test.skip(
  ({ browserName }) => browserName !== "chromium",
  "the SRC-lane flow runs on Chromium; the sync contract keeps the full matrix",
);

const bin = process.env.UNCOMPOSE_BIN || "uncompose-compare";
const fixtures = path.join(dir, "fixtures");
const inputs = {
  a: path.join(fixtures, "a.wav"),
  b: path.join(fixtures, "b.wav"),
  src: path.join(fixtures, "src.wav"),
};

/** Both sessions, keyed by mode: `{ child, url }` each. */
const servers = {};

test.beforeAll(async () => {
  // Each session gets its own throwaway cwd: a conclude would write a record
  // there, and neither should land beside the other's.
  const launch = async (mode, extra) => {
    const cwd = mkdtempSync(path.join(tmpdir(), `src-lane-${mode}-`));
    const served = spawnServed(bin, [inputs.a, inputs.b, "--source", inputs.src, ...extra], {
      cwd,
    });
    servers[mode] = { child: served.child, url: await served.url };
  };
  await Promise.all([launch("sighted", []), launch("blind", ["--blind"])]);
});

test.afterAll(() => {
  for (const server of Object.values(servers)) {
    if (server?.child?.pid) server.child.kill("SIGKILL");
  }
});

/** Open a session's page and wait for the workbench to render. */
const open = async (page, mode) => {
  await page.goto(servers[mode].url, { waitUntil: "load" });
  await page.getByTestId("workbench").waitFor({ state: "visible", timeout: 30_000 });
};

test("sighted: the source renders as a named third lane", async ({ page }) => {
  await open(page, "sighted");

  await expect(page.getByTestId("lane-A")).toBeVisible();
  await expect(page.getByTestId("lane-B")).toBeVisible();
  const src = page.getByTestId("lane-SRC");
  await expect(src).toBeVisible();
  await expect(src).toContainText("SRC");
  await expect(src).toContainText("src.wav");
  // It is a full playback lane, waveform and all — not a footnote.
  await expect(page.getByTestId("waveform-SRC")).toBeVisible();
});

test("sighted: SRC auditions by click but is not an `x` switch target", async ({ page }) => {
  await open(page, "sighted");
  await expect(page.getByTestId("live-lane")).toContainText("A");

  // Clicking the lane's label auditions the source.
  await page.getByTestId("lane-SRC").locator("strong").click();
  await expect(page.getByTestId("live-lane")).toContainText("SRC");
  await expect(page.getByTestId("lane-SRC")).toHaveAttribute("data-live", "true");
  await expect(page.getByTestId("live-marker-SRC")).toContainText("●");

  // `x` is the A/B toggle: from SRC it returns to A, and keeps toggling A/B —
  // the source is never a member of the pair being compared.
  await page.keyboard.press("x");
  await expect(page.getByTestId("live-lane")).toContainText("A");
  await page.keyboard.press("x");
  await expect(page.getByTestId("live-lane")).toContainText("B");
  await expect(page.getByTestId("lane-SRC")).toHaveAttribute("data-live", "false");
});

test("blind: A and B stay concealed while the source stays identified", async ({ page }) => {
  await open(page, "blind");

  // A/B concealment is exactly as without a source: no identifying lane rows,
  // two anonymous switch buttons instead.
  await expect(page.getByTestId("lanes")).toHaveCount(0);
  await expect(page.getByTestId("switch-A")).toBeVisible();
  await expect(page.getByTestId("switch-B")).toBeVisible();

  // The source lane, by contrast, is named and auditionable.
  const src = page.getByTestId("lane-SRC");
  await expect(src).toBeVisible();
  await expect(src).toContainText("src.wav");
  await src.locator("strong").click();
  await expect(page.getByTestId("live-lane")).toContainText("SRC");

  // Naming the source discloses nothing about which candidate is which: neither
  // candidate's basename appears anywhere in the DOM.
  const body = await page.locator("body").innerText();
  expect(body).toContain("src.wav");
  expect(body).not.toContain("a.wav");
  expect(body).not.toContain("b.wav");
});
