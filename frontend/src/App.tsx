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
 * `-`/`=` step 2 s, `home` (or `0`) rewind, click-to-seek on any waveform, a
 * "stop returns" toggle (default: resume), and `?` toggling the help modal. Bare
 * keys only, except `ctrl+z` / `ctrl+shift+z` for ledger undo/redo (issue #15).
 * The SRC lane and stems section are kept as empty structural slots so M4/M5 add
 * rows rather than redesign.
 *
 * The `/session` payload is a discriminated union (#28): a blind session's
 * candidates structurally cannot carry a name, path, hash, or size, and a
 * sighted session's carry all of them plus the compatibility flags. The page
 * branches on that one discriminator — anonymous switch buttons or identified
 * lane rows — so concealment is checked by the compiler rather than remembered.
 *
 * This file is the shell: state, the transport wiring, and the stage. The parts
 * that stand alone live beside it — the ledger (`Ledger.tsx`), the verdict modal
 * (`VerdictModal.tsx`), the help modal (`HelpModal.tsx`) — over the pure logic in
 * `transport.ts` / `ledger.ts` / `record.ts` and the audio graph in `engine.ts`.
 *
 * The `/session` fetch, the "uncompose-compare" marker, and the duration-mismatch
 * warning (issue #10) survive; the `window.__uncomposeSync` harness seam lives in
 * `sync.ts`, registered from `main.tsx`, and is untouched by this page.
 */
import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { PlaybackEngine } from "./engine";
import { Waveform, type Pin } from "./Waveform";
import { HelpModal } from "./HelpModal";
import { LedgerSection } from "./Ledger";
import { Stars } from "./ConfidenceStars";
import { canSave, emptyVerdict, VerdictModal } from "./VerdictModal";
import {
  computeLoudness,
  computePeaks,
  computeSpectrogram,
  dbToGain,
  formatTime,
  type CandidateViews,
  type Label,
  type LaneId,
  type Region,
  type ViewMode,
} from "./transport";
import { addObservation, emptyLedger, makeObservation, redo, undo, type Ledger, type Target } from "./ledger";
import { buildRecordPayload, type Verdict } from "./record";

/**
 * A candidate as a *blind* session serves it (#28): the server conceals identity,
 * so only the label, the shared duration, and the opaque audio reference arrive.
 * Name, path, sha256, size, and the per-candidate technical metadata are not
 * optional here — they are structurally absent, so the blind page cannot leak
 * what its type cannot hold. Reveal comes only at conclude (#29).
 */
interface BlindCandidate {
  label: string;
  duration_ms: number;
  audio: string;
}

/** A candidate as a sighted session serves it: identity and metadata, all present. */
interface SightedCandidate extends BlindCandidate {
  name: string;
  path: string;
  sha256: string;
  size: number;
  frames: number;
  sample_rate: number;
  channels: number;
}

/**
 * One label's revealed identity, returned by conclude and shown post-write (#29).
 * When loudness matching ran (#32) the reveal also carries the measured LUFS and
 * applied gain a blind session held back until this one irreversible event —
 * `measured_lufs` null for a lane that fell below the integrated gate.
 */
interface RevealCandidate {
  label: Label;
  path: string;
  sha256: string;
  size: number;
  measured_lufs?: number | null;
  gain_db?: number;
}

/**
 * One lane's loudness-match figures. Sighted (and the record) report both the
 * measured LUFS and the applied gain; a blind session (#32) conceals the measured
 * figure and carries only the `gain_db` the engine applies, so `measured_lufs` is
 * absent there — and `null` for any lane the integrated gate gave no reading for.
 */
interface LoudnessCandidate {
  measured_lufs?: number | null;
  gain_db: number;
}

/**
 * The session's `loudness_match` (issue #30), mirroring the record's
 * `playback.loudness_match` shape (uncompose#66). Off: `{ enabled: false }`. On:
 * the method plus a per-label map of the applied gain (and, sighted, the measured
 * LUFS — a blind session hides it until reveal, #32).
 */
interface LoudnessMatch {
  enabled: boolean;
  method?: string;
  candidates?: Partial<Record<LaneId, LoudnessCandidate>>;
}

/**
 * A blind session (#28): the label↔file assignment was shuffled at load and every
 * identifying detail is concealed. Blind mode refuses any mismatch pre-bind, so
 * the compatibility flags do not exist here at all — the payload carries the
 * candidates and the loudness match, and nothing that could name a file.
 */
export interface BlindSession {
  blind: true;
  candidates: BlindCandidate[];
  loudness_match: LoudnessMatch;
  /**
   * The SRC lane (spec #42): the shared source both candidates are compared
   * against. Always identified — concealment is A/B only — so it carries full
   * metadata even in a blind session. Absent when the pair shares no source.
   */
  source?: SightedCandidate;
}

/**
 * A sighted session: identified candidates plus the compatibility flags the
 * workbench warns on. Every flag is present (the server always states them), so
 * the sighted path reads real values rather than defaulting around absences.
 */
export interface SightedSession {
  blind?: false;
  candidates: SightedCandidate[];
  loudness_match: LoudnessMatch;
  duration_mismatch: boolean;
  duration_delta_ms: number;
  /**
   * Absent when the two lanes run at different sample rates: a frame delta
   * across rates counts incomparable units, so the server omits it rather than
   * report a number that means nothing.
   */
  duration_delta_samples?: number;
  sample_rate_mismatch: boolean;
  channel_count_mismatch: boolean;
  /** The SRC lane (spec #42), when the candidates share a source. */
  source?: SightedCandidate;
}

/**
 * The `/session` payload. The two shapes are discriminated by `blind`, so
 * concealment is a type-level fact: a blind payload structurally cannot hold an
 * identity, and the sighted path keeps its compile-time guarantees.
 */
export type SessionMeta = BlindSession | SightedSession;

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

function candidateColor(label: LaneId): string {
  if (label === "A") return "#4ea1ff";
  if (label === "B") return "#ff8f4e";
  return "#9b8fff"; // SRC — the reference lane
}

/**
 * The session's two lanes keyed by label — the label is the reference key
 * everywhere else (record, verdict, pins, loudness match), so the page never
 * treats "the first candidate" as A. Null when the payload does not name both.
 * The blind/sighted discriminator rides along, so the lane rows read identities
 * the type guarantees are there.
 */
export type Lanes =
  | { blind: true; byLabel: Record<Label, BlindCandidate> }
  | { blind: false; byLabel: Record<Label, SightedCandidate> };

export function lanesOf(session: SessionMeta): Lanes | null {
  const pair = <T extends { label: string }>(candidates: T[]): Record<Label, T> | null => {
    const a = candidates.find((c) => c.label === "A");
    const b = candidates.find((c) => c.label === "B");
    return a && b ? { A: a, B: b } : null;
  };
  if (session.blind) {
    const byLabel = pair(session.candidates);
    return byLabel && { blind: true, byLabel };
  }
  const byLabel = pair(session.candidates);
  return byLabel && { blind: false, byLabel };
}

/**
 * A lane's measured loudness for display: the figure, or the honest absence when
 * the lane fell below BS.1770's integrated gate (the record writes `null` there
 * rather than a number it does not have — ADR-0003).
 */
export function formatLufs(lufs: number | null | undefined): string {
  return typeof lufs === "number" ? `${lufs.toFixed(1)} LUFS` : "no LUFS reading (below the gate)";
}

/**
 * The static linear per-lane gains for the engine (issue #30): the measured
 * attenuation when matching is on, unity for any lane the match does not name
 * (and for both when matching is off — faithful as-is playback, #66).
 */
function laneGains(match: LoudnessMatch): Record<LaneId, number> {
  const gain = (label: LaneId) => {
    const db = match.enabled ? match.candidates?.[label]?.gain_db : undefined;
    return db === undefined ? 1 : dbToGain(db);
  };
  return { A: gain("A"), B: gain("B"), SRC: gain("SRC") };
}

/**
 * The engraved verdict: what Save committed, and the only thing conclude ever
 * writes. Null until the first save.
 */
interface SavedVerdict {
  verdict: Verdict;
  context: string;
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
  const [views, setViews] = useState<Partial<Record<LaneId, CandidateViews>> | null>(null);
  const [view, setView] = useState<ViewMode>("waveform");

  const [live, setLive] = useState<LaneId>("A");
  const [playing, setPlaying] = useState(false);
  const [position, setPosition] = useState(0);
  const [duration, setDuration] = useState(0);
  const [stopReturns, setStopReturns] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [region, setRegion] = useState<Region | null>(null);
  const [looping, setLooping] = useState(false);

  // The observation ledger (issue #15): append-only pins with undo/redo history.
  const [ledger, setLedger] = useState<Ledger>(emptyLedger);
  const [composer, setComposer] = useState("");
  // The highlighted pin — two-way between a caret and its ledger row.
  const [activePin, setActivePin] = useState<string | null>(null);

  // The verdict (issue #16). `saved` is the engraved one — what Save committed
  // and what conclude writes. `draft`/`draftContext` are what the open modal is
  // editing; closing without saving throws them away. Saving does not finalize:
  // reopening seeds a fresh draft from `saved`, right up until conclude.
  const [saved, setSaved] = useState<SavedVerdict | null>(null);
  const [draft, setDraft] = useState<Verdict>(emptyVerdict);
  const [draftContext, setDraftContext] = useState("");
  const [verdictOpen, setVerdictOpen] = useState(false);
  // The conclude outcome: where the record landed (with the revealed label→file
  // mapping, #29), or why the write was refused.
  const [concludeResult, setConcludeResult] = useState<{
    path?: string;
    reveal?: RevealCandidate[];
    error?: string;
  } | null>(null);

  const engineRef = useRef<PlaybackEngine | null>(null);
  const startedRef = useRef(false);
  const composerRef = useRef<HTMLInputElement | null>(null);

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

        const lanes = lanesOf(s);
        if (!lanes) throw new Error("session is missing candidate A or B");

        const ctx = new AudioContext();
        const decode = async (c: BlindCandidate): Promise<AudioBuffer> => {
          const buf = await (await fetch(c.audio)).arrayBuffer();
          return await ctx.decodeAudioData(buf);
        };
        const [bufA, bufB] = await Promise.all([
          decode(lanes.byLabel.A),
          decode(lanes.byLabel.B),
        ]);
        // The SRC lane (spec #42): the shared source, decoded into the same graph
        // so it plays sample-locked with A/B and can be auditioned. Always
        // identified, so it decodes like a sighted candidate.
        const bufSrc = s.source ? await decode(s.source) : undefined;

        // Static per-lane gains from the server's loudness match (issue #30):
        // faithful unity when off, the measured attenuation when on. Constant
        // for the session — the sync contract is untouched (#66).
        const laneGain = laneGains(s.loudness_match);
        const eng = new PlaybackEngine(ctx, bufA, bufB, laneGain, bufSrc);
        eng.onEnded = () => syncFrom(eng);
        engineRef.current = eng;
        setViews({
          A: computeViews(bufA.getChannelData(0)),
          B: computeViews(bufB.getChannelData(0)),
          ...(bufSrc ? { SRC: computeViews(bufSrc.getChannelData(0)) } : {}),
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
    (label: LaneId) => withEngine((eng) => eng.switchTo(label)),
    [withEngine],
  );

  // The transport actions, shared by the buttons and the keymap so the two can
  // never drift apart.
  const togglePlay = useCallback(
    () => withEngine((eng) => eng.togglePlay(stopReturns)),
    [withEngine, stopReturns],
  );
  const toggleSwitch = useCallback(() => withEngine((eng) => eng.toggleSwitch()), [withEngine]);
  const toggleLoop = useCallback(() => withEngine((eng) => eng.toggleLoop()), [withEngine]);
  const clearRegion = useCallback(() => withEngine((eng) => eng.clearRegion()), [withEngine]);
  const rewind = useCallback(() => withEngine((eng) => eng.rewind()), [withEngine]);
  const step = useCallback(
    (delta: number) => withEngine((eng) => eng.step(delta)),
    [withEngine],
  );

  const selectRegion = useCallback(
    (a: number, b: number) => withEngine((eng) => eng.setRegion(a, b)),
    [withEngine],
  );

  // Pin an observation at the current playhead, tagged to `candidate` (the live
  // one for `enter`, both for `shift+enter`), carrying the active region as its
  // loop reference. The wall-clock `at` and id are stamped here so the ledger
  // logic stays pure and testable.
  const pin = useCallback(
    (candidate: Target | null, text: string) => {
      const eng = engineRef.current;
      const obs = makeObservation({
        id: crypto.randomUUID(),
        at: new Date().toISOString(),
        candidate,
        // Read the live position off the engine (pins are only creatable once
        // the workbench — and its engine — has mounted).
        position: eng ? eng.position() : 0,
        region,
        text,
      });
      setLedger((l) => addObservation(l, obs));
      setActivePin(obs.id);
    },
    [region],
  );

  // Submit the composer's free text (issue #15): `enter` tags the live
  // candidate, `shift+enter` tags both. Empty text is ignored so a stray key
  // never drops a blank note.
  const submitComposer = useCallback(
    (both: boolean) => {
      const text = composer.trim();
      if (!text) return;
      // Auditioning the SRC reference tags nothing — it is not a comparison
      // candidate, so a note made on it is a general observation (null).
      pin(both ? "both" : live === "SRC" ? null : live, text);
      setComposer("");
    },
    [composer, live, pin],
  );

  // Jump the transport to an observation's pinned position (untethered notes
  // just highlight) and light up its caret/row pair.
  const seekToObservation = useCallback(
    (id: string, pos: number | null) => {
      if (pos !== null) seek(pos);
      setActivePin(id);
    },
    [seek],
  );

  // Open the modal on a fresh draft of whatever is currently engraved, so an
  // abandoned edit can never change what conclude writes.
  const openVerdict = () => {
    setDraft(saved?.verdict ?? emptyVerdict);
    setDraftContext(saved?.context ?? "");
    setVerdictOpen(true);
  };

  // Save engraves the draft but never finalizes it: everything stays editable
  // until conclude writes the record (#61).
  const saveVerdict = () => {
    if (!canSave(draft)) return;
    setSaved({ verdict: draft, context: draftContext });
    setVerdictOpen(false);
  };

  // Conclude the session: build the session-authored record payload from the
  // *saved* verdict and POST it. The server assembles the rest, validates against
  // the owned schema, and writes the immutable record exactly once, reporting
  // where it landed (issue #16).
  const conclude = useCallback(async () => {
    if (!saved) return;
    const payload = buildRecordPayload({
      verdict: saved.verdict,
      context: saved.context,
      observations: ledger.observations,
      region,
    });
    try {
      const res = await fetch("/record", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
      });
      const data = (await res.json().catch(() => ({}))) as {
        path?: string;
        reveal?: RevealCandidate[];
        error?: string;
      };
      setConcludeResult(
        res.ok
          ? { path: data.path, reveal: data.reveal }
          : { error: data.error ?? `record ${res.status}` },
      );
    } catch (e) {
      setConcludeResult({ error: String(e) });
    }
  }, [saved, ledger.observations, region]);

  // Keyboard transport: bare keys for the transport and pins, ctrl+z /
  // ctrl+shift+z for ledger undo/redo, and none of it while typing into the
  // composer or a ledger edit.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const typing =
        target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA");
      // Undo/redo (issue #15) are the only ctrl keys the workbench owns. They
      // fire only when not typing into a field, so a text input keeps its
      // native editing/undo behavior.
      if (e.ctrlKey || e.metaKey) {
        if (!typing && (e.key === "z" || e.key === "Z")) {
          e.preventDefault();
          setLedger((l) => (e.shiftKey ? redo(l) : undo(l)));
        }
        return;
      }
      if (e.altKey) return;
      // While typing into the composer or a ledger edit, the field's own key
      // handlers own the keyboard (so space, enter, etc. type normally).
      if (typing) return;
      switch (e.key) {
        case " ":
          e.preventDefault();
          togglePlay();
          break;
        case "x":
        case "X":
          toggleSwitch();
          break;
        case "ArrowLeft":
        case "-":
          e.preventDefault();
          step(-STEP_SECONDS);
          break;
        case "ArrowRight":
        case "=":
          e.preventDefault();
          step(STEP_SECONDS);
          break;
        case "Home":
        case "0":
          e.preventDefault();
          rewind();
          break;
        case "r":
        case "R":
          toggleLoop();
          break;
        case "u":
        case "U":
          clearRegion();
          break;
        case "Enter":
          // Pin on the live candidate (both with shift) without interrupting
          // listening; the empty text is filled in later in the ledger.
          e.preventDefault();
          pin(e.shiftKey ? "both" : live === "SRC" ? null : live, "");
          break;
        case "Tab":
          // Jump to the composer to write a free-text observation.
          e.preventDefault();
          composerRef.current?.focus();
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
  }, [togglePlay, toggleSwitch, toggleLoop, clearRegion, rewind, step, pin, live]);

  // The two lanes keyed by label, narrowed once from the session payload: the
  // blind pair carries no identity to render, the sighted pair carries all of it.
  const lanes = session && lanesOf(session);

  // Only positioned observations get a caret on the stage waveform.
  const pins: Pin[] = ledger.observations.flatMap((o) =>
    o.position === null ? [] : [{ id: o.id, position: o.position, candidate: o.candidate }],
  );

  // The saved verdict's colour-coded confidence stars beside the preferred
  // label (issue #16) — on its lane row when sighted, inside its anonymous
  // switch button when blind. Null until a confident preference is engraved.
  const preferredStars = (label: Label, style?: CSSProperties) =>
    saved?.verdict.preference === label && saved.verdict.confidence !== null ? (
      <Stars
        confidence={saved.verdict.confidence}
        testid={`verdict-stars-${label}`}
        title={`Preferred — confidence ${saved.verdict.confidence}/5`}
        style={style}
      />
    ) : null;

  // The lane header both presentations share: the live marker, the label, and
  // the engraved verdict's stars. The blind switch button and the sighted lane
  // row then differ only in their wrapper and in whether a filename follows the
  // label — so the live marker and the stars cannot drift between them.
  const laneHeader = (label: LaneId, name?: string) => (
    <>
      <span data-testid={`live-marker-${label}`}>{live === label ? "● " : "  "}</span>
      <strong>{label}</strong>
      {name ? ` ${name}` : null}
      {/* Only a comparison candidate (A/B) can carry a preferred verdict; the SRC
          reference never wins, so it never shows stars. */}
      {label !== "SRC" && preferredStars(label, { marginLeft: 6 })}
    </>
  );

  return (
    <main style={{ fontFamily: "system-ui, sans-serif", color: "#eee", background: "#0a0a0a", minHeight: "100vh", padding: 16 }}>
      <h1 data-testid="hello" style={{ fontSize: 18 }}>
        uncompose-compare
      </h1>

      {/* The compatibility warnings are a sighted-session concern: blind mode
          refuses a mismatched pair pre-bind, so there is nothing to warn about
          (and no metadata to warn with). */}
      {session && !session.blind && <SessionWarnings session={session} />}

      {/* Loudness matching (issue #30): state clearly whenever playback is not
          at mastered levels. Sighted mode shows the per-lane figures; a blind
          session (#32) reports only that matching is active — a per-lane LUFS
          figure fingerprints a candidate, so the numbers stay hidden until the
          conclude reveal. */}
      {session?.loudness_match.enabled && (
        <p data-testid="loudness-match" style={{ color: "#8fd6ff" }}>
          Loudness matching active — not mastered levels (BS.1770 integrated).{" "}
          {session.blind
            ? "Per-lane figures are hidden until reveal."
            : (["A", "B", "SRC"] as LaneId[]).map((label) => {
                const c = session.loudness_match.candidates?.[label];
                if (!c) return null;
                return (
                  <span key={label} data-testid={`loudness-${label}`} style={{ marginRight: 10 }}>
                    <strong>{label}</strong>: {formatLufs(c.measured_lufs)},{" "}
                    {c.gain_db.toFixed(1)} dB
                  </span>
                );
              })}
        </p>
      )}

      {error && (
        <p data-testid="session-error">Could not load session: {error}</p>
      )}

      {!views && !error && <p data-testid="loading">Loading candidates…</p>}

      {views && lanes && (
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
              // `live` always names a lane whose views were computed above (A/B
              // always, SRC only when it exists and can be switched to).
              views={views[live]!}
              view={view}
              duration={duration}
              position={position}
              onSeek={seek}
              region={region}
              looping={looping}
              onSelectRegion={selectRegion}
              pins={pins}
              activePin={activePin}
              onPinEnter={setActivePin}
              onPinLeave={() => setActivePin(null)}
              onPinClick={setActivePin}
              color={candidateColor(live)}
              height={96}
              testid="stage-waveform"
            />
            <div style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 8 }}>
              <button data-testid="play-toggle" onClick={togglePlay}>
                {playing ? "Stop" : "Play"}
              </button>
              <button data-testid="switch" onClick={toggleSwitch}>
                Switch (x)
              </button>
              <button data-testid="rewind" onClick={rewind}>
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

          {/* SRC lane (spec #42): the shared source, an always-identified third
              lane that plays sample-locked with A/B and can be auditioned by
              click. It never wins a preference, so it carries no verdict stars
              and is not an `x` switch target. Absent when the pair shares none. */}
          {session?.source && views.SRC && (
            <div
              data-testid="lane-SRC"
              data-live={live === "SRC"}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                marginBottom: 6,
                padding: 4,
                borderLeft: `3px solid ${live === "SRC" ? "#fff" : "transparent"}`,
              }}
            >
              <span style={{ width: 90, cursor: "pointer" }} onClick={() => switchTo("SRC")}>
                {laneHeader("SRC", session.source.name)}
              </span>
              <div style={{ flex: 1 }}>
                <Waveform
                  views={views.SRC}
                  view={view}
                  duration={duration}
                  position={position}
                  onSeek={seek}
                  region={region}
                  looping={looping}
                  onSelectRegion={selectRegion}
                  onActivate={() => switchTo("SRC")}
                  color={candidateColor("SRC")}
                  height={48}
                  testid="waveform-SRC"
                />
              </div>
            </div>
          )}

          {/* A blind session (#29) hides the identifying A/B lane rows and shows
              two anonymous switch buttons in their place (per the #61 design); a
              sighted session keeps the labelled lane rows with their waveforms.
              Either way the live lane switches by button and by `x`. */}
          {lanes.blind ? (
            <div data-testid="switch-buttons" role="group" aria-label="Switch candidate">
              {(["A", "B"] as Label[]).map((label) => (
                <button
                  key={label}
                  data-testid={`switch-${label}`}
                  data-live={live === label}
                  aria-pressed={live === label}
                  onClick={() => switchTo(label)}
                  style={{
                    marginRight: 8,
                    padding: "8px 16px",
                    fontWeight: "bold",
                    borderLeft: `3px solid ${live === label ? "#fff" : "transparent"}`,
                  }}
                >
                  {/* An anonymous button carries only the shared lane header —
                      no name, path, hash, or size (concealment). */}
                  {laneHeader(label)}
                </button>
              ))}
            </div>
          ) : (
            /* A/B lane rows: click to audition; ● marks the live lane. */
            <div data-testid="lanes">
              {(["A", "B"] as Label[]).map((label) => {
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
                      {laneHeader(label, lanes.byLabel[label].name)}
                    </span>
                    <div style={{ flex: 1 }}>
                      <Waveform
                        views={views[label]!}
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
          )}

          {/* Stems section: an empty structural slot (project mode, M5). */}
          <div data-testid="stems-slot" aria-hidden="true" />

          <LedgerSection
            ledger={ledger}
            setLedger={setLedger}
            composer={composer}
            setComposer={setComposer}
            submitComposer={submitComposer}
            composerRef={composerRef}
            live={live}
            activePin={activePin}
            setActivePin={setActivePin}
            onSeek={seekToObservation}
          />

          {/* Verdict & conclude (issue #16): decide a preference (or no
              preference), then write the immutable comparison record. */}
          <section data-testid="verdict" style={{ marginTop: 16 }}>
            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <h2 style={{ fontSize: 15, margin: 0 }}>Verdict</h2>
              <button data-testid="open-verdict" onClick={openVerdict}>
                {saved ? "Edit verdict…" : "Set verdict…"}
              </button>
              {saved && (
                <span data-testid="verdict-summary" style={{ color: "#bbb" }}>
                  {saved.verdict.preference === "none" ? (
                    "No preference"
                  ) : (
                    <>
                      Prefers <strong>{saved.verdict.preference}</strong>
                      {saved.verdict.confidence !== null && (
                        <Stars
                          confidence={saved.verdict.confidence}
                          testid="verdict-summary-stars"
                          style={{ marginLeft: 6 }}
                        />
                      )}
                    </>
                  )}
                </span>
              )}
              <button
                data-testid="conclude"
                style={{ marginLeft: "auto" }}
                disabled={!saved || !!concludeResult?.path}
                onClick={conclude}
                title={saved ? "Write the comparison record" : "Save a verdict before concluding"}
              >
                Conclude &amp; write record
              </button>
            </div>
            {concludeResult?.path && (
              <p data-testid="conclude-path" style={{ color: "#5cd67a" }}>
                Record written to <code>{concludeResult.path}</code>
              </p>
            )}
            {/* Reveal at conclude (#29): only after the record is written — the
                one irreversible event — does the UI show which file each label
                was. Only a blind session concealed anything, so only a blind
                conclude carries a reveal; a sighted response has no `reveal` key
                to render (ADR-0006). */}
            {concludeResult?.reveal && (
              <div data-testid="reveal" style={{ color: "#8fd6ff", marginTop: 8 }}>
                <strong>Revealed:</strong>{" "}
                {concludeResult.reveal.map((c) => (
                  <span key={c.label} data-testid={`reveal-${c.label}`} style={{ marginRight: 12 }}>
                    <strong>{c.label}</strong> was <code>{c.path}</code>
                    {/* Matching on (#32): the measured LUFS and applied gain,
                        held back during the blind session, surface here. */}
                    {c.gain_db !== undefined && (
                      <> ({formatLufs(c.measured_lufs)}, {c.gain_db.toFixed(1)} dB)</>
                    )}
                  </span>
                ))}
              </div>
            )}
            {concludeResult?.error && (
              <p data-testid="conclude-error" role="alert" style={{ color: "#ff5c5c" }}>
                Conclude refused: {concludeResult.error}
              </p>
            )}
          </section>
        </section>
      )}

      {verdictOpen && (
        <VerdictModal
          verdict={draft}
          onChange={setDraft}
          context={draftContext}
          onContextChange={setDraftContext}
          onSave={saveVerdict}
          onClose={() => setVerdictOpen(false)}
        />
      )}

      {helpOpen && <HelpModal onClose={() => setHelpOpen(false)} />}
    </main>
  );
}

/**
 * The compatibility warnings a sighted session shows (issues #10, #26 story 11):
 * a duration, sample-rate, or channel-count difference is reported, never
 * blocking — both files still load and play at their own rate.
 *
 * Each warning names only the property it is about. A sample delta is reported
 * only when the server sent one: across different sample rates a frame delta
 * counts incomparable units, so the listener gets the rate warning alone rather
 * than a second, meaningless one beside it.
 */
function SessionWarnings({ session }: { session: SightedSession }) {
  const lanes = lanesOf(session);
  if (!lanes || lanes.blind) return null;
  const { A, B } = lanes.byLabel;
  return (
    <>
      {session.duration_mismatch && (
        <p role="alert" data-testid="duration-mismatch" style={{ color: "#ffcf6b" }}>
          Duration mismatch: the candidates differ by{" "}
          {session.duration_delta_samples !== undefined
            ? `${session.duration_delta_samples} samples (${session.duration_delta_ms.toFixed(1)} ms)`
            : `${session.duration_delta_ms.toFixed(1)} ms`}
          . Both still load.
        </p>
      )}

      {session.sample_rate_mismatch && (
        <p role="alert" data-testid="sample-rate-mismatch" style={{ color: "#ffcf6b" }}>
          Sample-rate mismatch: A is {A.sample_rate} Hz, B is {B.sample_rate} Hz. Both
          still load.
        </p>
      )}

      {session.channel_count_mismatch && (
        <p role="alert" data-testid="channel-count-mismatch" style={{ color: "#ffcf6b" }}>
          Channel-count mismatch: A has {A.channels}, B has {B.channels}. Both still load.
        </p>
      )}
    </>
  );
}
