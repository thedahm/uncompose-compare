// Kill the packaged binary launched in global-setup and clean up its pid/url
// files, so a run never leaks the server process.
import { readFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const dir = path.dirname(fileURLToPath(import.meta.url));
const URL_FILE = path.join(dir, ".served-url");
const PID_FILE = path.join(dir, ".server-pid");

export default async function globalTeardown() {
  try {
    const pid = Number(readFileSync(PID_FILE, "utf8").trim());
    if (pid) process.kill(pid);
  } catch {
    // Already gone, or never started — nothing to reap.
  }
  for (const f of [PID_FILE, URL_FILE]) rmSync(f, { force: true });
}
