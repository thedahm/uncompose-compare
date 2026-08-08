/**
 * A waveform view for one candidate (issue #12): the min/max peak envelope
 * drawn on a canvas, a playhead overlay, and click-to-seek. Used for both the
 * primary stage waveform and the A/B lane rows. Loudness and spectral views are
 * later sub-issues; this is the waveform the workbench core needs.
 */
import { useEffect, useRef } from "react";
import { clampPosition } from "./transport";

interface WaveformProps {
  peaks: { min: Float32Array; max: Float32Array };
  duration: number;
  position: number;
  onSeek: (sec: number) => void;
  color: string;
  height: number;
  testid: string;
}

export function Waveform({
  peaks,
  duration,
  position,
  onSeek,
  color,
  height,
  testid,
}: WaveformProps) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

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

  const seekFromEvent = (e: React.MouseEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    if (rect.width === 0) return;
    const frac = (e.clientX - rect.left) / rect.width;
    onSeek(clampPosition(frac * duration, duration));
  };

  const playFrac = duration > 0 ? clampPosition(position, duration) / duration : 0;

  return (
    <div
      data-testid={testid}
      onClick={seekFromEvent}
      style={{
        position: "relative",
        width: "100%",
        height,
        cursor: "pointer",
        background: "#111",
        overflow: "hidden",
      }}
    >
      <canvas
        ref={canvasRef}
        style={{ width: "100%", height: "100%", display: "block" }}
      />
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
