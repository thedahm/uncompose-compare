/**
 * Spike 4 stub-page plumbing: the Web Audio dual-source graph the #73 sync
 * harness exercises, living in the *served page* rather than injected by the
 * test (spec #1, story 4; issue #5).
 *
 * The harness (copied from `thedahm/uncompose` `wayfinder/73-sync-spike`) now
 * targets the packaged binary's served page: it hands this function the
 * seeded-noise fixtures as base64, this page decodes them and renders the real
 * playback-graph shape (three sample-locked buffer sources into per-lane gains,
 * a mid-render 10 ms linear crossfade A→B) in an `OfflineAudioContext`, and
 * returns compact metrics. The Playwright side asserts the three contract points
 * (zero-lag correlation peak, exact decode sample counts, bit-identical output
 * outside the fade window).
 *
 * Keeping the plumbing in the page — not the test — is the point of #5: the
 * thing under test is what the wheel actually ships, exercised the way a
 * browser loads it.
 *
 * The static-gain variant (issue #31, spec #26 story 30) re-runs the identical
 * zero-offset render with distinct per-lane *constant* attenuation applied — the
 * shape loudness matching (#66/#30) uses. Constant gain, unchanged topology, so
 * the zero-lag correlation peak and the crossfade bound must both survive; the
 * variant certifies that on every push so loudness matching can never regress
 * the sync promise unnoticed.
 *
 * The three-lane variant (spec #42, ADR-0009) re-runs the same render with the
 * SRC lane audible at its own static gain beside the switching pair — the shape
 * a project session plays. The source is a playback lane on the one transport
 * and a member of the loudness match group, so both contract points have to hold
 * with it sounding: the post-switch region must still peak at lag 0 against the
 * live candidate *and* against the source, and the output outside the fade
 * window must be bit-identical to the two-lane mix.
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

/** The metrics one render pass yields (unattenuated, or a static-gain variant). */
export interface VariantResult {
  identity: ChannelIdentity[];
  correlation: Correlation;
  /** The same region correlated against the source lane; absent when SRC is silent. */
  sourceCorrelation?: Correlation;
}

export interface SyncResult {
  decodeInfo: Record<string, DecodeInfo>;
  identity?: ChannelIdentity[];
  correlation?: Correlation;
  /**
   * The static-gain variant: the same zero-offset render with distinct constant
   * per-lane attenuation applied (issue #31). Absent when the render is skipped.
   */
  attenuated?: VariantResult;
  /**
   * The three-lane variant (spec #42): the same render with the SRC lane audible
   * beside the switching pair, each of the three at its own static loudness-match
   * gain. Absent when the render is skipped.
   */
  threeLane?: VariantResult;
  renderSkipped?: boolean;
}

/** Render geometry shared by every variant: SyncParams minus the fixtures. */
type Geometry = Omit<SyncParams, "fixtures">;

/**
 * Distinct constant per-lane gains. A and B crossfade; SRC holds its gain for
 * the whole render — it is a reference lane, never a switch target (ADR-0009) —
 * so `src: 0` is the two-lane contract and a non-zero `src` is a session whose
 * source is audible alongside the live candidate.
 */
interface LaneGains {
  a: number;
  b: number;
  src: number;
}

/**
 * Render the crossfade graph once with the given constant per-lane gains and
 * compute the identity + correlation metrics. `gains.a`/`gains.b` scale the A
 * and B lanes and `gains.src` the source lane; `1`/`1`/`0` is the faithful
 * unattenuated two-lane render. Powers of two keep the scaled comparison an
 * exact float multiply, so the crossfade bound stays bit-exact under
 * attenuation — and outside the fade window at most two lanes are ever non-zero,
 * so the expected mix is a single correctly-rounded float32 add whatever order
 * the engine sums in.
 */
