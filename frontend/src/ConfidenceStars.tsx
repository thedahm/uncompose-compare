/**
 * The confidence readout, shared by every place a saved verdict shows itself
 * (the preferred lane row, the verdict summary) and by the modal's picker.
 *
 * Colour carries the tier — red for a hesitant 1-2, amber for a middling 3,
 * green for a confident 4-5 (confirmed verdict UX, uncompose#61) — so a glance
 * at the stage says how sure the listener was, not just which candidate won.
 */
import type { CSSProperties } from "react";
import { confidenceTier } from "./record";

export const TIER_COLOR = { low: "#ff5c5c", mid: "#ffcf6b", high: "#5cd67a" } as const;

/** A read-only five-star string, filled up to `confidence`. */
export function starString(confidence: number): string {
  return "★".repeat(confidence) + "☆".repeat(5 - confidence);
}

export function Stars({
  confidence,
  testid,
  title,
  style,
}: {
  confidence: number;
  testid?: string;
  title?: string;
  style?: CSSProperties;
}) {
  return (
    <span
      data-testid={testid}
      title={title}
      style={{ color: TIER_COLOR[confidenceTier(confidence)], whiteSpace: "nowrap", ...style }}
    >
      {starString(confidence)}
    </span>
  );
}
