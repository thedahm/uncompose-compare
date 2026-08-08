/**
 * M3 listening slice, first cut (issue #10): a minimal page that identifies the
 * two loaded candidates.
 *
 * The full #61 workbench is a later ticket; this page's job is to demo the
 * two-file command — render candidates A and B with their file names and
 * durations, and surface a visible duration-mismatch warning (in samples and
 * ms) when they differ, without blocking either from loading. It fetches the
 * `/session` endpoint the Rust binary serves (the cookie the page was seeded
 * with authorizes the request).
 *
 * The `data-testid="hello"` marker and the `uncompose-compare` string are kept
 * as the stable anchors the served-page HTTP tests assert against; the Web
 * Audio sync plumbing the #73 harness drives lives in `sync.ts`, registered on
 * `window` from `main.tsx`.
 */
import { useEffect, useState } from "react";

interface Candidate {
  label: string;
  name: string;
  path: string;
  sha256: string;
  size: number;
  frames: number;
  duration_ms: number;
  sample_rate: number;
  channels: number;
}

interface SessionMeta {
  candidates: Candidate[];
  duration_mismatch: boolean;
  duration_delta_samples: number;
  duration_delta_ms: number;
}

function formatDuration(ms: number): string {
  const secs = ms / 1000;
  return `${secs.toFixed(3)} s`;
}

export function App() {
  const [session, setSession] = useState<SessionMeta | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    fetch("/session")
      .then((r) => {
        if (!r.ok) throw new Error(`session ${r.status}`);
        return r.json() as Promise<SessionMeta>;
      })
      .then((s) => {
        if (!cancelled) setSession(s);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <main>
      <h1 data-testid="hello">uncompose-compare</h1>

      {session?.duration_mismatch && (
        <p role="alert" data-testid="duration-mismatch">
          Duration mismatch: the candidates differ by{" "}
          {session.duration_delta_samples} samples ({session.duration_delta_ms.toFixed(1)} ms).
          Both still load.
        </p>
      )}

      {error && <p data-testid="session-error">Could not load session: {error}</p>}

      {session && (
        <ul data-testid="candidates">
          {session.candidates.map((c) => (
            <li key={c.label} data-testid={`candidate-${c.label}`}>
              <strong>{c.label}</strong>: {c.name} — {formatDuration(c.duration_ms)} (
              {c.frames} samples @ {c.sample_rate} Hz, {c.channels} ch)
            </li>
          ))}
        </ul>
      )}
    </main>
  );
}
