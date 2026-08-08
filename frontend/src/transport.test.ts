import { describe, expect, it } from "vitest";
import {
  clampPosition,
  computePeaks,
  equalPowerCurves,
  formatTime,
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

describe("formatTime", () => {
  it("formats minutes, seconds, and milliseconds", () => {
    expect(formatTime(0)).toBe("0:00.000");
    expect(formatTime(65.25)).toBe("1:05.250");
  });
  it("never renders a negative readout", () => {
    expect(formatTime(-1)).toBe("0:00.000");
  });
});
