/**
 * The observation ledger for the M3 workbench (issue #15).
 *
 * A listener captures what they hear as timestamped observations: `enter` pins
 * one on the audible candidate, `shift+enter` on both, and the composer writes a
 * free-text one. Observations are stored append-only in the order made (a
 * chronological ledger), and every create/edit/delete is undoable — `ctrl+z` /
 * `ctrl+shift+z` walk a history of the whole observation set.
 *
 * The store and its history are pure (the App injects each observation's id and
 * wall-clock `at`, so nothing here reads the clock), which keeps this logic
 * unit-tested off a browser; the DOM wiring (carets, ledger rows, click-to-edit)
 * is certified by the workbench flow spec, matching the spec's testing decisions.
 *
 * The observation shape matches what the v0 comparison-record schema needs
 * (parent #8, decision uncompose#65): wall-clock `at`, optional `position`
 * (playback seconds), optional `loop` index into the record's `loops[]` when a
 * region is active, optional `candidate` attachment, and free `text`.
 */
import type { Label, Region } from "./transport";

/** A pin's attachment: one candidate, or `both` (the ◆ caret). */
export type Target = Label | "both";

export interface Observation {
  id: string;
  /** Wall-clock time the observation was made (ISO 8601). */
  at: string;
  /** Playback position (seconds) when pinned, or null for an untethered note. */
  position: number | null;
  /** Index into the record's `loops[]` when made inside the active region. */
  loop: number | null;
  /** The candidate(s) the pin is tagged to, or null for a general note. */
  candidate: Target | null;
  /** Free text (may be empty for a quick pin, filled in later in the ledger). */
  text: string;
}

/**
 * The ledger: the current observations plus the undo/redo history as two stacks
 * of prior/next observation sets. Snapshotting the whole set keeps every kind of
 * change (create, edit, delete) uniformly undoable.
 */
export interface Ledger {
  observations: Observation[];
  past: Observation[][];
  future: Observation[][];
}

export const emptyLedger: Ledger = { observations: [], past: [], future: [] };

/** The caret glyph for a pin's attachment (▼ A above, ◆ both, ▲ B below). */
export function caretGlyph(candidate: Target | null): string {
  if (candidate === "A") return "▼";
  if (candidate === "B") return "▲";
  return "◆";
}

/** Build an observation, deriving the loop reference from the active region. */
export function makeObservation(params: {
  id: string;
  at: string;
  candidate: Target | null;
  position: number | null;
  region: Region | null;
  text: string;
}): Observation {
  return {
    id: params.id,
    at: params.at,
    position: params.position,
    // One region in M3, so a pin made inside it references loop index 0.
    loop: params.region ? 0 : null,
    candidate: params.candidate,
    text: params.text,
  };
}

/** Replace the observation set, pushing the old one onto the undo stack. */
function commit(ledger: Ledger, next: Observation[]): Ledger {
  return {
    observations: next,
    past: [...ledger.past, ledger.observations],
    future: [],
  };
}

export function addObservation(ledger: Ledger, obs: Observation): Ledger {
  return commit(ledger, [...ledger.observations, obs]);
}

export function editObservation(ledger: Ledger, id: string, text: string): Ledger {
  return commit(
    ledger,
    ledger.observations.map((o) => (o.id === id ? { ...o, text } : o)),
  );
}

export function deleteObservation(ledger: Ledger, id: string): Ledger {
  return commit(
    ledger,
    ledger.observations.filter((o) => o.id !== id),
  );
}

export function undo(ledger: Ledger): Ledger {
  if (ledger.past.length === 0) return ledger;
  const previous = ledger.past[ledger.past.length - 1];
  return {
    observations: previous,
    past: ledger.past.slice(0, -1),
    future: [ledger.observations, ...ledger.future],
  };
}

export function redo(ledger: Ledger): Ledger {
  if (ledger.future.length === 0) return ledger;
  const next = ledger.future[0];
  return {
    observations: next,
    past: [...ledger.past, ledger.observations],
    future: ledger.future.slice(1),
  };
}

export function canUndo(ledger: Ledger): boolean {
  return ledger.past.length > 0;
}

export function canRedo(ledger: Ledger): boolean {
  return ledger.future.length > 0;
}
