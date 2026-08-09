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

/**
 * A playback lane: the two comparison candidates plus the optional SRC lane —
 * the shared source both candidates are compared against (spec #42). SRC joins
 * the sample-locked graph and can be auditioned, but it is not a `Label`: it
 * never wins a preference and never enters the record's `candidates[]`.
 */
export type LaneId = Label | "SRC";

/**
 * Which visualization every audio display shows. The toggle applies to all
 * displays at once (stage + lanes): the raw sample envelope, the RMS loudness
 * envelope, or the FFT spectrogram.
 */
export type ViewMode = "waveform" | "loudness" | "spectral";

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

/**
 * A decibel gain as a linear amplitude factor: `10^(db/20)` (issue #30). 0 dB is
 * unity; the negative gains loudness matching applies attenuate below 1. Used to
 * turn the session's per-lane `gain_db` into the engine's static lane gains.
 */
export function dbToGain(db: number): number {
  return Math.pow(10, db / 20);
}

/** Clamp a playback position (seconds) into `[0, duration]`. */
export function clampPosition(pos: number, duration: number): number {
  if (pos < 0) return 0;
  if (pos > duration) return duration;
  return pos;
}

/**
 * Clamp a unitless 0–1 fraction — a pointer position across a display, a
 * normalized magnitude. Same arithmetic as `clampPosition`, different quantity:
 * keeping them apart is what stops "position (seconds)" from reading as a lie.
 */
export function clampFraction(value: number): number {
  if (value < 0) return 0;
  if (value > 1) return 1;
  return value;
}

/**
 * The loop bounds actually applied to both lanes, given the shortest candidate.
 *
 * Web Audio clamps a source's `loopEnd` to that source's own buffer duration.
 * With candidates of different lengths (issue #15's mismatch case) a region
 * running past the shorter one would therefore loop over a *shorter* span on
 * that lane — the two lanes would drift apart, and away from the drawn playhead.
 * Clamping the loop to the shorter candidate keeps A and B sample-locked, which
 * is the contract that matters; a region entirely past the shorter candidate's
 * end cannot be looped in lock at all, so it returns null and playback runs
 * through linearly.
 */
