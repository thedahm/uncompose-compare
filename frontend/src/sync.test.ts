import { describe, expect, it } from "vitest";
import { installSyncHarness, renderSync } from "./sync";

// Foundation smoke test for the web `test` lane: prove the served-page seam the
// #73 harness drives is the one actually registered on load. The cross-engine
// audio math itself is certified by the Playwright matrix in `harness/`, not
// re-unit-tested here (per the spec's testing decisions).
describe("installSyncHarness", () => {
  it("registers renderSync as the sync seam on window", () => {
    const g = globalThis as unknown as {
      window?: { __uncomposeSync?: unknown };
    };
    g.window = {};
    installSyncHarness();
    expect(g.window.__uncomposeSync).toBe(renderSync);
  });
});
