/**
 * The M3 playback workbench core (issue #12): the served page becomes the
 * listening stage, scoped to sighted two-file mode.
 *
 * Both candidates load from their `/audio/<sha256>` proxy endpoints (issue #11),
 * decode client-side into browser memory only, and render as waveforms — a
 * primary stage waveform plus A/B lane rows. All lanes play sample-locked in one
 * Web Audio graph (`engine.ts`) on a shared transport: switching the audible
 * candidate (`x`, or clicking a lane) is an instant ~10 ms equal-power crossfade
 * that never shifts the playback position. `●` marks the live lane.
 *
 * Transport keymap (confirmed #61): `space` play/stop, `x` switch, `←`/`→` or
 * `-`/`=` step 2 s, `home` rewind, click-to-seek on any waveform, a "stop
 * returns" toggle (default: resume), and `?` toggling the help modal. Bare keys
 * only — `ctrl` stays reserved for undo/redo (a later ticket). The SRC lane and
 * stems section are kept as empty structural slots so M4/M5 add rows rather than
 * redesign.
 *
 * The `/session` fetch, the "uncompose-compare" marker, and the duration-mismatch
 * warning (issue #10) survive; the `window.__uncomposeSync` harness seam lives in
 * `sync.ts`, registered from `main.tsx`, and is untouched by this page.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { PlaybackEngine } from "./engine";
import { Waveform } from "./Waveform";
import {
  computeLoudness,
  computePeaks,
  computeSpectrogram,
  formatTime,
  otherLabel,
  type CandidateViews,
  type Label,
  type Region,
  type ViewMode,
} from "./transport";

interface Candidate {
  label: string;
  name: string;
  path: string;
  sha256: string;
  size: number;
  frames: number;
  duration_ms: number;
  sample_rate: number;
  channels: number;
  audio: string;
}

interface SessionMeta {
  candidates: Candidate[];
  duration_mismatch: boolean;
  duration_delta_samples: number;
  duration_delta_ms: number;
}

/** Waveform/loudness envelope resolution — enough detail without paint cost. */
const BUCKETS = 1000;
/** Spectral view resolution: time columns and FFT window (bins = size / 2). */
const SPECTRAL_COLUMNS = 512;
const FFT_SIZE = 512;
/** The transport step, per the #61 keymap. */
const STEP_SECONDS = 2;

/** The view toggle options, in display order (issue #14). */
const VIEWS: { id: ViewMode; label: string }[] = [
  { id: "waveform", label: "Waveform" },
  { id: "loudness", label: "Loudness" },
  { id: "spectral", label: "Spectral" },
];

function candidateColor(label: Label): string {
  return label === "A" ? "#4ea1ff" : "#ff8f4e";
}

/**
 * Compute all three views of a decoded channel once (issue #14). The waveform,
 * loudness (RMS), and spectral (FFT) data live only in this returned object, in
 * browser memory — nothing is persisted or fetched (privacy contract).
 */
function computeViews(channel: Float32Array): CandidateViews {
  return {
    peaks: computePeaks(channel, BUCKETS),
    loudness: computeLoudness(channel, BUCKETS),
    spectral: computeSpectrogram(channel, SPECTRAL_COLUMNS, FFT_SIZE),
  };
}

