/**
 * Spike 1 stub page: the "embedded hello skeleton".
 *
 * This is intentionally minimal — it carries only enough to prove the
 * packaging chain (Vite build → rust-embed → served page). The Web Audio
 * plumbing the #73 sync harness needs lands in a later issue; here we just
 * render a stable marker the served-page tests can assert against.
 */
export function App() {
  return (
    <main>
      <h1 data-testid="hello">uncompose-compare</h1>
      <p>Embedded hello skeleton served from the Rust binary.</p>
    </main>
  );
}
