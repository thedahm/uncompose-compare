// Project-mode conclude flow spec (issue #41, spec #42 slice 5), driven against
// the packaged binary's served page like the blind and sighted workbench flows.
//
// It certifies the slice-5 DoD a listener can observe: launched in project mode,
// a conclude writes the record into the project's `evaluations/` and hands it to
// `uncompose-project import`; the closing screen shows the registration outcome.
// Composed with `--blind`, the reveal still works alongside it — the two closing
// affordances (identity reveal, registration outcome) coexist.
//
// The project tool is stubbed (a fake `uncompose-project` on PATH that logs the
// argv it receives and exits 0), the pattern the Rust CLI tests use: this repo
// stubs Project's evaluation import (spec #42), so the harness only needs the
// registration outcome to ride the conclude response. Chromium only, like the
// other flow specs.
import { test, expect } from "@playwright/test";
import {
  mkdtempSync,
  mkdirSync,
  copyFileSync,
  writeFileSync,
  readFileSync,
  readdirSync,
  chmodSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { spawnServed } from "./serve.mjs";

const dir = path.dirname(fileURLToPath(import.meta.url));

test.skip(
  ({ browserName }) => browserName !== "chromium",
  "project-mode flow runs on Chromium; the sync contract keeps the full matrix",
);

const bin = process.env.UNCOMPOSE_BIN || "uncompose-compare";
const fixtures = path.join(dir, "fixtures");

const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

let child;
let servedUrl;
let projectRoot;
let stubArgvLog;

test.beforeAll(async () => {
  // A self-contained project: the two matched fixtures as candidate mixes,
  // resolved by bare slug (no derivations, so no SRC lane — this spec is about
  // the handover, not the source lane). The manifest records each file's real
  // sha256 so the loader's integrity check passes.
  projectRoot = mkdtempSync(path.join(tmpdir(), "project-flow-"));
  copyFileSync(path.join(fixtures, "a.wav"), path.join(projectRoot, "mix-a.wav"));
  copyFileSync(path.join(fixtures, "b.wav"), path.join(projectRoot, "mix-b.wav"));
  const manifest = {
    schema: "https://uncompose.org/schemas/project/v0/uncompose.project.json",
    id: "01PROJECTFLOWULID0000000000",
    assets: [
      {
        id: "mix-a",
        slug: "mix-a",
        file: "mix-a.wav",
        sha256: sha256(path.join(projectRoot, "mix-a.wav")),
      },
      {
        id: "mix-b",
        slug: "mix-b",
        file: "mix-b.wav",
        sha256: sha256(path.join(projectRoot, "mix-b.wav")),
      },
    ],
  };
  writeFileSync(
    path.join(projectRoot, "uncompose.project.json"),
    JSON.stringify(manifest, null, 2),
  );

  // Stub `uncompose-project` on PATH: it logs the argv it received and exits 0
  // (a clean registration). Kept in its own dir so PATH finds exactly it.
  const stubDir = mkdtempSync(path.join(tmpdir(), "project-tool-"));
  stubArgvLog = path.join(stubDir, "argv");
  const tool = path.join(stubDir, "uncompose-project");
  writeFileSync(tool, `#!/bin/sh\nprintf '%s\\n' "$@" > ${JSON.stringify(stubArgvLog)}\nexit 0\n`);
  chmodSync(tool, 0o755);

  const served = spawnServed(bin, ["--project", projectRoot, "mix-a", "mix-b", "--blind"], {
    cwd: projectRoot,
    env: { ...process.env, PATH: `${stubDir}${path.delimiter}${process.env.PATH}` },
  });
  child = served.child;
  servedUrl = await served.url;
});

test.afterAll(() => {
  if (child?.pid) child.kill("SIGKILL");
});

test("conclude: the closing screen shows the registration outcome, and the reveal works alongside it", async ({
  page,
}) => {
  await page.goto(servedUrl, { waitUntil: "load" });
  await page.getByTestId("workbench").waitFor({ state: "visible", timeout: 30_000 });

  // Engrave a verdict — the flow is identical to the blind spec.
  const stage = page.getByTestId("stage-waveform");
  const box = await stage.boundingBox();
  await page.mouse.click(box.x + box.width * 0.5, box.y + box.height / 2);
  await page.getByTestId("open-verdict").click();
  await page.getByTestId("prefer-A").click();
  await page.getByTestId("confidence-4").click();
  await page.getByTestId("save-verdict").click();
  await expect(page.getByTestId("verdict-modal")).toHaveCount(0);

  await expect(page.getByTestId("conclude")).toBeEnabled();
  await page.getByTestId("conclude").click();

  // The closing screen shows both affordances: the record path, the registration
  // outcome ("registered"), and — because this is a blind session — the reveal.
  await expect(page.getByTestId("conclude-path")).toBeVisible();
  await expect(page.getByTestId("registration-ok")).toBeVisible();
  await expect(page.getByTestId("reveal")).toBeVisible();

  // The record landed under the project's `evaluations/`, not the invoking cwd.
  const evaluations = path.join(projectRoot, "evaluations");
  const records = readdirSync(evaluations).filter((f) => f.endsWith(".json"));
  expect(records).toHaveLength(1);
  const recordPath = path.join(evaluations, records[0]);

  // The stub received the pinned argv with absolute paths.
  const argv = readFileSync(stubArgvLog, "utf8").trim().split("\n");
  expect(argv).toEqual(["import", "--project", projectRoot, recordPath]);
});