export function App() {
  const [session, setSession] = useState<SessionMeta | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [candidates, setCandidates] = useState<Record<Label, Candidate> | null>(null);
  const [views, setViews] = useState<Record<Label, CandidateViews> | null>(null);
  const [view, setView] = useState<ViewMode>("waveform");

  const [live, setLive] = useState<Label>("A");
  const [playing, setPlaying] = useState(false);
  const [position, setPosition] = useState(0);
  const [duration, setDuration] = useState(0);
  const [stopReturns, setStopReturns] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [region, setRegion] = useState<Region | null>(null);
  const [looping, setLooping] = useState(false);

  const engineRef = useRef<PlaybackEngine | null>(null);
  const startedRef = useRef(false);

  const syncFrom = useCallback((eng: PlaybackEngine) => {
    setLive(eng.live());
    setPlaying(eng.isPlaying());
    setPosition(eng.position());
    setRegion(eng.getRegion());
    setLooping(eng.isLooping());
  }, []);

  // Load /session, then fetch and decode both proxies into one Web Audio graph.
  // The ref makes this a one-shot: StrictMode's dev-mode double-mount must not
  // build a second graph, and the single flight is allowed to finish (App is
  // the root component, so it never unmounts mid-flight in practice).
  useEffect(() => {
    if (startedRef.current) return;
    startedRef.current = true;

    (async () => {
      try {
        const res = await fetch("/session");
        if (!res.ok) throw new Error(`session ${res.status}`);
        const s = (await res.json()) as SessionMeta;
        setSession(s);

        const byLabel = (label: Label) =>
          s.candidates.find((c) => c.label === label);
        const a = byLabel("A");
        const b = byLabel("B");
        if (!a || !b) throw new Error("session is missing candidate A or B");

        const ctx = new AudioContext();
        const decode = async (c: Candidate): Promise<AudioBuffer> => {
          const buf = await (await fetch(c.audio)).arrayBuffer();
          return await ctx.decodeAudioData(buf);
        };
        const [bufA, bufB] = await Promise.all([decode(a), decode(b)]);

        const eng = new PlaybackEngine(ctx, bufA, bufB);
        eng.onEnded = () => syncFrom(eng);
        engineRef.current = eng;
        setCandidates({ A: a, B: b });
        setViews({
          A: computeViews(bufA.getChannelData(0)),
          B: computeViews(bufB.getChannelData(0)),
        });
        setDuration(eng.duration());
        syncFrom(eng);
      } catch (e) {
        setError(String(e));
      }
    })();
  }, [syncFrom]);

  // While playing, follow the transport so the playhead and readout track it.
  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    const tick = () => {
      const eng = engineRef.current;
      if (eng) {
        setPosition(eng.position());
        setPlaying(eng.isPlaying());
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing]);

  /** Run a transport action, then mirror the engine's state into React. */
  const withEngine = useCallback(
    (action: (eng: PlaybackEngine) => void) => {
      const eng = engineRef.current;
      if (!eng) return;
      action(eng);
      syncFrom(eng);
    },
    [syncFrom],
  );

  const seek = useCallback(
    (sec: number) => withEngine((eng) => eng.seek(sec)),
    [withEngine],
  );

  const switchTo = useCallback(
    (label: Label) => withEngine((eng) => eng.switchTo(label)),
    [withEngine],
  );

  const togglePlay = () => withEngine((eng) => eng.togglePlay(stopReturns));
  const toggleLoop = () => withEngine((eng) => eng.toggleLoop());
  const clearRegion = () => withEngine((eng) => eng.clearRegion());

  const selectRegion = useCallback(
    (a: number, b: number) => withEngine((eng) => eng.setRegion(a, b)),
    [withEngine],
  );

  // Keyboard transport: bare keys only, ctrl reserved for undo/redo (a later
  // ticket), and never while typing into a future ledger/verdict field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA")) {
        return;
      }
      switch (e.key) {
        case " ":
          e.preventDefault();
          withEngine((eng) => eng.togglePlay(stopReturns));
          break;
        case "x":
        case "X":
          withEngine((eng) => eng.toggleSwitch());
          break;
        case "ArrowLeft":
        case "-":
          e.preventDefault();
          withEngine((eng) => eng.step(-STEP_SECONDS));
          break;
        case "ArrowRight":
        case "=":
          e.preventDefault();
          withEngine((eng) => eng.step(STEP_SECONDS));
          break;
        case "Home":
          e.preventDefault();
          withEngine((eng) => eng.rewind());
          break;
        case "r":
        case "R":
          withEngine((eng) => eng.toggleLoop());
          break;
        case "u":
        case "U":
          withEngine((eng) => eng.clearRegion());
          break;
        case "?":
          e.preventDefault();
          setHelpOpen((open) => !open);
          break;
        default:
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [stopReturns, withEngine]);

  return (
    <main style={{ fontFamily: "system-ui, sans-serif", color: "#eee", background: "#0a0a0a", minHeight: "100vh", padding: 16 }}>
      <h1 data-testid="hello" style={{ fontSize: 18 }}>
        uncompose-compare
      </h1>

      {session?.duration_mismatch && (
        <p role="alert" data-testid="duration-mismatch" style={{ color: "#ffcf6b" }}>
          Duration mismatch: the candidates differ by {session.duration_delta_samples}{" "}
          samples ({session.duration_delta_ms.toFixed(1)} ms). Both still load.
        </p>
      )}

      {error && (
        <p data-testid="session-error">Could not load session: {error}</p>
      )}

      {!views && !error && <p data-testid="loading">Loading candidates…</p>}

      {views && candidates && (
        <section data-testid="workbench">
          {/* Primary stage: the live candidate's display with the transport. */}
          <div data-testid="stage" style={{ marginBottom: 12 }}>
            <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 4 }}>
              <span data-testid="live-lane">
                Live: <strong>{live}</strong>
              </span>
              {/* View toggle: switches every display (stage + lanes) together. */}
              <span data-testid="view-toggle" role="group" aria-label="View">
                {VIEWS.map((v) => (
                  <button
                    key={v.id}
                    data-testid={`view-${v.id}`}
                    aria-pressed={view === v.id}
                    onClick={() => setView(v.id)}
                    style={{
                      marginLeft: 4,
                      fontWeight: view === v.id ? "bold" : "normal",
                      textDecoration: view === v.id ? "underline" : "none",
                    }}
                  >
                    {v.label}
                  </button>
                ))}
              </span>
              <span data-testid="transport-position">
                {formatTime(position)} / {formatTime(duration)}
              </span>
            </div>
            <Waveform
              views={views[live]}
              view={view}
              duration={duration}
              position={position}
              onSeek={seek}
              region={region}
              looping={looping}
              onSelectRegion={selectRegion}
              color={candidateColor(live)}
              height={96}
              testid="stage-waveform"
            />
            <div style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 8 }}>
              <button data-testid="play-toggle" onClick={togglePlay}>
                {playing ? "Stop" : "Play"}
              </button>
              <button data-testid="switch" onClick={() => switchTo(otherLabel(live))}>
                Switch (x)
              </button>
              <button data-testid="rewind" onClick={() => seek(0)}>
                Rewind
              </button>
              <button data-testid="loop-toggle" disabled={!region} onClick={toggleLoop}>
                {looping ? "Looping (r)" : "Loop (r)"}
              </button>
              <button data-testid="clear-region" disabled={!region} onClick={clearRegion}>
                Clear region (u)
              </button>
              <span
                data-testid="loop-status"
                data-region={region ? "true" : "false"}
                data-looping={looping ? "true" : "false"}
              >
                {region
                  ? `Region ${formatTime(region.start)}–${formatTime(region.end)}${looping ? " (looping)" : ""}`
                  : "No region"}
              </span>
              <label data-testid="stop-returns" style={{ marginLeft: "auto" }}>
                <input
                  type="checkbox"
                  checked={stopReturns}
                  onChange={(e) => setStopReturns(e.target.checked)}
                />{" "}
                Stop returns to start
              </label>
              <button data-testid="help-button" onClick={() => setHelpOpen((o) => !o)}>
                ? Help
              </button>
            </div>
          </div>

          {/* SRC lane: an empty structural slot (needs --source / project mode, M5). */}
          <div data-testid="src-lane-slot" aria-hidden="true" />

          {/* A/B lane rows: click to audition; ● marks the live lane. */}
          <div data-testid="lanes">
            {(["A", "B"] as Label[]).map((label) => {
              const c = candidates[label];
              const isLive = live === label;
              return (
                <div
                  key={label}
                  data-testid={`lane-${label}`}
                  data-live={isLive}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 8,
                    marginBottom: 6,
                    padding: 4,
                    borderLeft: `3px solid ${isLive ? "#fff" : "transparent"}`,
                  }}
                >
                  {/* Clicking the label auditions; dragging the waveform selects a
                      region, while a plain click on it also auditions (onActivate). */}
                  <span style={{ width: 90, cursor: "pointer" }} onClick={() => switchTo(label)}>
                    <span data-testid={`live-marker-${label}`}>{isLive ? "● " : "  "}</span>
                    <strong>{label}</strong> {c.name}
                  </span>
                  <div style={{ flex: 1 }}>
                    <Waveform
                      views={views[label]}
                      view={view}
                      duration={duration}
                      position={position}
                      onSeek={seek}
                      region={region}
                      looping={looping}
                      onSelectRegion={selectRegion}
                      onActivate={() => switchTo(label)}
                      color={candidateColor(label)}
                      height={48}
                      testid={`waveform-${label}`}
                    />
                  </div>
                </div>
              );
            })}
          </div>

          {/* Stems section: an empty structural slot (project mode, M5). */}
          <div data-testid="stems-slot" aria-hidden="true" />
        </section>
      )}

      {helpOpen && (
        <div
          data-testid="help-modal"
          role="dialog"
          aria-label="Keyboard help"
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(0,0,0,0.8)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
          }}
          onClick={() => setHelpOpen(false)}
        >
          <div
            style={{ background: "#161616", padding: 24, maxWidth: 420 }}
            onClick={(e) => e.stopPropagation()}
          >
            <h2 style={{ marginTop: 0 }}>Keyboard</h2>
            <section>
              <h3>Transport</h3>
              <ul>
                <li><kbd>space</kbd> — play / stop</li>
                <li><kbd>x</kbd> — switch audible candidate</li>
                <li><kbd>←</kbd> / <kbd>→</kbd> or <kbd>-</kbd> / <kbd>=</kbd> — step 2 s</li>
                <li><kbd>home</kbd> — rewind to start</li>
                <li>click a waveform — seek</li>
              </ul>
            </section>
            <section>
              <h3>Region &amp; loop</h3>
              <ul>
                <li>drag a waveform — select a region</li>
                <li><kbd>r</kbd> — loop the region (plays into it, then loops)</li>
                <li><kbd>u</kbd> — clear the region</li>
              </ul>
            </section>
            <section>
              <h3>Views</h3>
              <ul>
                <li>Waveform / Loudness / Spectral toggle — switches every display</li>
              </ul>
            </section>
            <section>
              <h3>Help</h3>
              <ul>
                <li><kbd>?</kbd> — toggle this help</li>
              </ul>
            </section>
            <button onClick={() => setHelpOpen(false)}>Close</button>
          </div>
        </div>
      )}
    </main>
  );
}
