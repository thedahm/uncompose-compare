/**
 * The sample-locked A/B playback engine for the M3 workbench (issue #12).
 *
 * Both candidates play in ONE Web Audio graph on a shared transport: whenever
 * playback runs, an `AudioBufferSourceNode` for each candidate is started at the
 * SAME offset into a per-candidate `GainNode`, so A and B are sample-locked and
 * switching the audible one never shifts the playback position. Switching is a
 * ~10 ms equal-power crossfade on the two gains (the sources are untouched), so
 * it is instant and gap-free. Seeking/stepping restarts both sources together at
 * the new offset (a buffer source cannot be repositioned in place).
 *
 * Decoded PCM lives only in the `AudioBuffer`s here, in browser memory; nothing
 * is written to disk client-side (acceptance criterion). The pure arithmetic
 * (clamping, stepping, the crossfade curve) lives in `transport.ts`; this module
 * is the imperative shell around the Web Audio nodes and is exercised by the
 * browser flow spec, not unit tests (no Web Audio off a browser).
 */
import { clampPosition, equalPowerCurves, type Label, otherLabel, stepPosition } from "./transport";

/** The equal-power switch fade, ~10 ms — matches the sync contract's bound. */
const FADE_SECONDS = 0.01;
/**
 * The crossfade curves, 64 steps, computed once — `setValueCurveAtTime` copies
 * the arrays, so sharing them across switches is safe.
 */
const FADE_CURVES = equalPowerCurves(64);

export class PlaybackEngine {
  private ctx: AudioContext;
  private buffers: Record<Label, AudioBuffer>;
  private gains: Record<Label, GainNode>;
  private sources: Record<Label, AudioBufferSourceNode> | null = null;

  private liveLabel: Label = "A";
  private playing = false;
  /** Frozen position (seconds) while stopped; the resume/seek point. */
  private pausePos = 0;
  /** `ctx.currentTime` when the current sources started. */
  private startedAt = 0;
  /** Playback offset (seconds) the current sources started at. */
  private startOffset = 0;
  /** Bumped on every (re)start so a stale `onended` cannot fire transport logic. */
  private generation = 0;

  /** Notified when playback ends on its own (reaches the end of the track). */
  onEnded: (() => void) | null = null;

  constructor(ctx: AudioContext, a: AudioBuffer, b: AudioBuffer) {
    this.ctx = ctx;
    this.buffers = { A: a, B: b };
    this.gains = {
      A: ctx.createGain(),
      B: ctx.createGain(),
    };
    // Live lane audible, the other silent, until the first switch.
    this.gains.A.gain.value = 1;
    this.gains.B.gain.value = 0;
    this.gains.A.connect(ctx.destination);
    this.gains.B.connect(ctx.destination);
  }

  /** Transport length: the longer candidate (they start locked at 0). */
  duration(): number {
    return Math.max(this.buffers.A.duration, this.buffers.B.duration);
  }

  live(): Label {
    return this.liveLabel;
  }

  isPlaying(): boolean {
    return this.playing;
  }

  /** Current playback position in seconds, live while playing. */
  position(): number {
    if (!this.playing) return this.pausePos;
    const elapsed = this.ctx.currentTime - this.startedAt;
    return clampPosition(this.startOffset + elapsed, this.duration());
  }

  /** Space: start from the frozen position (resumes a suspended context). */
  play(): void {
    if (this.playing) return;
    // A play from the very end rewinds to the start (DAW-like) rather than
    // starting sources that would never run.
    if (this.pausePos >= this.duration()) this.pausePos = 0;
    void this.ctx.resume();
    this.startSources(this.pausePos);
    this.playing = true;
  }

  /**
   * Space (while playing): stop. `returnToStart` is the "stop returns" toggle —
   * true rewinds to 0, false leaves the playhead where it stopped so the next
   * play resumes.
   */
  stop(returnToStart: boolean): void {
    if (!this.playing) {
      if (returnToStart) this.pausePos = 0;
      return;
    }
    const pos = this.position();
    this.teardownSources();
    this.playing = false;
    this.pausePos = returnToStart ? 0 : pos;
  }

