/**
 * The `?` help modal: the whole keymap, in one place, so the keyboard-first
 * transport is discoverable without documentation (issue #12, keymap #61).
 */
import { Modal } from "./Modal";

export function HelpModal({ onClose }: { onClose: () => void }) {
  return (
    <Modal testid="help-modal" label="Keyboard help" width={420} onClose={onClose}>
      <h2 style={{ marginTop: 0 }}>Keyboard</h2>
      <section>
        <h3>Transport</h3>
        <ul>
          <li><kbd>space</kbd> — play / stop</li>
          <li><kbd>x</kbd> — switch audible candidate</li>
          <li><kbd>←</kbd> / <kbd>→</kbd> or <kbd>-</kbd> / <kbd>=</kbd> — step 2 s</li>
          <li><kbd>home</kbd> or <kbd>0</kbd> — rewind to start</li>
          <li>click a waveform — seek</li>
        </ul>
      </section>
      <section>
        <h3>Region &amp; loop</h3>
        <ul>
          <li>drag a waveform — select a region</li>
          <li><kbd>r</kbd> — loop the region (plays into it, then loops)</li>
          <li><kbd>u</kbd> — clear the region</li>
        </ul>
      </section>
      <section>
        <h3>Views</h3>
        <ul>
          <li>Waveform / Loudness / Spectral toggle — switches every display</li>
        </ul>
      </section>
      <section>
        <h3>Observations &amp; ledger</h3>
        <ul>
          <li><kbd>enter</kbd> — pin an observation on the live candidate</li>
          <li><kbd>shift</kbd>+<kbd>enter</kbd> — pin on both candidates</li>
          <li><kbd>tab</kbd> — focus the composer to write a note</li>
          <li>click a timestamp — seek; click text — edit; ✕ — delete</li>
          <li><kbd>ctrl</kbd>+<kbd>z</kbd> / <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>z</kbd> — undo / redo</li>
        </ul>
      </section>
      <section>
        <h3>Verdict &amp; conclude</h3>
        <ul>
          <li>Set verdict — prefer A, B, or no preference; stars set confidence</li>
          <li>Save engraves the verdict but keeps it editable</li>
          <li>Conclude — write the immutable comparison record</li>
        </ul>
      </section>
      <section>
        <h3>Help</h3>
        <ul>
          <li><kbd>?</kbd> — toggle this help</li>
        </ul>
      </section>
      <button onClick={onClose}>Close</button>
    </Modal>
  );
}
