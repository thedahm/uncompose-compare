/**
 * A waveform view for one candidate (issue #12): the min/max peak envelope
 * drawn on a canvas, a playhead overlay, and click-to-seek. Used for both the
 * primary stage waveform and the A/B lane rows. Loudness and spectral views are
 * later sub-issues; this is the waveform the workbench core needs.
 *
 * Drag-to-select (issue #13): pressing and dragging paints a region; a plain
 * click (no meaningful drag) still seeks. The selected region is drawn as an
 * overlay so the same region shows consistently on every display, and reads as
 * "looping" when the loop is active.
 */
import { useEffect, useRef, useState } from "react";
import { clampPosition, orderedRegion, type Region } from "./transport";

interface WaveformProps {
  peaks: { min: Float32Array; max: Float32Array };
  duration: number;
  position: number;
  onSeek: (sec: number) => void;
  region: Region | null;
  looping: boolean;
  onSelectRegion: (a: number, b: number) => void;
  /** Fired on a genuine click (not a drag) — the lane rows use it to audition. */
  onActivate?: () => void;
  color: string;
  height: number;
  testid: string;
}

/** Below this many pixels of travel a press is a click (seek), not a drag. */
const DRAG_THRESHOLD_PX = 4;

export function Waveform({
  peaks,
  duration,
  position,
  onSeek,
  region,
  looping,
  onSelectRegion,
  onActivate,
  color,
  height,
  testid,
}: WaveformProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // The in-progress drag: the press pixel (for the click/drag threshold) and the
  // press point as a track fraction; null when not dragging.
  const dragRef = useRef<{ startX: number; startFrac: number } | null>(null);
  const [preview, setPreview] = useState<{ a: number; b: number } | null>(null);

  // Redraw the envelope only when the peaks or colour change — the playhead is a
  // cheap DOM overlay, so animation never repaints the canvas.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const w = peaks.max.length;
    canvas.width = w;
    canvas.height = height;
    const g = canvas.getContext("2d");
    if (!g) return;
    g.clearRect(0, 0, w, height);
    g.fillStyle = color;
    const mid = height / 2;
    for (let x = 0; x < w; x++) {
      const top = mid - peaks.max[x] * mid;
      const bottom = mid - peaks.min[x] * mid;
      g.fillRect(x, top, 1, Math.max(1, bottom - top));
    }
  }, [peaks, color, height]);

  const fracFromEvent = (e: React.PointerEvent<HTMLDivElement>): number => {
    const rect = e.currentTarget.getBoundingClientRect();
    if (rect.width === 0) return 0;
    return clampPosition((e.clientX - rect.left) / rect.width, 1);
  };

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    const frac = fracFromEvent(e);
    dragRef.current = { startX: e.clientX, startFrac: frac };
    setPreview({ a: frac, b: frac });
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    setPreview((p) => (p ? { a: p.a, b: fracFromEvent(e) } : p));
  };

  const onPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    dragRef.current = null;
    setPreview(null);
    if (!drag) return;
    const endFrac = fracFromEvent(e);
    if (Math.abs(e.clientX - drag.startX) < DRAG_THRESHOLD_PX) {
      // A click, not a drag: seek to the press point (and, on a lane, audition).
      onSeek(endFrac * duration);
      onActivate?.();
    } else {
      // A drag: select the region between the press and release points.
      onSelectRegion(drag.startFrac * duration, endFrac * duration);
    }
  };

  const playFrac = duration > 0 ? clampPosition(position, duration) / duration : 0;
  const regionFrac =
    region && duration > 0
      ? { start: region.start / duration, end: region.end / duration }
      : null;
  const previewFrac = preview ? orderedRegion(preview.a, preview.b, 1) : null;
  // While dragging, show the live preview; otherwise the committed region. Only
  // a committed region reads as looping — a drag preview is always neutral.
  const shown = previewFrac ?? regionFrac;
  const shownLooping = looping && !previewFrac;
  const regionBorder = `1px solid ${shownLooping ? "#78dc8c" : "#bbb"}`;

  return (
    <div
      data-testid={testid}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      style={{
        position: "relative",
        width: "100%",
        height,
        cursor: "pointer",
        background: "#111",
        overflow: "hidden",
        touchAction: "none",
      }}
    >
      <canvas
        ref={canvasRef}
        style={{ width: "100%", height: "100%", display: "block" }}
      />
      {shown && shown.end > shown.start && (
        <div
          data-testid={`${testid}-region`}
          data-looping={String(shownLooping)}
          style={{
            position: "absolute",
            top: 0,
            bottom: 0,
            left: `${shown.start * 100}%`,
            width: `${(shown.end - shown.start) * 100}%`,
            background: shownLooping ? "rgba(120,220,140,0.28)" : "rgba(255,255,255,0.16)",
            borderLeft: regionBorder,
            borderRight: regionBorder,
            pointerEvents: "none",
          }}
        />
      )}
      <div
        data-testid={`${testid}-playhead`}
        style={{
          position: "absolute",
          top: 0,
          bottom: 0,
          left: `${playFrac * 100}%`,
          width: 2,
          background: "#fff",
          pointerEvents: "none",
        }}
      />
    </div>
  );
}
