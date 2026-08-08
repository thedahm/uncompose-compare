/**
 * Spike 4 stub-page plumbing: the Web Audio dual-source graph the #73 sync
 * harness exercises, living in the *served page* rather than injected by the
 * test (spec #1, story 4; issue #5).
 *
 * The harness (copied from `thedahm/uncompose` `wayfinder/73-sync-spike`) now
 * targets the packaged binary's served page: it hands this function the
 * seeded-noise fixtures as base64, this page decodes them and renders the real
 * playback-graph shape (three sample-locked buffer sources into per-lane gains,
 * SRC muted, a mid-render 10 ms linear crossfade A→B) in an
 * `OfflineAudioContext`, and returns compact metrics. The Playwright side
 * asserts the three contract points (zero-lag correlation peak, exact decode
 * sample counts, bit-identical output outside the fade window).
 *
 * Keeping the plumbing in the page — not the test — is the point of #5: the
 * thing under test is what the wheel actually ships, exercised the way a
 * browser loads it.
 */

/** What the harness passes in: base64 fixtures plus the render geometry. */
export interface SyncParams {
  fixtures: Record<string, string>;
  SR: number;
  FRAMES: number;
  SWITCH_SAMPLE: number;
  FADE_SAMPLES: number;
}

interface DecodeInfo {
  length?: number;
  sampleRate?: number;
  channels?: number;
  error?: string;
}

interface ChannelIdentity {
  ch: number;
  preMismatch: number;
  postMismatch: number;
  firstPre: number;
  firstPost: number;
  maxDiff: number;
}

interface Correlation {
  bestLag: number | null;
  peakRatio: number;
  zeroVal: number | null;
  bestVal: number;
}

export interface SyncResult {
  decodeInfo: Record<string, DecodeInfo>;
  identity?: ChannelIdentity[];
  correlation?: Correlation;
  renderSkipped?: boolean;
}

/**
 * Decode the fixtures, render the dual-source crossfade graph, and return the
 * metrics the harness asserts over. Mirrors the reference #73 graph shape so
 * packaging is proven not to perturb the sync contract.
 */
export async function renderSync({
  fixtures,
  SR,
  FRAMES,
  SWITCH_SAMPLE,
  FADE_SAMPLES,
}: SyncParams): Promise<SyncResult> {
  const toArrayBuffer = (base64: string): ArrayBuffer => {
    const bin = atob(base64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes.buffer;
  };

  // decodeAudioData resamples to the context rate, so the context must be
  // built at the fixture rate or the decode-count canary is meaningless.
  const ctx = new OfflineAudioContext(2, FRAMES, SR);
  const decoded: Record<string, AudioBuffer> = {};
  const decodeInfo: Record<string, DecodeInfo> = {};
  for (const [name, base64] of Object.entries(fixtures)) {
    try {
      const buf = await ctx.decodeAudioData(toArrayBuffer(base64));
      decoded[name] = buf;
      decodeInfo[name] = {
        length: buf.length,
        sampleRate: buf.sampleRate,
        channels: buf.numberOfChannels,
      };
    } catch (e) {
      decodeInfo[name] = { error: String(e) };
    }
  }
  if (!decoded["a.wav"] || !decoded["b.wav"]) {
    return { decodeInfo, renderSkipped: true };
  }

  // Real playback-graph shape: three sample-locked sources (SRC muted) into
  // per-lane gains, linear crossfade A→B over FADE_SAMPLES.
  const tSwitch = SWITCH_SAMPLE / SR;
  const tEnd = (SWITCH_SAMPLE + FADE_SAMPLES) / SR;
  const lanes = [
    { buf: decoded["a.wav"], g0: 1, g1: 0 }, // A
    { buf: decoded["b.wav"], g0: 0, g1: 1 }, // B
    { buf: decoded["a.wav"], g0: 0, g1: 0 }, // SRC, muted throughout
  ];
  for (const lane of lanes) {
    const src = ctx.createBufferSource();
    src.buffer = lane.buf;
    const gain = ctx.createGain();
    gain.gain.setValueAtTime(lane.g0, 0);
    gain.gain.setValueAtTime(lane.g0, tSwitch);
    gain.gain.linearRampToValueAtTime(lane.g1, tEnd);
    src.connect(gain).connect(ctx.destination);
    src.start(0);
  }
  const rendered = await ctx.startRendering();

  // Bit-identity outside the fade window, per channel: rendered === A before
  // the switch, rendered === B after the ramp end. Exact float compare.
  const identity: ChannelIdentity[] = [];
  for (let ch = 0; ch < 2; ch++) {
    const out = rendered.getChannelData(ch);
    const a = decoded["a.wav"].getChannelData(ch);
    const b = decoded["b.wav"].getChannelData(ch);
    let preMismatch = 0;
    let postMismatch = 0;
    let firstPre = -1;
    let firstPost = -1;
    let maxDiff = 0;
    for (let i = 0; i < SWITCH_SAMPLE; i++) {
      if (out[i] !== a[i]) {
        preMismatch++;
        if (firstPre < 0) firstPre = i;
        maxDiff = Math.max(maxDiff, Math.abs(out[i] - a[i]));
      }
    }
    for (let i = SWITCH_SAMPLE + FADE_SAMPLES; i < FRAMES; i++) {
      if (out[i] !== b[i]) {
        postMismatch++;
        if (firstPost < 0) firstPost = i;
        maxDiff = Math.max(maxDiff, Math.abs(out[i] - b[i]));
      }
    }
    identity.push({ ch, preMismatch, postMismatch, firstPre, firstPost, maxDiff });
  }

  // Cross-correlation of the post-switch region against expected candidate B,
  // lags -256..256; the contract wants the peak at exactly lag 0.
  const out = rendered.getChannelData(0);
  const b = decoded["b.wav"].getChannelData(0);
  const start = SWITCH_SAMPLE + FADE_SAMPLES + 256;
  const len = FRAMES - start - 256;
  let bestLag: number | null = null;
  let bestVal = -Infinity;
  let zeroVal: number | null = null;
  let secondVal = -Infinity;
  for (let lag = -256; lag <= 256; lag++) {
    let sum = 0;
    for (let i = 0; i < len; i++) sum += out[start + i] * b[start + i + lag];
    if (lag === 0) zeroVal = sum;
    if (sum > bestVal) {
      secondVal = bestVal;
      bestVal = sum;
      bestLag = lag;
    } else if (sum > secondVal) {
      secondVal = sum;
    }
  }
  const correlation: Correlation = {
    bestLag,
    peakRatio: bestVal / Math.max(secondVal, 1e-12),
    zeroVal,
    bestVal,
  };

  return { decodeInfo, identity, correlation };
}

declare global {
  interface Window {
    /** The seam the #73 harness drives on the served page. */
    __uncomposeSync?: (params: SyncParams) => Promise<SyncResult>;
  }
}

/** Register the harness seam on the served page. */
export function installSyncHarness(): void {
  window.__uncomposeSync = renderSync;
}
