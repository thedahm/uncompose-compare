/**
 * The verdict modal (issue #16): prefer A, prefer B, or record an honest "no
 * preference", with a colour-coded 1-5 confidence the schema requires exactly
 * when a candidate is preferred.
 *
 * The modal edits a *draft*. Save is the engrave gate — it commits the draft and
 * is what a later conclude writes — so closing without saving discards the edit
 * rather than quietly changing what the record will say. Saving does not
 * finalize anything: reopening edits the verdict again, right up until conclude.
 */
import { Modal } from "./Modal";
import { Stars, TIER_COLOR } from "./ConfidenceStars";
import { confidenceTier, type Verdict } from "./record";

/** The verdict a fresh session starts with: undecided, nothing engraved. */
export const emptyVerdict: Verdict = {
  preference: null,
  confidence: null,
  criterion: "",
  summary: "",
};

/** True once a candidate (not "no preference") is picked — confidence applies. */
export function preferenceChosen(verdict: Verdict): boolean {
  return verdict.preference === "A" || verdict.preference === "B";
}

/**
 * A verdict is savable once a decision is made: a chosen candidate needs a
 * confidence (the schema requires it), while "no preference" needs nothing.
 */
export function canSave(verdict: Verdict): boolean {
  return (
    verdict.preference !== null && (!preferenceChosen(verdict) || verdict.confidence !== null)
  );
}

export function VerdictModal({
  verdict,
  onChange,
  context,
  onContextChange,
  onSave,
  onClose,
}: {
  verdict: Verdict;
  onChange: (update: (v: Verdict) => Verdict) => void;
  context: string;
  onContextChange: (context: string) => void;
  onSave: () => void;
  onClose: () => void;
}) {
  const chosen = preferenceChosen(verdict);

  return (
    <Modal testid="verdict-modal" label="Verdict" width={460} onClose={onClose}>
      <h2 style={{ marginTop: 0 }}>Verdict</h2>

      {/* Prefer buttons select without closing the modal (#61). */}
      <div role="group" aria-label="Preference" style={{ display: "flex", gap: 8 }}>
        {(["A", "B", "none"] as const).map((choice) => (
          <button
            key={choice}
            data-testid={`prefer-${choice}`}
            aria-pressed={verdict.preference === choice}
            onClick={() => onChange((v) => ({ ...v, preference: choice }))}
            style={{
              padding: "6px 12px",
              fontWeight: verdict.preference === choice ? "bold" : "normal",
              outline: verdict.preference === choice ? "2px solid #5cd67a" : "1px solid #444",
            }}
          >
            {choice === "none" ? "No preference" : `Prefer ${choice}`}
          </button>
        ))}
      </div>

      {/* Confidence stars: required when a candidate is preferred, colour coded
          red 1-2 / amber 3 / green 4-5, and hidden for no-preference. */}
      {chosen && (
        <div style={{ marginTop: 12 }}>
          <label style={{ display: "block", marginBottom: 4 }}>
            Confidence <span style={{ color: "#888" }}>(required)</span>
          </label>
          <div data-testid="confidence-stars" role="group" aria-label="Confidence">
            {[1, 2, 3, 4, 5].map((n) => {
              const filled = verdict.confidence !== null && n <= verdict.confidence;
              const color =
                verdict.confidence !== null
                  ? TIER_COLOR[confidenceTier(verdict.confidence)]
                  : "#888";
              return (
                <button
                  key={n}
                  data-testid={`confidence-${n}`}
                  aria-pressed={filled}
                  title={`${n} of 5`}
                  onClick={() => onChange((v) => ({ ...v, confidence: n }))}
                  style={{
                    background: "none",
                    border: "none",
                    cursor: "pointer",
                    fontSize: 22,
                    color: filled ? color : "#555",
                  }}
                >
                  {filled ? "★" : "☆"}
                </button>
              );
            })}
          </div>
        </div>
      )}

      <label style={{ display: "block", marginTop: 12 }}>
        Criterion <span style={{ color: "#888" }}>(optional)</span>
        <input
          data-testid="verdict-criterion"
          value={verdict.criterion}
          placeholder="What you judged on (e.g. clarity)"
          onChange={(e) => onChange((v) => ({ ...v, criterion: e.target.value }))}
          style={{ display: "block", width: "100%", padding: 4, marginTop: 4 }}
        />
      </label>
      <label style={{ display: "block", marginTop: 12 }}>
        Summary <span style={{ color: "#888" }}>(optional)</span>
        <input
          data-testid="verdict-summary-input"
          value={verdict.summary}
          placeholder="A one-line summary of the decision"
          onChange={(e) => onChange((v) => ({ ...v, summary: e.target.value }))}
          style={{ display: "block", width: "100%", padding: 4, marginTop: 4 }}
        />
      </label>
      <label style={{ display: "block", marginTop: 12 }}>
        Session context <span style={{ color: "#888" }}>(optional)</span>
        <input
          data-testid="verdict-context"
          value={context}
          placeholder="Why you were comparing these files"
          onChange={(e) => onContextChange(e.target.value)}
          style={{ display: "block", width: "100%", padding: 4, marginTop: 4 }}
        />
      </label>

      <div style={{ display: "flex", gap: 8, marginTop: 16, alignItems: "center" }}>
        <button data-testid="save-verdict" disabled={!canSave(verdict)} onClick={onSave}>
          Save verdict
        </button>
        <button data-testid="cancel-verdict" onClick={onClose}>
          Close
        </button>
        {chosen && verdict.confidence !== null && (
          <span data-testid="verdict-draft-stars" style={{ marginLeft: "auto" }}>
            <Stars confidence={verdict.confidence} />
          </span>
        )}
        {chosen && verdict.confidence === null && (
          <span style={{ color: "#ffcf6b" }}>Pick a confidence to save.</span>
        )}
      </div>
      <p style={{ color: "#888", fontSize: 12, marginBottom: 0 }}>
        Closing without saving discards these edits; the saved verdict is what a
        conclude writes.
      </p>
    </Modal>
  );
}
