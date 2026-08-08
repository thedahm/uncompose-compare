/**
 * The comparison-record payload builder for the M3 workbench (issue #16).
 *
 * Concluding a session writes an immutable comparison record conforming to the
 * v0 JSON Schema this repo owns (`schemas/compare/v0/…`, decision uncompose#65).
 * The record is split across the two ends of the app: the server owns the parts
 * only it can vouch for — `schema`, the ULID `id`, `created_at`/`completed_at`,
 * the trusted `candidates[]` (path/sha256/size), `mode`, `playback` — and this
 * module builds the session-authored parts the listener actually decided:
 * `result`, `observations`, `loops`, and free-text `context`.
 *
 * It is a pure function of the in-memory verdict, ledger, and region, so the
 * shaping (seconds → integer ms, ledger `both`/untethered pins → the-comparison-
 * generally, the active region → the single `loops[]` entry) is unit-tested off
 * a browser; the modal DOM and the on-disk round-trip are covered by the flow
 * spec and the CLI process-boundary tests.
 */
import type { Observation } from "./ledger";
import type { Label, Region } from "./transport";

/** The listener's verdict: a preferred candidate, "no preference", or undecided. */
export interface Verdict {
  /** "A"/"B" prefer that candidate; "none" is a recorded no-preference; null = undecided. */
  preference: Label | "none" | null;
  /** 1-5 confidence, meaningful only when a candidate is preferred. */
  confidence: number | null;
  criterion: string;
  summary: string;
}

/** The `result` object as the schema shapes it. */
export interface RecordResult {
  preference: string | null;
  confidence?: number;
  criterion?: string;
  summary?: string;
}

/** One `observations[]` entry, schema-shaped (ms positions, label candidates). */
export interface RecordObservation {
  at: string;
  position_ms?: number;
  loop?: number;
  candidate?: string;
  text: string;
}

/** One `loops[]` entry, in integer milliseconds. */
export interface RecordLoop {
  start_ms: number;
  end_ms: number;
}

/** The session-authored slice of the record the browser POSTs to `/record`. */
export interface RecordPayload {
  result: RecordResult;
  observations: RecordObservation[];
  loops: RecordLoop[];
  context?: string;
}

/** Seconds → whole milliseconds (the schema stores integer ms). */
function toMs(seconds: number): number {
  return Math.round(seconds * 1000);
}

/**
 * The color tier for a 1-5 confidence: red for a hesitant 1-2, amber for a
 * middling 3, green for a confident 4-5 (confirmed verdict UX, uncompose#61).
 */
export function confidenceTier(confidence: number): "low" | "mid" | "high" {
  if (confidence <= 2) return "low";
  if (confidence === 3) return "mid";
  return "high";
}

/**
 * Build the session-authored record payload from the current verdict, the
 * chronological ledger, the active region, and the free-text context.
 */
export function buildRecordPayload(params: {
  verdict: Verdict;
  context: string;
  observations: Observation[];
  region: Region | null;
}): RecordPayload {
  const { verdict, context, observations, region } = params;

  const prefers = verdict.preference === "A" || verdict.preference === "B";
  const result: RecordResult = {
    // "no preference" is a valid, honest outcome: a null preference.
    preference: prefers ? (verdict.preference as Label) : null,
  };
  // Confidence is meaningful — and required by the schema — exactly when a
  // candidate is preferred; a no-preference result carries none.
  if (prefers && verdict.confidence !== null) result.confidence = verdict.confidence;
  const criterion = verdict.criterion.trim();
  if (criterion) result.criterion = criterion;
  const summary = verdict.summary.trim();
  if (summary) result.summary = summary;

  const recordObservations = observations.map((o) => {
    const entry: RecordObservation = { at: o.at, text: o.text };
    if (o.position !== null) entry.position_ms = toMs(o.position);
    // The schema's `candidate` is a single candidate label; a "both" pin and an
    // untethered general note both read as the-comparison-generally (no label).
    if (o.candidate === "A" || o.candidate === "B") entry.candidate = o.candidate;
    if (o.loop !== null) entry.loop = o.loop;
    return entry;
  });

  // The UI's single region is the schema's single loop (at most one in v0.1).
  const loops: RecordLoop[] = region
    ? [{ start_ms: toMs(region.start), end_ms: toMs(region.end) }]
    : [];

  const payload: RecordPayload = { result, observations: recordObservations, loops };
  const trimmedContext = context.trim();
  if (trimmedContext) payload.context = trimmedContext;
  return payload;
}
