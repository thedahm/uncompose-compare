/**
 * One candidate's audio display (issues #12–#14): a canvas view plus a region
 * overlay, a playhead overlay, click-to-seek, and drag-to-select. Used for both
 * the primary stage waveform and the A/B lane rows.
 *
 * The canvas renders one of three views selected by the shared `view` toggle
 * (issue #14): the raw sample envelope ("waveform"), the RMS loudness envelope
 * ("loudness"), or the FFT spectrogram ("spectral"), all precomputed once per
 * decoded buffer (`transport.ts`) and tinted with the candidate colour. The
 * time axis is identical across views, so the region overlay and the playhead —
 * both positioned as track fractions — render correctly in every view.
 *
 * Drag-to-select (issue #13): pressing and dragging paints a region; a plain
 * click (no meaningful drag) still seeks. The selected region is drawn as an
 * overlay so the same region shows consistently on every display, and reads as
 * "looping" when the loop is active.
 */
import { useEffect, useRef, useState } from "react";
import {
  clampPosition,
  orderedRegion,
  type CandidateViews,
  type Region,
  type ViewMode,
} from "./transport";
import { caretGlyph, type Target } from "./ledger";

/** One observation pin drawn as a caret over the stage waveform (issue #15). */
export interface Pin {
  id: string;
  position: number;
  candidate: Target | null;
}

interface WaveformProps {
  views: CandidateViews;
  view: ViewMode;
  duration: number;
  position: number;
  onSeek: (sec: number) => void;
  region: Region | null;
  looping: boolean;
  onSelectRegion: (a: number, b: number) => void;
  /** Fired on a genuine click (not a drag) — the lane rows use it to audition. */
  onActivate?: () => void;
  /** Observation carets to draw over the waveform (the stage only). */
  pins?: Pin[];
  /** The highlighted pin (two-way with the ledger), or null. */
  activePin?: string | null;
  onPinEnter?: (id: string) => void;
  onPinLeave?: () => void;
  onPinClick?: (id: string) => void;
  color: string;
  height: number;
  testid: string;
}

/** Below this many pixels of travel a press is a click (seek), not a drag. */
const DRAG_THRESHOLD_PX = 4;

/** Parse a `#rrggbb` colour into its 0–255 channels, for tinting the spectrogram. */
function rgbOf(hex: string): [number, number, number] {
  const n = parseInt(hex.replace("#", ""), 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

/**
 * Draw the selected `view` onto the canvas, sized to the view's own resolution
 * (CSS stretches it to the display). The waveform and loudness views draw a
 * symmetric envelope; the spectral view paints the spectrogram as a tinted
 * bitmap with high frequencies on top.
 */
function drawView(
  canvas: HTMLCanvasElement,
  views: CandidateViews,
  view: ViewMode,
  color: string,
  height: number,
): void {
  const g = canvas.getContext("2d");
  if (!g) return;

  if (view === "spectral") {
    const { columns, bins, data } = views.spectral;
    canvas.width = columns;
    canvas.height = bins;
    const img = g.createImageData(columns, bins);
    const [r, gr, b] = rgbOf(color);
    for (let c = 0; c < columns; c++) {
      for (let k = 0; k < bins; k++) {
        const v = data[c * bins + k];
        // Flip the frequency axis so the highest bin sits on top.
        const y = bins - 1 - k;
        const o = (y * columns + c) * 4;
        img.data[o] = r * v;
        img.data[o + 1] = gr * v;
        img.data[o + 2] = b * v;
        img.data[o + 3] = 255;
      }
    }
    g.putImageData(img, 0, 0);
    return;
  }

  // Envelope views (waveform / loudness) share the min/max drawing loop.
  const w = view === "loudness" ? views.loudness.length : views.peaks.max.length;
  canvas.width = w;
  canvas.height = height;
  g.clearRect(0, 0, w, height);
  g.fillStyle = color;
  const mid = height / 2;
  for (let x = 0; x < w; x++) {
    let top: number;
    let bottom: number;
    if (view === "loudness") {
      // A symmetric envelope around the centre from the RMS magnitude.
      const rms = views.loudness[x];
      top = mid - rms * mid;
      bottom = mid + rms * mid;
    } else {
      top = mid - views.peaks.max[x] * mid;
      bottom = mid - views.peaks.min[x] * mid;
    }
    g.fillRect(x, top, 1, Math.max(1, bottom - top));
  }
}

/** A caret's vertical placement: ▼ A above, ▲ B below, ◆ both centered. */
function caretPlacement(candidate: Target | null): React.CSSProperties {
  if (candidate === "A") return { top: 0, transform: "translateX(-50%)" };
  if (candidate === "B") return { bottom: 0, transform: "translateX(-50%)" };
  return { top: "50%", transform: "translate(-50%, -50%)" };
}

export function Waveform({
  views,
  view,
  duration,
  position,
  onSeek,
  region,
  looping,
  onSelectRegion,
  onActivate,
  pins,
  activePin,
  onPinEnter,
  onPinLeave,
  onPinClick,
  color,
  height,
  testid,
}: WaveformProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // The in-progress drag: the press pixel (for the click/drag threshold) and the
  // press point as a track fraction; null when not dragging.
  const dragRef = useRef<{ startX: number; startFrac: number } | null>(null);
  const [preview, setPreview] = useState<{ a: number; b: number } | null>(null);

  // Redraw only when the view, its data, or the colour change — the playhead is a
  // cheap DOM overlay, so animation never repaints the canvas.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    drawView(canvas, views, view, color, height);
  }, [views, view, color, height]);

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
      data-view={view}
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
      {/* Observation carets: ▼ above for A, ◆ overlaid for both, ▲ below for B.
          They sit above the drag/seek surface, so their pointer events are
          stopped from reaching it (a caret click finds its ledger entry, never
          seeks). */}
      {duration > 0 &&
        pins?.map((pin) => {
          const frac = clampPosition(pin.position, duration) / duration;
          const active = activePin === pin.id;
          return (
            <span
              key={pin.id}
              data-testid={`${testid}-caret-${pin.id}`}
              data-caret-candidate={pin.candidate ?? "none"}
              data-active={String(active)}
              onPointerDown={(e) => e.stopPropagation()}
              onPointerUp={(e) => e.stopPropagation()}
              onMouseEnter={() => onPinEnter?.(pin.id)}
              onMouseLeave={() => onPinLeave?.()}
              onClick={(e) => {
                e.stopPropagation();
                onPinClick?.(pin.id);
              }}
              style={{
                position: "absolute",
                left: `${frac * 100}%`,
                ...caretPlacement(pin.candidate),
                fontSize: 12,
                lineHeight: 1,
                cursor: "pointer",
                color: active ? "#fff" : "#bbb",
                textShadow: active ? "0 0 4px #fff" : "none",
                userSelect: "none",
              }}
            >
              {caretGlyph(pin.candidate)}
            </span>
          );
        })}
    </div>
  );
}
