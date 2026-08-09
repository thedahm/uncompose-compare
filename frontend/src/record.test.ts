import { describe, expect, it } from "vitest";
import { buildRecordPayload, confidenceTier } from "./record";
import type { Observation } from "./ledger";

// The record payload builder is the pure seam between the workbench's in-memory
// session state (verdict, ledger, region) and the schema-shaped JSON the browser
// POSTs to the record endpoint (issue #16). The server owns the rest of the
// record (id, timestamps, candidates); this builds only the session-authored
// result/observations/loops/context. Kept pure so it is unit-tested off a
// browser; the modal DOM and the round-trip write are covered elsewhere.

function obs(over: Partial<Observation> = {}): Observation {
  return {
    id: "o1",
    at: "2026-08-08T00:00:01.000Z",
    position: 1.5,
    loop: null,
    candidate: "A",
    text: "note",
    ...over,
  };
}

describe("buildRecordPayload", () => {
  it("maps a chosen preference to its label with confidence and trimmed criterion/summary", () => {
    const p = buildRecordPayload({
      verdict: { preference: "B", confidence: 4, criterion: "  clarity  ", summary: " B wins " },
      context: "  picking a master  ",
      observations: [],
      region: null,
    });
    expect(p.result).toEqual({
      preference: "B",
      confidence: 4,
      criterion: "clarity",
      summary: "B wins",
    });
    expect(p.context).toBe("picking a master");
    expect(p.loops).toEqual([]);
    expect(p.observations).toEqual([]);
  });

  it("records no preference as a null preference with no confidence", () => {
    const p = buildRecordPayload({
      verdict: { preference: "none", confidence: 3, criterion: "", summary: "" },
      context: "",
      observations: [],
      region: null,
    });
    // No-preference is a valid outcome: preference null, and confidence is
    // dropped even if a value lingered in the modal state.
    expect(p.result).toEqual({ preference: null });
    expect("confidence" in p.result).toBe(false);
    expect("context" in p).toBe(false);
  });

  it("shapes observations to the schema: ms positions, label candidates, loop index", () => {
    const p = buildRecordPayload({
      verdict: { preference: "A", confidence: 5, criterion: "", summary: "" },
      context: "",
      observations: [
        obs({ id: "1", position: 2.25, candidate: "A", loop: 0, text: "bright" }),
        obs({ id: "2", position: null, candidate: "both", loop: null, text: "general" }),
        obs({ id: "3", position: 0.5, candidate: "B", loop: null, text: "" }),
      ],
      region: { start: 1, end: 3 },
    });
    // position seconds → integer ms; A/B keep their label; a positioned pin in
    // the region keeps its loop index.
    expect(p.observations[0]).toEqual({
      at: "2026-08-08T00:00:01.000Z",
      position_ms: 2250,
      loop: 0,
      candidate: "A",
      text: "bright",
    });
    // "both" and an untethered position both drop to the-comparison-generally:
    // no candidate, no position_ms, no loop.
    expect(p.observations[1]).toEqual({
      at: "2026-08-08T00:00:01.000Z",
      text: "general",
    });
    expect("candidate" in p.observations[1]).toBe(false);
    expect("position_ms" in p.observations[1]).toBe(false);
    // Empty text is still a valid observation (a quick pin never filled in).
    expect(p.observations[2]).toEqual({
      at: "2026-08-08T00:00:01.000Z",
      position_ms: 500,
      candidate: "B",
      text: "",
    });
    // The active region becomes the single loop, in ms.
    expect(p.loops).toEqual([{ start_ms: 1000, end_ms: 3000 }]);
  });
});

describe("confidenceTier", () => {
  it("color-codes confidence: red 1-2, amber 3, green 4-5", () => {
    expect(confidenceTier(1)).toBe("low");
    expect(confidenceTier(2)).toBe("low");
    expect(confidenceTier(3)).toBe("mid");
    expect(confidenceTier(4)).toBe("high");
    expect(confidenceTier(5)).toBe("high");
  });
});