export function loopBounds(region: Region | null, shortest: number): Region | null {
  if (!region) return null;
  const end = Math.min(region.end, shortest);
  return end > region.start ? { start: region.start, end } : null;
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

/** A min/max peak envelope: one pair per bucket, for waveform drawing. */
export interface Peaks {
  min: Float32Array;
  max: Float32Array;
}

/**
 * The sample range `[start, end)` covered by bucket `b` of an equal split into
 * buckets of `per` samples each: at least one sample wide, clamped to the data.
 * Shared by the peak and loudness reducers so both views bucket identically.
 */
function bucketRange(b: number, per: number, length: number): { start: number; end: number } {
  const start = Math.floor(b * per);
  const end = Math.min(length, Math.max(start + 1, Math.floor((b + 1) * per)));
  return { start, end };
}

/**
 * Reduce a channel's samples to `buckets` min/max pairs for waveform drawing.
 * Each bucket spans an equal slice of the samples; drawing a vertical line from
 * `min[i]` to `max[i]` gives the familiar filled-envelope waveform without
 * touching every sample at paint time.
 */
export function computePeaks(data: Float32Array, buckets: number): Peaks {
  const min = new Float32Array(buckets);
  const max = new Float32Array(buckets);
  const per = data.length / buckets;
  for (let b = 0; b < buckets; b++) {
    const { start, end } = bucketRange(b, per, data.length);
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

/**
 * Reduce a channel's samples to `buckets` RMS values for the loudness view —
 * the root-mean-square level over each equal slice of the samples, in the same
 * [0, 1] magnitude scale as the peaks. Drawing a symmetric envelope from these
 * gives the "loudness" (RMS) view: the same time axis as the waveform, but the
 * energy rather than the raw sample extremes. Computed once per decoded buffer.
 */
export function computeLoudness(data: Float32Array, buckets: number): Float32Array {
  const rms = new Float32Array(buckets);
  const per = data.length / buckets;
  for (let b = 0; b < buckets; b++) {
    const { start, end } = bucketRange(b, per, data.length);
    let sum = 0;
    for (let i = start; i < end; i++) {
      sum += data[i] * data[i];
    }
    rms[b] = end > start ? Math.sqrt(sum / (end - start)) : 0;
  }
  return rms;
}

/**
 * In-place radix-2 Cooley–Tukey FFT (decimation-in-time). `re`/`im` are the
 * real and imaginary parts of the input, overwritten with the transform; their
 * length MUST be a power of two. This is the real-FFT engine behind the spectral
 * view — a self-contained transform so nothing but the decoded PCM is needed and
 * no library is fetched over the network.
 */
export function fft(re: Float32Array, im: Float32Array): void {
  const n = re.length;
  // Bit-reversal permutation.
  for (let i = 1, j = 0; i < n; i++) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) {
      const tr = re[i];
      re[i] = re[j];
      re[j] = tr;
      const ti = im[i];
      im[i] = im[j];
      im[j] = ti;
    }
  }
  // Butterflies, doubling the transform length each stage.
  for (let len = 2; len <= n; len <<= 1) {
    const ang = (-2 * Math.PI) / len;
    const wr = Math.cos(ang);
    const wi = Math.sin(ang);
    const half = len >> 1;
    for (let i = 0; i < n; i += len) {
      let cr = 1;
      let ci = 0;
      for (let k = 0; k < half; k++) {
        const a = i + k;
        const b = a + half;
        const vr = re[b] * cr - im[b] * ci;
        const vi = re[b] * ci + im[b] * cr;
        re[b] = re[a] - vr;
        im[b] = im[a] - vi;
        re[a] += vr;
        im[a] += vi;
        const ncr = cr * wr - ci * wi;
        ci = cr * wi + ci * wr;
        cr = ncr;
      }
    }
  }
}

/** A precomputed FFT spectrogram: a `columns × bins` grid, each cell in [0, 1]. */
export interface Spectrogram {
  columns: number;
  bins: number;
  /** Row-major by column: `data[col * bins + bin]`, dB-normalized to [0, 1]. */
  data: Float32Array;
}

/**
 * The three visualizations of one candidate, each computed once per decoded
 * buffer and kept in browser memory only (privacy contract). The view toggle
 * picks which of these every display draws.
 */
export interface CandidateViews {
  peaks: Peaks;
  loudness: Float32Array;
  spectral: Spectrogram;
}

/** dB floor for the spectral view: energy this far below full scale reads black. */
const SPECTRAL_DB_FLOOR = 100;

/**
 * Compute a real FFT spectrogram of a channel for the spectral view: `columns`
 * evenly spaced Hann-windowed frames of `fftSize` samples, each transformed to
 * `fftSize / 2` magnitude bins in dB, normalized to [0, 1] against a fixed floor.
 * `bin` 0 is DC (lowest frequency); the renderer flips the axis so highs sit on
 * top. Computed once per decoded buffer and kept in browser memory only.
 */
export function computeSpectrogram(
  data: Float32Array,
  columns: number,
  fftSize: number,
): Spectrogram {
  const bins = fftSize >> 1;
  const out = new Float32Array(columns * bins);
  const re = new Float32Array(fftSize);
  const im = new Float32Array(fftSize);
  // Hann window over the frame, tapering edges to suppress spectral leakage.
  const win = new Float32Array(fftSize);
  for (let i = 0; i < fftSize; i++) {
    win[i] = 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / (fftSize - 1));
  }
  const span = Math.max(0, data.length - fftSize);
  for (let c = 0; c < columns; c++) {
    const start = columns > 1 ? Math.floor((c * span) / (columns - 1)) : 0;
    for (let i = 0; i < fftSize; i++) {
      const s = start + i;
      re[i] = s < data.length ? data[s] * win[i] : 0;
      im[i] = 0;
    }
    fft(re, im);
    for (let k = 0; k < bins; k++) {
      const mag = Math.hypot(re[k], im[k]) / bins;
      const db = 20 * Math.log10(mag + 1e-9);
      out[c * bins + k] = clampFraction((db + SPECTRAL_DB_FLOOR) / SPECTRAL_DB_FLOOR);
    }
  }
  return { columns, bins, data: out };
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
