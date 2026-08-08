// Kill the packaged binary launched in global-setup and clean up its pid/url
// files, so a run never leaks the server process.
import { readFileSync, rmSync } from "node:fs";
import { PID_FILE, URL_FILE } from "./global-setup.mjs";

export default async function globalTeardown() {
  try {
    const pid = Number(readFileSync(PID_FILE, "utf8").trim());
    if (pid) process.kill(pid);
  } catch {
    // Already gone, or never started — nothing to reap.
  }
  for (const f of [PID_FILE, URL_FILE]) rmSync(f, { force: true });
}
