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
    let lo = 0;
    let hi = 0;
    let seen = false;
    for (let i = start; i < end; i++) {
      const v = data[i];
      if (!seen) {
        lo = v;
        hi = v;
        seen = true;
      } else {
        if (v < lo) lo = v;
        if (v > hi) hi = v;
      }
    }
    min[b] = lo;
    max[b] = hi;
  }
  return { min, max };
}

/** Format a position (seconds) as `m:ss.mmm`, never negative. */
export function formatTime(sec: number): string {
  const t = sec > 0 ? sec : 0;
  const minutes = Math.floor(t / 60);
  const seconds = Math.floor(t % 60);
  const millis = Math.round((t - Math.floor(t)) * 1000);
  // Rounding can carry milliseconds to 1000; fold it into the next second.
  const ms = millis === 1000 ? 0 : millis;
  const carry = millis === 1000 ? 1 : 0;
  const s = seconds + carry;
  return `${minutes}:${String(s).padStart(2, "0")}.${String(ms).padStart(3, "0")}`;
}
