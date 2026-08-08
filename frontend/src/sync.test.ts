import { describe, expect, it } from "vitest";
import { installSyncHarness, renderSync } from "./sync";

// Foundation smoke test for the web `test` lane: prove the served-page seam the
// #73 harness drives is the one actually registered on load. The cross-engine
// audio math itself is certified by the Playwright matrix in `harness/`, not
// re-unit-tested here (per the spec's testing decisions).
describe("installSyncHarness", () => {
  it("registers renderSync as the sync seam on window", () => {
    // vitest runs in Node, which has no `window`; stub the bare object the
    // seam is installed onto.
    globalThis.window = {} as Window & typeof globalThis;
    installSyncHarness();
    expect(window.__uncomposeSync).toBe(renderSync);
  });
});
