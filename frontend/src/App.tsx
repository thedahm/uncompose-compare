/**
 * Spike stub page.
 *
 * It stays intentionally minimal — a stable marker the served-page HTTP tests
 * assert against. The Web Audio dual-source plumbing the #73 sync harness
 * exercises lives in `sync.ts` (registered on `window` from `main.tsx`), not in
 * this component: the harness drives that seam, not the DOM.
 */
export function App() {
  return (
    <main>
      <h1 data-testid="hello">uncompose-compare</h1>
      <p>Embedded stub page served from the Rust binary — carries the sync plumbing only.</p>
    </main>
  );
}
