/**
 * The workbench's modal shell: a dimmed full-screen overlay that closes on a
 * backdrop click or Escape, wrapping a centered panel that doesn't. Both the
 * verdict and the help modal are this shape, so it lives in one place.
 */
import { useEffect, type ReactNode } from "react";

export function Modal({
  testid,
  label,
  width,
  onClose,
  children,
}: {
  testid: string;
  label: string;
  /** The panel's maximum width in px. */
  width: number;
  onClose: () => void;
  children: ReactNode;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      data-testid={testid}
      role="dialog"
      aria-label={label}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.8)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
      onClick={onClose}
    >
      <div
        style={{ background: "#161616", padding: 24, maxWidth: width, width: "90%" }}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}
