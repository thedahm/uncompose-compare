#!/usr/bin/env bash
#
# Clean-install proof (spec #1, issue #3, stories 2/3/9):
# install the wheel with pip alone into a fresh venv where the build toolchain
# (node, cargo, rustc, maturin) is NOT on PATH, launch the installed
# `uncompose-compare`, and assert over HTTP that it serves the embedded page.
#
# This is the one seam the spike verifies: the installed wheel's process
# boundary. Nothing below the wheel is inspected -- serving proves the contents.
#
# Usage: scripts/clean-install-proof.sh [path/to/wheel]
#        (defaults to the single wheel in target/wheels/)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
default_wheel=("$REPO_ROOT"/target/wheels/*.whl)
WHEEL="${1:-${default_wheel[0]}}"

if [ ! -f "$WHEEL" ]; then
    echo "clean-install-proof: wheel not found: $WHEEL" >&2
    echo "build one first with scripts/build-wheel.sh" >&2
    exit 1
fi

WORK="$(mktemp -d)"
VENV="$WORK/venv"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

echo ">> creating fresh venv at $VENV" >&2
python3 -m venv "$VENV" >/dev/null 2>&1 || python3 -m venv --without-pip "$VENV"

# On a standard runner `python -m venv` provides pip. Where ensurepip is missing,
# bootstrap it -- pip is the install tool under test, not a build toolchain.
if ! "$VENV/bin/python" -m pip --version >/dev/null 2>&1; then
    echo ">> venv has no pip; bootstrapping" >&2
    "$VENV/bin/python" -m ensurepip --upgrade >/dev/null 2>&1 || {
        curl -sSfL https://bootstrap.pypa.io/get-pip.py -o "$WORK/get-pip.py"
        "$VENV/bin/python" "$WORK/get-pip.py" >/dev/null
    }
fi

# The proof's whole point: a PATH with the venv and core system dirs only. If any
# build tool leaks in, the "no toolchain needed" story is untested -- fail hard.
CLEAN_PATH="$VENV/bin:/usr/bin:/bin"
for tool in node npm cargo rustc maturin; do
    if PATH="$CLEAN_PATH" command -v "$tool" >/dev/null 2>&1; then
        echo "clean-install-proof: $tool is on PATH; the toolchain-free proof is invalid" >&2
        exit 1
    fi
done
echo ">> confirmed toolchain-free PATH (no node/npm/cargo/rustc/maturin)" >&2

echo ">> pip install $(basename "$WHEEL")" >&2
PATH="$CLEAN_PATH" "$VENV/bin/python" -m pip install --no-index "$WHEEL" >&2

BIN="$VENV/bin/uncompose-compare"
if [ ! -x "$BIN" ]; then
    echo "clean-install-proof: entry point not installed at $BIN" >&2
    exit 1
fi

# The CLI takes two candidate audio files (`uncompose-compare <A> <B>`).
# Synthesize two tiny WAVs with the venv's python (stdlib only, so nothing
# beyond the clean PATH is required) — no binary fixtures live in the repo.
echo ">> synthesizing two candidate WAVs with the venv's python" >&2
PATH="$CLEAN_PATH" "$VENV/bin/python" - "$WORK/a.wav" "$WORK/b.wav" <<'PY'
import struct, sys, wave

def synth(path, seed):
    rate, frames = 44100, 4410  # 0.1 s, stereo, 16-bit
    state = seed & 0xFFFFFFFF
    samples = []
    for _ in range(frames * 2):
        state = (state * 1664525 + 1013904223) & 0xFFFFFFFF
        samples.append(int((state / 0xFFFFFFFF - 0.5) * 0.5 * 32767))
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(struct.pack(f"<{len(samples)}h", *samples))

synth(sys.argv[1], 1)
synth(sys.argv[2], 2)
PY

echo ">> launching installed binary and verifying it serves the embedded page" >&2
# Launch under the clean PATH with the two candidates; capture the URL it prints
# on its first stdout line. XDG_CACHE_HOME keeps the proxy cache inside $WORK so
# the proof never touches the machine's real cache.
url_file="$WORK/url"
PATH="$CLEAN_PATH" XDG_CACHE_HOME="$WORK/cache" "$BIN" "$WORK/a.wav" "$WORK/b.wav" >"$url_file" 2>"$WORK/stderr" &
server_pid=$!
stop() { kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; cleanup; }
trap stop EXIT

# Wait for the URL line (bounded).
url=""
for _ in $(seq 1 50); do
    url="$(head -n1 "$url_file" 2>/dev/null || true)"
    [ -n "$url" ] && break
    sleep 0.1
done

if [ -z "$url" ]; then
    echo "clean-install-proof: binary printed no URL. stderr:" >&2
    cat "$WORK/stderr" >&2
    exit 1
fi
echo ">> installed binary serving at $url" >&2

# Verify with the venv's python (urllib) so no extra tool is required. Asserts the
# #72 no-store header and the embedded-page marker.
PATH="$CLEAN_PATH" "$VENV/bin/python" - "$url" <<'PY'
import sys, urllib.request
url = sys.argv[1]
with urllib.request.urlopen(url, timeout=5) as r:
    body = r.read().decode("utf-8", "replace")
    cache = (r.headers.get("Cache-Control") or "").lower()
assert "uncompose-compare" in body, f"served page missing app marker:\n{body[:400]}"
assert "no-store" in cache, f"expected Cache-Control: no-store, got {cache!r}"
print(">> OK: embedded page served with no-store from the pip-installed wheel", file=sys.stderr)
PY

echo >&2
echo "=== clean-install proof: PASS ===" >&2
echo "pip-installed the wheel into a toolchain-free venv and served the embedded page." >&2
