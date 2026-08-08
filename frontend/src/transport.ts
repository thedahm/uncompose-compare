/**
 * Pure transport arithmetic for the M3 playback workbench (issue #12).
 *
 * These are the position/step/crossfade/waveform functions the workbench's Web
 * Audio graph and its React shell are built on. They are kept side-effect-free
 * so they can be unit-tested off a browser; the audio graph that consumes them
 * (`engine.ts`) is certified cross-engine by the sync harness, not re-tested
 * here.
 */

/** The two candidate labels, in argument order (CONTEXT.md: the reference key). */
export type Label = "A" | "B";

/** The other candidate — the target of an A/B switch. */
export function otherLabel(label: Label): Label {
  return label === "A" ? "B" : "A";
}

/**
 * A drag-selected loop region (seconds). The UI term is "region" (drag-to-
 * select); the comparison-record schema stores it under `loops[]` — CONTEXT.md
 * records the mapping so neither term drifts.
 */
export interface Region {
  start: number;
  end: number;
}

/**
 * Turn the two endpoints of a drag into an ordered, in-track region: the earlier
 * point is `start`, the later `end`, both clamped to `[0, duration]`. A drag left
 * or right lands on the same region.
 */
export function orderedRegion(a: number, b: number, duration: number): Region {
  return {
    start: clampPosition(Math.min(a, b), duration),
    end: clampPosition(Math.max(a, b), duration),
  };
}

/**
 * Map a linear playback offset to the audible position under DAW-style looping.
 *
 * With looping off (or no region), position is just the offset clamped to the
 * track. With looping on, a playhead before the region plays linearly *into* it
 * and through its first pass (so `linear < region.end` stays linear — the
 * "play-into" semantics); once the offset passes the region end it folds back
 * onto `[start, end)` by the loop length, matching the sample-accurate wrap the
 * Web Audio source performs natively. This mirrors `engine.ts`'s native
 * `loopStart`/`loopEnd` so the rendered playhead tracks the audio.
 */
export function loopedPosition(
  linear: number,
  region: Region | null,
  looping: boolean,
  duration: number,
): number {
  if (!looping || !region) return clampPosition(linear, duration);
  const len = region.end - region.start;
  if (len <= 0) return clampPosition(linear, duration);
  if (linear < region.end) return clampPosition(linear, duration);
  return region.start + ((linear - region.start) % len);
}

/** Clamp a playback position (seconds) into `[0, duration]`. */
export function clampPosition(pos: number, duration: number): number {
  if (pos < 0) return 0;
  if (pos > duration) return duration;
  return pos;
}

/** Step a position by `delta` seconds, clamped to the track (2 s transport step). */
export function stepPosition(pos: number, delta: number, duration: number): number {
  return clampPosition(pos + delta, duration);
}

/**
 * Equal-power crossfade curves for `setValueCurveAtTime`: `up` rises 0→1 as the
 * incoming lane fades in, `down` (its mirror) falls 1→0 as the outgoing lane
 * fades out. Equal-power means `up^2 + down^2 == 1` at every step, so the summed
 * loudness stays constant through the ~10 ms switch and no click or dip is heard.
 */
export function equalPowerCurves(steps: number): { up: Float32Array; down: Float32Array } {
  const n = Math.max(2, steps);
  const up = new Float32Array(n);
  const down = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    const t = (i / (n - 1)) * (Math.PI / 2);
    up[i] = Math.sin(t);
    down[i] = Math.cos(t);
  }
  return { up, down };
}

/**
 * Reduce a channel's samples to `buckets` min/max pairs for waveform drawing.
 * Each bucket spans an equal slice of the samples; drawing a vertical line from
 * `min[i]` to `max[i]` gives the familiar filled-envelope waveform without
 * touching every sample at paint time.
 */
export function computePeaks(
  data: Float32Array,
  buckets: number,
): { min: Float32Array; max: Float32Array } {
  const min = new Float32Array(buckets);
  const max = new Float32Array(buckets);
  const per = data.length / buckets;
  for (let b = 0; b < buckets; b++) {
    const start = Math.floor(b * per);
    const end = Math.min(data.length, Math.max(start + 1, Math.floor((b + 1) * per)));
    // A bucket that falls past the data (short input) stays at silence.
    let lo = start < data.length ? data[start] : 0;
    let hi = lo;
    for (let i = start + 1; i < end; i++) {
      const v = data[i];
      if (v < lo) lo = v;
      if (v > hi) hi = v;
    }
    min[b] = lo;
    max[b] = hi;
  }
  return { min, max };
}

/** Format a position (seconds) as `m:ss.mmm`, never negative. */
export function formatTime(sec: number): string {
  // Round once to whole milliseconds, then decompose, so rounding carries
  // cleanly through every unit (59.9996 s reads "1:00.000", not "0:60.000").
  const totalMs = Math.round(Math.max(0, sec) * 1000);
  const minutes = Math.floor(totalMs / 60_000);
  const seconds = Math.floor((totalMs % 60_000) / 1000);
  const ms = totalMs % 1000;
  return `${minutes}:${String(seconds).padStart(2, "0")}.${String(ms).padStart(3, "0")}`;
}
