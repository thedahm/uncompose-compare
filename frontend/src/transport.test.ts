import { describe, expect, it } from "vitest";
import {
  clampFraction,
  clampPosition,
  dbToGain,
  loopBounds,
  computeLoudness,
  computePeaks,
  computeSpectrogram,
  equalPowerCurves,
  fft,
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

describe("dbToGain", () => {
  it("maps 0 dB to unity gain", () => {
    expect(dbToGain(0)).toBe(1);
  });
  it("maps -6 dB to roughly half amplitude", () => {
    expect(dbToGain(-6.0206)).toBeCloseTo(0.5, 4);
  });
  it("attenuates below unity for any negative gain", () => {
    expect(dbToGain(-2.4)).toBeLessThan(1);
    expect(dbToGain(-2.4)).toBeGreaterThan(0);
  });
});

describe("clampFraction", () => {
  it("keeps a fraction inside 0-1", () => {
    expect(clampFraction(0.25)).toBe(0.25);
  });
  it("clamps outside 0-1 to the ends", () => {
    expect(clampFraction(-0.5)).toBe(0);
    expect(clampFraction(1.5)).toBe(1);
  });
});

describe("loopBounds", () => {
  it("passes a region that fits both candidates through untouched", () => {
    expect(loopBounds({ start: 1, end: 2 }, 3)).toEqual({ start: 1, end: 2 });
  });
  it("clamps a region running past the shorter candidate", () => {
    // Web Audio would clamp each source to its own buffer, so the shorter lane
    // would loop over a shorter span and drift; one shared end keeps them locked.
    expect(loopBounds({ start: 1, end: 5 }, 3)).toEqual({ start: 1, end: 3 });
  });
  it("refuses to loop a region entirely past the shorter candidate", () => {
    expect(loopBounds({ start: 4, end: 5 }, 3)).toBeNull();
  });
  it("has nothing to loop without a region", () => {
    expect(loopBounds(null, 3)).toBeNull();
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

describe("computeLoudness", () => {
  it("reduces samples to one RMS value per bucket", () => {
    // A ±1 square wave has RMS 1; a ±0.5 one has RMS 0.5.
    const data = new Float32Array([1, -1, 0.5, -0.5]);
    const rms = computeLoudness(data, 2);
    expect(rms).toHaveLength(2);
    expect(rms[0]).toBeCloseTo(1, 6);
    expect(rms[1]).toBeCloseTo(0.5, 6);
  });
  it("returns the RMS of a constant signal as its magnitude", () => {
    const rms = computeLoudness(new Float32Array([0.25, 0.25, 0.25, 0.25]), 1);
    expect(rms[0]).toBeCloseTo(0.25, 6);
  });
  it("emits exactly `buckets` values even for short data", () => {
    expect(computeLoudness(new Float32Array([0.2]), 8)).toHaveLength(8);
  });
});

describe("fft", () => {
  it("puts all energy in bin 0 for a DC signal", () => {
    const n = 8;
    const re = new Float32Array(n).fill(1);
    const im = new Float32Array(n);
    fft(re, im);
    expect(re[0]).toBeCloseTo(n, 5); // sum of ones
    for (let k = 1; k < n; k++) {
      expect(Math.hypot(re[k], im[k])).toBeCloseTo(0, 5);
    }
  });
  it("peaks at the bin matching a pure sinusoid's frequency", () => {
    const n = 64;
    const k = 5;
    const re = new Float32Array(n);
    const im = new Float32Array(n);
    for (let i = 0; i < n; i++) re[i] = Math.sin((2 * Math.PI * k * i) / n);
    fft(re, im);
    let argmax = 0;
    let peak = -1;
    for (let b = 0; b < n / 2; b++) {
      const mag = Math.hypot(re[b], im[b]);
      if (mag > peak) {
        peak = mag;
        argmax = b;
      }
    }
    expect(argmax).toBe(k);
  });
});

describe("computeSpectrogram", () => {
  it("produces a columns × bins grid (bins = fftSize / 2)", () => {
    const spec = computeSpectrogram(new Float32Array(1024), 4, 64);
    expect(spec.columns).toBe(4);
    expect(spec.bins).toBe(32);
    expect(spec.data).toHaveLength(4 * 32);
  });
  it("concentrates energy in the bin matching a steady sinusoid", () => {
    // 8 cycles per 64-sample window => a peak at bin 8 in every column.
    const len = 512;
    const data = new Float32Array(len);
    for (let i = 0; i < len; i++) data[i] = Math.sin((2 * Math.PI * 8 * i) / 64);
    const spec = computeSpectrogram(data, 4, 64);
    let argmax = 0;
    let peak = -1;
    for (let b = 0; b < spec.bins; b++) {
      const v = spec.data[b]; // first column
      if (v > peak) {
        peak = v;
        argmax = b;
      }
    }
    expect(argmax).toBe(8);
  });
  it("normalizes every value into [0, 1]", () => {
    const len = 256;
    const data = new Float32Array(len);
    for (let i = 0; i < len; i++) data[i] = Math.sin((2 * Math.PI * 4 * i) / 32);
    const spec = computeSpectrogram(data, 3, 32);
    for (const v of spec.data) {
      expect(v).toBeGreaterThanOrEqual(0);
      expect(v).toBeLessThanOrEqual(1);
    }
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
