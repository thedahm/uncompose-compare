/**
 * The observation ledger (issue #15): the composer plus the chronological list
 * of pins, each seekable, editable in place, and deletable, with undo/redo over
 * the whole set.
 *
 * The pure store lives in `ledger.ts`; this is its surface. The one piece of
 * state it owns is which row is being edited — nothing outside the ledger cares.
 * The highlighted pin is shared with the stage's carets, so hovering or clicking
 * a caret lights up the matching row *and scrolls it into view*: with a long
 * session's ledger the "found" entry would otherwise be off-screen (story 11).
 */
import { useEffect, useRef, useState, type MutableRefObject } from "react";
import { formatTime, type Label } from "./transport";
import {
  canRedo,
  canUndo,
  caretGlyph,
  deleteObservation,
  editObservation,
  redo,
  undo,
  type Ledger,
} from "./ledger";

export function LedgerSection({
  ledger,
  setLedger,
  composer,
  setComposer,
  submitComposer,
  composerRef,
  live,
  activePin,
  setActivePin,
  onSeek,
}: {
  ledger: Ledger;
  setLedger: (update: (l: Ledger) => Ledger) => void;
  composer: string;
  setComposer: (text: string) => void;
  /** Submit the composer's text — `both` tags both candidates (shift+enter). */
  submitComposer: (both: boolean) => void;
  composerRef: MutableRefObject<HTMLInputElement | null>;
  live: Label;
  activePin: string | null;
  setActivePin: (id: string | null) => void;
  /** Seek to an observation and light up its caret/row pair. */
  onSeek: (id: string, position: number | null) => void;
}) {
  // The row being edited in place (null when none) — local to the ledger.
  const [editing, setEditing] = useState<{ id: string; text: string } | null>(null);

  const rows = useRef(new Map<string, HTMLLIElement>());
  useEffect(() => {
    if (!activePin) return;
    // `nearest` keeps an already-visible row still; only an off-screen one moves.
    rows.current.get(activePin)?.scrollIntoView?.({ block: "nearest" });
  }, [activePin]);

  // Commit the in-place edit (enter or blur both land here).
  const commitEdit = () => {
    if (!editing) return;
    const { id, text } = editing;
    setLedger((l) => editObservation(l, id, text));
    setEditing(null);
  };

  return (
    <section data-testid="ledger" style={{ marginTop: 16 }}>
      <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8 }}>
        <h2 style={{ fontSize: 15, margin: 0 }}>Observations</h2>
        <button
          data-testid="ledger-undo"
          disabled={!canUndo(ledger)}
          onClick={() => setLedger(undo)}
        >
          Undo (⌃z)
        </button>
        <button
          data-testid="ledger-redo"
          disabled={!canRedo(ledger)}
          onClick={() => setLedger(redo)}
        >
          Redo (⌃⇧z)
        </button>
      </div>
      <div style={{ display: "flex", gap: 8, marginBottom: 8 }}>
        <input
          ref={composerRef}
          data-testid="composer"
          value={composer}
          placeholder="Note what you hear… (enter: live, shift+enter: both)"
          onChange={(e) => setComposer(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submitComposer(e.shiftKey);
            }
          }}
          style={{ flex: 1, padding: 4 }}
        />
        <button data-testid="composer-pin" onClick={() => submitComposer(false)}>
          Pin ({live})
        </button>
      </div>
      {ledger.observations.length === 0 ? (
        <p data-testid="ledger-empty" style={{ color: "#888" }}>
          No observations yet — press <kbd>enter</kbd> to pin one.
        </p>
      ) : (
        <ol data-testid="ledger-entries" style={{ listStyle: "none", padding: 0, margin: 0 }}>
          {ledger.observations.map((o) => {
            const active = activePin === o.id;
            return (
              <li
                key={o.id}
                ref={(el) => {
                  if (el) rows.current.set(o.id, el);
                  else rows.current.delete(o.id);
                }}
                data-testid={`ledger-entry-${o.id}`}
                data-active={String(active)}
                onMouseEnter={() => setActivePin(o.id)}
                onMouseLeave={() => setActivePin(null)}
                style={{
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  padding: 4,
                  background: active ? "#1d2a1d" : "transparent",
                }}
              >
                <button
                  data-testid={`ledger-seek-${o.id}`}
                  title="Seek to this observation"
                  onClick={() => onSeek(o.id, o.position)}
                  style={{ fontVariantNumeric: "tabular-nums" }}
                >
                  <span data-testid={`ledger-caret-${o.id}`}>{caretGlyph(o.candidate)}</span>{" "}
                  {o.position === null ? "—" : formatTime(o.position)}
                  {o.loop !== null ? " ⟳" : ""}
                </button>
                {editing?.id === o.id ? (
                  <input
                    data-testid={`ledger-text-input-${o.id}`}
                    autoFocus
                    value={editing.text}
                    onChange={(e) => setEditing({ id: o.id, text: e.target.value })}
                    onBlur={commitEdit}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        commitEdit();
                      } else if (e.key === "Escape") {
                        e.preventDefault();
                        setEditing(null);
                      }
                    }}
                    style={{ flex: 1, padding: 2 }}
                  />
                ) : (
                  <span
                    data-testid={`ledger-text-${o.id}`}
                    onClick={() => setEditing({ id: o.id, text: o.text })}
                    style={{ flex: 1, cursor: "text", color: o.text ? "#eee" : "#888" }}
                  >
                    {o.text || "(click to add a note)"}
                  </span>
                )}
                <button
                  data-testid={`ledger-delete-${o.id}`}
                  title="Delete this observation"
                  onClick={() => setLedger((l) => deleteObservation(l, o.id))}
                >
                  ✕
                </button>
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}
