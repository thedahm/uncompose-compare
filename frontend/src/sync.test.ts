import { afterEach, describe, expect, it } from "vitest";
import { installSyncHarness, renderSync } from "./sync";

// Foundation smoke test for the web `test` lane: prove the served-page seam the
// #73 harness drives is the one actually registered on load. The seam's whole
// contract is "`window.__uncomposeSync` is the render entry point" — the render
// itself needs an OfflineAudioContext, so the cross-engine audio math is
// certified by the Playwright matrix in `harness/`, not re-unit-tested here
// (per the spec's testing decisions).
describe("installSyncHarness", () => {
  const hadWindow = "window" in globalThis;

  afterEach(() => {
    // vitest runs in Node, which has no `window`; leave the global as we found
    // it so this test cannot leak into any other.
    if (!hadWindow) delete (globalThis as { window?: unknown }).window;
  });

  it("registers renderSync as the sync seam on window", () => {
    globalThis.window = {} as Window & typeof globalThis;
    installSyncHarness();
    expect(window.__uncomposeSync).toBe(renderSync);
  });
});