  /** Space: play if stopped, stop (honoring the toggle) if playing. */
  togglePlay(returnToStart: boolean): void {
    if (this.playing) this.stop(returnToStart);
    else this.play();
  }

  /** Seek to an absolute position; restarts both sources if playing. */
  seek(pos: number): void {
    const clamped = clampPosition(pos, this.duration());
    if (this.playing) {
      if (clamped >= this.duration()) {
        // Seeking to the very end stops there cleanly rather than starting
        // sources past their span (which would never fire `onended`).
        this.teardownSources();
        this.playing = false;
        this.pausePos = this.duration();
        this.onEnded?.();
      } else {
        this.startSources(clamped);
      }
    } else {
      this.pausePos = clamped;
    }
  }

  /** Step by `delta` seconds (the ±2 s transport step), clamped to the track. */
  step(delta: number): void {
    this.seek(stepPosition(this.position(), delta, this.duration()));
  }

  /** Home: return to the start. */
  rewind(): void {
    this.seek(0);
  }

  /** `x` / lane click: make `label` the audible candidate with a crossfade. */
  switchTo(label: Label): void {
    if (label === this.liveLabel) return;
    const outgoing = this.liveLabel;
    this.liveLabel = label;
    const now = this.ctx.currentTime;
    if (this.playing) {
      const { up, down } = FADE_CURVES;
      this.gains[label].gain.cancelScheduledValues(now);
      this.gains[outgoing].gain.cancelScheduledValues(now);
      // Anchor the ramp at the present value so the curve starts from "now".
      this.gains[label].gain.setValueAtTime(this.gains[label].gain.value, now);
      this.gains[outgoing].gain.setValueAtTime(this.gains[outgoing].gain.value, now);
      this.gains[label].gain.setValueCurveAtTime(up, now, FADE_SECONDS);
      this.gains[outgoing].gain.setValueCurveAtTime(down, now, FADE_SECONDS);
    } else {
      this.gains[label].gain.value = 1;
      this.gains[outgoing].gain.value = 0;
    }
  }

  /** `x`: switch to whichever candidate is not currently live. */
  toggleSwitch(): void {
    this.switchTo(otherLabel(this.liveLabel));
  }

  private startSources(offset: number): void {
    this.teardownSources();
    const generation = ++this.generation;
    const now = this.ctx.currentTime;
    const sources: Record<Label, AudioBufferSourceNode> = {
      A: this.ctx.createBufferSource(),
      B: this.ctx.createBufferSource(),
    };
    (["A", "B"] as Label[]).forEach((label) => {
      const src = sources[label];
      src.buffer = this.buffers[label];
      src.connect(this.gains[label]);
      // Restore the steady-state gains (a mid-switch restart lands on the live
      // lane fully up, the other fully down) so a seek never leaves a fade half
      // applied.
      this.gains[label].gain.cancelScheduledValues(now);
      this.gains[label].gain.setValueAtTime(label === this.liveLabel ? 1 : 0, now);
      src.onended = () => {
        // Only the freshest generation drives transport state; a stop/seek that
        // tore these sources down has already moved on.
        if (generation !== this.generation) return;
        this.playing = false;
        this.pausePos = this.duration();
        this.sources = null;
        this.onEnded?.();
      };
      // A source shorter than the transport is started only within its own span.
      const bufferDur = this.buffers[label].duration;
      if (offset < bufferDur) src.start(0, offset);
    });
    this.sources = sources;
    this.startedAt = now;
    this.startOffset = offset;
  }

  private teardownSources(): void {
    if (!this.sources) return;
    // Bump the generation first so the stop()-triggered onended is ignored.
    this.generation++;
    (["A", "B"] as Label[]).forEach((label) => {
      const src = this.sources![label];
      src.onended = null;
      try {
        src.stop();
      } catch {
        // Already stopped / never started (offset past its buffer) — fine.
      }
      src.disconnect();
    });
    this.sources = null;
  }
}
