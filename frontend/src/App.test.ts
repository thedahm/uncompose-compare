import { describe, expect, it } from "vitest";
import { formatLufs, lanesOf, type BlindSession, type SightedSession } from "./App";

/**
 * The blind/sighted narrowing the workbench branches on (#28). The browser flow
 * specs certify what the page *shows*; these cover the discrimination itself,
 * which decides whether a lane row can name a file at all.
 */
describe("lanesOf", () => {
  const blind: BlindSession = {
    blind: true,
    loudness_match: { enabled: false },
    candidates: [
      { label: "A", duration_ms: 1000, audio: "/audio/deadbeef" },
      { label: "B", duration_ms: 1000, audio: "/audio/feedface" },
    ],
  };

  const sighted: SightedSession = {
    loudness_match: { enabled: false },
    duration_mismatch: false,
    duration_delta_ms: 0,
    duration_delta_samples: 0,
    sample_rate_mismatch: false,
    channel_count_mismatch: false,
    candidates: [
      {
        label: "A",
        duration_ms: 1000,
        audio: "/audio/aaa",
        name: "alpha.wav",
        path: "/tmp/alpha.wav",
        sha256: "aaa",
        size: 100,
        frames: 44_100,
        sample_rate: 44_100,
        channels: 2,
      },
      {
        label: "B",
        duration_ms: 1000,
        audio: "/audio/bbb",
        name: "bravo.wav",
        path: "/tmp/bravo.wav",
        sha256: "bbb",
        size: 101,
        frames: 44_100,
        sample_rate: 44_100,
        channels: 2,
      },
    ],
  };

  it("keys the lanes by label rather than by position", () => {
    // Argument order is not label order — the blind shuffle is exactly that
    // case — so the page must resolve A and B by name, never by index.
    const reversed: SightedSession = {
      ...sighted,
      candidates: [sighted.candidates[1], sighted.candidates[0]],
    };
    const lanes = lanesOf(reversed);
    expect(lanes?.blind).toBe(false);
    expect(lanes && !lanes.blind && lanes.byLabel.A.name).toBe("alpha.wav");
    expect(lanes && !lanes.blind && lanes.byLabel.B.name).toBe("bravo.wav");
  });

  it("carries the blind discriminator, so the identified branch is unreachable", () => {
    const lanes = lanesOf(blind);
    expect(lanes?.blind).toBe(true);
    // A blind payload has no identity to hand out: the lane object holds only
    // what the server sent (label, duration, opaque reference).
    expect(lanes && lanes.blind && Object.keys(lanes.byLabel.A).sort()).toEqual([
      "audio",
      "duration_ms",
      "label",
    ]);
  });

  it("is null when the payload does not name both lanes", () => {
    expect(lanesOf({ ...blind, candidates: [blind.candidates[0]] })).toBeNull();
  });
});

describe("formatLufs", () => {
  it("renders a measurement", () => {
    expect(formatLufs(-13.64)).toBe("-13.6 LUFS");
  });

  it("states the absence for a lane the gate gave no reading for", () => {
    // The record writes null there (ADR-0003); the page must not render it as a
    // number — least of all as 0.0, the loudest reading there is.
    expect(formatLufs(null)).not.toContain("0.0");
    expect(formatLufs(null)).toBe(formatLufs(undefined));
    expect(formatLufs(null)).toMatch(/no LUFS reading/);
  });
});