async function renderVariant(
  geo: Geometry,
  decoded: Record<string, AudioBuffer>,
  gains: LaneGains,
): Promise<VariantResult> {
  const { SR, FRAMES, SWITCH_SAMPLE, FADE_SAMPLES } = geo;

  // decodeAudioData already resampled to SR; render at the same rate.
  const ctx = new OfflineAudioContext(2, FRAMES, SR);
  const tSwitch = SWITCH_SAMPLE / SR;
  const tEnd = (SWITCH_SAMPLE + FADE_SAMPLES) / SR;

  // Real playback-graph shape: three sample-locked sources into per-lane gains,
  // linear crossfade A→B over FADE_SAMPLES, SRC held at a constant gain. The
  // variant folds its constant attenuation into each lane's held pre/post gain —
  // the switch curve is unchanged in shape, only scaled, exactly as loudness
  // matching's static per-lane gain does (#66), SRC included (ADR-0009).
  const lanes = [
    { buf: decoded["a.wav"], g0: gains.a, g1: 0 }, // A
    { buf: decoded["b.wav"], g0: 0, g1: gains.b }, // B
    { buf: decoded["src.wav"], g0: gains.src, g1: gains.src }, // SRC, never switched
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

  // Bit-identity outside the fade window, per channel: before the switch the
  // output is A scaled by gains.a, after the ramp end it is B scaled by gains.b,
  // in both cases summed with the source lane at its held gain. Exact float
  // compare (the scale is exact for power-of-two gains, and the mix is one
  // correctly-rounded float32 add — a sample-shifted SRC lane would show up here
  // as a wall of mismatches).
  const identity: ChannelIdentity[] = [];
  for (let ch = 0; ch < 2; ch++) {
    const out = rendered.getChannelData(ch);
    const a = decoded["a.wav"].getChannelData(ch);
    const b = decoded["b.wav"].getChannelData(ch);
    const s = decoded["src.wav"].getChannelData(ch);
    const mixed = (live: number, gain: number, i: number) =>
      Math.fround(live * gain + s[i] * gains.src);
    let preMismatch = 0;
    let postMismatch = 0;
    let firstPre = -1;
    let firstPost = -1;
    let maxDiff = 0;
    for (let i = 0; i < SWITCH_SAMPLE; i++) {
      const expected = mixed(a[i], gains.a, i);
      if (out[i] !== expected) {
        preMismatch++;
        if (firstPre < 0) firstPre = i;
        maxDiff = Math.max(maxDiff, Math.abs(out[i] - expected));
      }
    }
    for (let i = SWITCH_SAMPLE + FADE_SAMPLES; i < FRAMES; i++) {
      const expected = mixed(b[i], gains.b, i);
      if (out[i] !== expected) {
        postMismatch++;
        if (firstPost < 0) firstPost = i;
        maxDiff = Math.max(maxDiff, Math.abs(out[i] - expected));
      }
    }
    identity.push({ ch, preMismatch, postMismatch, firstPre, firstPost, maxDiff });
  }

  // Cross-correlation of the post-switch region against expected candidate B,
  // lags -256..256; the contract wants the peak at exactly lag 0. A constant
  // lane gain scales every product equally, so it can move the peak's height but
  // never its lag — which is the whole point the variant certifies. When the
  // source lane is audible the same region is correlated against it too: SRC
  // rides the one transport, so it must peak at lag 0 in the same render (#66,
  // ADR-0009).
  const out = rendered.getChannelData(0);
  const start = SWITCH_SAMPLE + FADE_SAMPLES + 256;
  const len = FRAMES - start - 256;
  const correlate = (reference: Float32Array): Correlation => {
    let bestLag: number | null = null;
    let bestVal = -Infinity;
    let zeroVal: number | null = null;
    let secondVal = -Infinity;
    for (let lag = -256; lag <= 256; lag++) {
      let sum = 0;
      for (let i = 0; i < len; i++) sum += out[start + i] * reference[start + i + lag];
      if (lag === 0) zeroVal = sum;
      if (sum > bestVal) {
        secondVal = bestVal;
        bestVal = sum;
        bestLag = lag;
      } else if (sum > secondVal) {
        secondVal = sum;
      }
    }
    return { bestLag, peakRatio: bestVal / Math.max(secondVal, 1e-12), zeroVal, bestVal };
  };

  const correlation = correlate(decoded["b.wav"].getChannelData(0));
  const sourceCorrelation =
    gains.src === 0 ? undefined : correlate(decoded["src.wav"].getChannelData(0));

  return { identity, correlation, sourceCorrelation };
}

/**
 * Decode the fixtures, render the dual-source crossfade graph, and return the
 * metrics the harness asserts over. Mirrors the reference #73 graph shape so
 * packaging is proven not to perturb the sync contract. Runs the render twice:
 * once faithful (unattenuated) and once with distinct constant per-lane gains
 * (the static-gain variant, issue #31), so the same push proves attenuation
 * leaves the zero-offset promise intact.
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
  const decodeCtx = new OfflineAudioContext(2, FRAMES, SR);
  const decoded: Record<string, AudioBuffer> = {};
  const decodeInfo: Record<string, DecodeInfo> = {};
  for (const [name, base64] of Object.entries(fixtures)) {
    try {
      const buf = await decodeCtx.decodeAudioData(toArrayBuffer(base64));
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
  if (!decoded["a.wav"] || !decoded["b.wav"] || !decoded["src.wav"]) {
    return { decodeInfo, renderSkipped: true };
  }

  const geo: Geometry = { SR, FRAMES, SWITCH_SAMPLE, FADE_SAMPLES };
  // Faithful render (the existing contract), then the static-gain variant with
  // distinct attenuations (A −6 dB, B −12 dB): both exact powers of two so the
  // scaled crossfade bound stays bit-identical, and both ≤ 0 dB like loudness
  // matching, which only ever attenuates (#66). Then the three-lane variant
  // (spec #42): the same render with SRC audible at its own gain (−18 dB), the
  // shape a project session plays — the source is in the match group and on the
  // one transport, so both contract points must survive its presence.
  // The renders are independent offline contexts, so they run together rather
  // than one after the other.
  const [faithful, attenuated, threeLane] = await Promise.all([
    renderVariant(geo, decoded, { a: 1, b: 1, src: 0 }),
    renderVariant(geo, decoded, { a: 0.5, b: 0.25, src: 0 }),
    renderVariant(geo, decoded, { a: 0.5, b: 0.25, src: 0.125 }),
  ]);

  return { decodeInfo, ...faithful, attenuated, threeLane };
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
