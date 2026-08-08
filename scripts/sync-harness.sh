#!/usr/bin/env bash
#
# Sync-harness stage of the Track C packaging pipeline (spec #1, issue #5):
# run the #73 cross-engine Playwright harness against the *pip-installed*
# uncompose-compare binary's served page.
#
# Steps: pip-install the wheel into a fresh venv, install the harness's npm deps
# and Playwright browsers, generate the seeded-noise fixtures, then drive the
# three-engine matrix with UNCOMPOSE_BIN pointed at the installed binary. This is
# the one seam the spike verifies: the artifact a user's machine would run.
#
# Usage: scripts/sync-harness.sh [path/to/wheel]
#        (defaults to the single wheel in target/wheels/; builds one if absent)
#
# Env toggles (CI sets these itself):
#   SKIP_NPM_INSTALL=1   skip `npm install` in harness/ (deps already present)
#   SKIP_PW_INSTALL=1    skip `npx playwright install` (browsers already present)
#   PW_WITH_DEPS=1       pass --with-deps to the browser install (needs root)
#   FFMPEG=/path/ffmpeg  ffmpeg binary for FLAC fixtures (default: the bundled
#                        ffmpeg-static devDependency — no system ffmpeg needed)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# --- 1. Wheel: reuse the one passed / present, else build it. --------------
WHEEL="${1:-}"
if [ -z "$WHEEL" ]; then
    existing=("$REPO_ROOT"/target/wheels/*.whl)
    if [ -f "${existing[0]}" ]; then
        WHEEL="${existing[0]}"
    else
        echo ">> no wheel found; building one" >&2
        WHEEL="$(bash "$REPO_ROOT/scripts/build-wheel.sh")"
    fi
fi
if [ ! -f "$WHEEL" ]; then
    echo "sync-harness: wheel not found: $WHEEL" >&2
    exit 1
fi

# --- 2. pip-install the wheel into a fresh venv. --------------------------
WORK="$(mktemp -d)"
VENV="$WORK/venv"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

echo ">> creating fresh venv at $VENV" >&2
python3 -m venv "$VENV" >/dev/null 2>&1 || python3 -m venv --without-pip "$VENV"
if ! "$VENV/bin/python" -m pip --version >/dev/null 2>&1; then
    "$VENV/bin/python" -m ensurepip --upgrade >/dev/null 2>&1 || {
        curl -sSfL https://bootstrap.pypa.io/get-pip.py -o "$WORK/get-pip.py"
        "$VENV/bin/python" "$WORK/get-pip.py" >/dev/null
    }
fi
echo ">> pip install $(basename "$WHEEL")" >&2
"$VENV/bin/python" -m pip install --no-index "$WHEEL" >&2

BIN="$VENV/bin/uncompose-compare"
if [ ! -x "$BIN" ]; then
    echo "sync-harness: entry point not installed at $BIN" >&2
    exit 1
fi

# --- 3. Harness deps + browsers + fixtures. ------------------------------
cd "$REPO_ROOT/harness"
if [ -z "${SKIP_NPM_INSTALL:-}" ]; then
    echo ">> installing harness npm deps" >&2
    npm install --no-audit --no-fund >&2
fi
if [ -z "${SKIP_PW_INSTALL:-}" ]; then
    echo ">> installing Playwright browsers" >&2
    if [ -n "${PW_WITH_DEPS:-}" ]; then
        npx playwright install --with-deps chromium firefox webkit >&2
    else
        npx playwright install chromium firefox webkit >&2
    fi
fi
echo ">> generating seeded-noise fixtures" >&2
node gen-fixtures.mjs >&2

# --- 4. Run the matrix against the installed binary. ---------------------
echo ">> running the three-engine sync matrix against $BIN" >&2
UNCOMPOSE_BIN="$BIN" npx playwright test

echo >&2
echo "=== sync harness: PASS ===" >&2
echo "the #73 matrix ran green against the pip-installed binary's served page." >&2
