import { describe, expect, it } from "vitest";
import {
  clampPosition,
  computePeaks,
  equalPowerCurves,
  formatTime,
  loopedPosition,
  orderedRegion,
  otherLabel,
  stepPosition,
} from "./transport";

// The transport helpers are the pure arithmetic behind the workbench: seek
// clamping, 2-second stepping, the equal-power crossfade curve, waveform peak
// reduction, and the readout formatting. The Web Audio graph that consumes them
// is certified cross-engine by the sync harness (per the spec's testing
// decisions); only this pure math is unit-tested here.

describe("clampPosition", () => {
  it("keeps a position inside the track", () => {
    expect(clampPosition(1.5, 3)).toBe(1.5);
  });
  it("clamps below zero to the start", () => {
    expect(clampPosition(-2, 3)).toBe(0);
  });
  it("clamps past the end to the duration", () => {
    expect(clampPosition(9, 3)).toBe(3);
  });
});

describe("stepPosition", () => {
  it("steps forward by the delta", () => {
    expect(stepPosition(1, 2, 10)).toBe(3);
  });
  it("steps backward by the delta", () => {
    expect(stepPosition(5, -2, 10)).toBe(3);
  });
  it("never steps past the end", () => {
    expect(stepPosition(9, 2, 10)).toBe(10);
  });
  it("never steps before the start", () => {
    expect(stepPosition(1, -2, 10)).toBe(0);
  });
});

describe("otherLabel", () => {
  it("flips A to B and back", () => {
    expect(otherLabel("A")).toBe("B");
    expect(otherLabel("B")).toBe("A");
  });
});

describe("equalPowerCurves", () => {
  it("rises from 0 to 1 while the mirror falls from 1 to 0", () => {
    const { up, down } = equalPowerCurves(64);
    expect(up[0]).toBeCloseTo(0, 6);
    expect(up[up.length - 1]).toBeCloseTo(1, 6);
    expect(down[0]).toBeCloseTo(1, 6);
    expect(down[down.length - 1]).toBeCloseTo(0, 6);
  });
  it("conserves power: up^2 + down^2 == 1 at every step", () => {
    const { up, down } = equalPowerCurves(64);
    for (let i = 0; i < up.length; i++) {
      expect(up[i] * up[i] + down[i] * down[i]).toBeCloseTo(1, 6);
    }
  });
});

describe("computePeaks", () => {
  it("reduces samples to one min/max pair per bucket", () => {
    const data = new Float32Array([1, -1, 0.5, -0.5]);
    const { min, max } = computePeaks(data, 2);
    expect(max[0]).toBe(1);
    expect(min[0]).toBe(-1);
    expect(max[1]).toBe(0.5);
    expect(min[1]).toBe(-0.5);
  });
  it("emits exactly `buckets` pairs even for short data", () => {
    const { min, max } = computePeaks(new Float32Array([0.2]), 8);
    expect(min).toHaveLength(8);
    expect(max).toHaveLength(8);
  });
});

describe("orderedRegion", () => {
  it("orders the two drag endpoints into start<=end", () => {
    expect(orderedRegion(3, 1, 10)).toEqual({ start: 1, end: 3 });
    expect(orderedRegion(1, 3, 10)).toEqual({ start: 1, end: 3 });
  });
  it("clamps both endpoints into the track", () => {
    expect(orderedRegion(-2, 99, 10)).toEqual({ start: 0, end: 10 });
  });
});

describe("loopedPosition", () => {
  const region = { start: 4, end: 6 };
  it("clamps linearly when not looping", () => {
    expect(loopedPosition(5, region, false, 10)).toBe(5);
    expect(loopedPosition(99, region, false, 10)).toBe(10);
  });
  it("clamps linearly when looping without a region", () => {
    expect(loopedPosition(5, null, true, 10)).toBe(5);
  });
  it("plays linearly into the region from before it", () => {
    // A playhead before the region advances straight into it — no wrap yet.
    expect(loopedPosition(2, region, true, 10)).toBe(2);
    expect(loopedPosition(4, region, true, 10)).toBe(4);
  });
  it("advances linearly through the first pass of the region", () => {
    expect(loopedPosition(5, region, true, 10)).toBe(5);
  });
  it("wraps back to the region start once past the region end", () => {
    // Just past the end folds back to just past the start (sample-accurate wrap).
    expect(loopedPosition(6.5, region, true, 10)).toBeCloseTo(4.5, 6);
  });
  it("wraps repeatedly across many loop lengths", () => {
    // start=4, len=2: linear 10 => 4 + ((10-4) % 2) = 4.
    expect(loopedPosition(10, region, true, 10)).toBeCloseTo(4, 6);
    expect(loopedPosition(11, region, true, 10)).toBeCloseTo(5, 6);
  });
  it("clamps linearly for a degenerate zero-length region", () => {
    expect(loopedPosition(7, { start: 5, end: 5 }, true, 10)).toBe(7);
  });
});

describe("formatTime", () => {
  it("formats minutes, seconds, and milliseconds", () => {
    expect(formatTime(0)).toBe("0:00.000");
    expect(formatTime(65.25)).toBe("1:05.250");
  });
  it("never renders a negative readout", () => {
    expect(formatTime(-1)).toBe("0:00.000");
  });
  it("carries rounded milliseconds across the minute boundary", () => {
    expect(formatTime(59.9996)).toBe("1:00.000");
  });
});
