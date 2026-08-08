import { describe, expect, it } from "vitest";
import {
  addObservation,
  canRedo,
  canUndo,
  caretGlyph,
  deleteObservation,
  editObservation,
  emptyLedger,
  makeObservation,
  redo,
  undo,
  type Observation,
} from "./ledger";

// The ledger is the append-only observation store plus an undo/redo history.
// It is pure (ids and wall-clock times are injected by the App, not read from
// the clock here) so the create/edit/delete/undo/redo logic is unit-tested off
// a browser, while the DOM wiring is certified by the workbench flow spec.

function obs(id: string, over: Partial<Observation> = {}): Observation {
  return {
    id,
    at: "2026-08-08T00:00:00.000Z",
    position: 1,
    loop: null,
    candidate: "A",
    text: "",
    ...over,
  };
}

describe("makeObservation", () => {
  it("captures at/position/candidate/text and no loop when no region is active", () => {
    const o = makeObservation({
      id: "1",
      at: "2026-08-08T00:00:01.000Z",
      candidate: "A",
      position: 2.5,
      region: null,
      text: "hi",
    });
    expect(o).toEqual({
      id: "1",
      at: "2026-08-08T00:00:01.000Z",
      candidate: "A",
      position: 2.5,
      loop: null,
      text: "hi",
    });
  });
  it("references loop 0 when a region is active (the schema's single loop)", () => {
    const o = makeObservation({
      id: "1",
      at: "2026-08-08T00:00:01.000Z",
      candidate: "both",
      position: 5,
      region: { start: 4, end: 6 },
      text: "",
    });
    expect(o.loop).toBe(0);
  });
});

describe("caretGlyph", () => {
  it("uses ▼ for A, ▲ for B, ◆ for both", () => {
    expect(caretGlyph("A")).toBe("▼");
    expect(caretGlyph("B")).toBe("▲");
    expect(caretGlyph("both")).toBe("◆");
  });
});

describe("addObservation", () => {
  it("appends in creation order (chronological)", () => {
    const l = addObservation(addObservation(emptyLedger, obs("1")), obs("2"));
    expect(l.observations.map((o) => o.id)).toEqual(["1", "2"]);
  });
});

describe("editObservation", () => {
  it("rewrites one entry's text and leaves the rest", () => {
    const l = editObservation(
      addObservation(addObservation(emptyLedger, obs("1", { text: "a" })), obs("2", { text: "b" })),
      "1",
      "edited",
    );
    expect(l.observations.map((o) => o.text)).toEqual(["edited", "b"]);
  });
});

describe("deleteObservation", () => {
  it("removes the entry by id", () => {
    const l = deleteObservation(
      addObservation(addObservation(emptyLedger, obs("1")), obs("2")),
      "1",
    );
    expect(l.observations.map((o) => o.id)).toEqual(["2"]);
  });
});

describe("undo / redo", () => {
  it("undo restores the previous observation set", () => {
    const one = addObservation(emptyLedger, obs("1"));
    const two = addObservation(one, obs("2"));
    const back = undo(two);
    expect(back.observations.map((o) => o.id)).toEqual(["1"]);
  });
  it("redo replays an undone change", () => {
    const two = addObservation(addObservation(emptyLedger, obs("1")), obs("2"));
    const forward = redo(undo(two));
    expect(forward.observations.map((o) => o.id)).toEqual(["1", "2"]);
  });
  it("undo/redo restore an edit as well as a create", () => {
    const created = addObservation(emptyLedger, obs("1", { text: "orig" }));
    const edited = editObservation(created, "1", "changed");
    expect(undo(edited).observations[0].text).toBe("orig");
    expect(redo(undo(edited)).observations[0].text).toBe("changed");
  });
  it("undo/redo restore a delete", () => {
    const created = addObservation(emptyLedger, obs("1"));
    const removed = deleteObservation(created, "1");
    expect(undo(removed).observations.map((o) => o.id)).toEqual(["1"]);
    expect(redo(undo(removed)).observations).toEqual([]);
  });
  it("a fresh change after an undo clears the redo stack", () => {
    const two = addObservation(addObservation(emptyLedger, obs("1")), obs("2"));
    const afterUndo = undo(two);
    const diverged = addObservation(afterUndo, obs("3"));
    expect(canRedo(diverged)).toBe(false);
    expect(diverged.observations.map((o) => o.id)).toEqual(["1", "3"]);
  });
  it("undo/redo at the ends of history are no-ops", () => {
    expect(undo(emptyLedger)).toBe(emptyLedger);
    expect(redo(emptyLedger)).toBe(emptyLedger);
    expect(canUndo(emptyLedger)).toBe(false);
    expect(canRedo(emptyLedger)).toBe(false);
  });
});
