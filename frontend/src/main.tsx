import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { installSyncHarness } from "./sync";

// Expose the Web Audio plumbing the #73 sync harness drives before React
// mounts, so the harness can call it as soon as the served page loads.
installSyncHarness();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
