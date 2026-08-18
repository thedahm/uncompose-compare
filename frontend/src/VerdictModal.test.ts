import { describe, expect, it } from "vitest";
import { canSave, emptyVerdict, preferenceChosen } from "./VerdictModal";
import type { Verdict } from "./record";

/**
 * The save gate (issue #53): `canSave` is the schema's rule in code — a chosen
 * candidate needs a confidence, "no preference" needs nothing, undecided is
 * never savable. The modal DOM is covered by the workbench flow specs; this
 * pins the truth table the disabled Save button reads from.
 */
function verdict(fields: Partial<Verdict>): Verdict {
  return { ...emptyVerdict, ...fields };
}

describe("preferenceChosen", () => {
  it("is false while undecided", () => {
    expect(preferenceChosen(verdict({ preference: null }))).toBe(false);
  });
  it("is false for a recorded no-preference", () => {
    expect(preferenceChosen(verdict({ preference: "none" }))).toBe(false);
  });
  it("is true for either candidate", () => {
    expect(preferenceChosen(verdict({ preference: "A" }))).toBe(true);
    expect(preferenceChosen(verdict({ preference: "B" }))).toBe(true);
  });
  it("ignores the confidence", () => {
    expect(preferenceChosen(verdict({ preference: "none", confidence: 4 }))).toBe(false);
    expect(preferenceChosen(verdict({ preference: "A", confidence: null }))).toBe(true);
  });
});

describe("canSave", () => {
  it("refuses an undecided verdict", () => {
    expect(canSave(verdict({ preference: null }))).toBe(false);
    expect(canSave(emptyVerdict)).toBe(false);
  });

  it("refuses an undecided verdict even with a stray confidence", () => {
    // Confidence survives switching back to undecided; a decision is still required.
    expect(canSave(verdict({ preference: null, confidence: 3 }))).toBe(false);
  });

  it("saves a no-preference with nothing else set", () => {
    expect(canSave(verdict({ preference: "none" }))).toBe(true);
  });

  it("refuses a chosen candidate without a confidence", () => {
    expect(canSave(verdict({ preference: "A" }))).toBe(false);
    expect(canSave(verdict({ preference: "B" }))).toBe(false);
  });

  it("saves a chosen candidate once a confidence is picked", () => {
    for (const confidence of [1, 2, 3, 4, 5]) {
      expect(canSave(verdict({ preference: "A", confidence }))).toBe(true);
      expect(canSave(verdict({ preference: "B", confidence }))).toBe(true);
    }
  });

  it("does not require the optional prose", () => {
    expect(canSave(verdict({ preference: "A", confidence: 5, criterion: "", summary: "" }))).toBe(
      true,
    );
  });
});
