// Launching the binary and catching the tokened loopback URL it prints — the one
// place that knows the printed-URL protocol.
//
// Playwright's global setup and every spec that needs its own server (the blind
// flows launch their own `--blind` process, since global-setup runs a sighted
// one) go through here, so the timeout, the failure messages, and the stdout
// line buffering stay in one implementation rather than one per caller.
import { spawn } from "node:child_process";

/** How long the binary gets to print its URL before the launch is called failed. */
const URL_TIMEOUT_MS = 15_000;

/** The loopback prefix every printed URL starts with (#72: never off this host). */
const LOOPBACK = "http://127.0.0.1:";

/**
 * Spawn `bin` with `args` and resolve the first line it prints, which the CLI
 * contract says is the tokened loopback URL.
 *
 * Returns `{ child, url }`: the process (the caller owns killing it) and a
 * promise for the URL, rejected — never left hanging — if the binary fails to
 * launch, exits first, prints something else, or says nothing in time.
 */
export function spawnServed(bin, args, options = {}) {
  const child = spawn(bin, args, { stdio: ["ignore", "pipe", "inherit"], ...options });
  // Name the invocation, not just the binary, so a failure in one of several
  // concurrent launches says which one it was.
  const what = [bin, ...args.filter((arg) => arg.startsWith("--"))].join(" ");

  const url = new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`${what} printed no URL within ${URL_TIMEOUT_MS / 1000}s`)),
      URL_TIMEOUT_MS,
    );
    const fail = (message) => {
      clearTimeout(timer);
      reject(new Error(message));
    };
    child.on("error", (err) => fail(`failed to launch ${what}: ${err.message}`));
    child.on("exit", (code) => fail(`${what} exited (code ${code}) before printing a URL`));

    let buf = "";
    child.stdout.on("data", (chunk) => {
      buf += chunk.toString();
      const nl = buf.indexOf("\n");
      if (nl === -1) return;
      const line = buf.slice(0, nl).trim();
      clearTimeout(timer);
      if (!line.startsWith(LOOPBACK)) reject(new Error(`expected a loopback URL, got: ${line}`));
      else resolve(line);
    });
  });

  return { child, url };
}
