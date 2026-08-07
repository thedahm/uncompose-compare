#!/usr/bin/env bash
#
# Build stage of the Track C packaging pipeline (spec #1, issue #3):
# Vite bundle -> rust-embed -> maturin wheel, from a clean checkout.
#
# Node and the Rust toolchain are needed here (build time only, #60); the wheel
# they produce carries neither. Prints the measured wheel size and end-to-end
# build time for the findings ticket.
#
# Usage: scripts/build-wheel.sh
# Output: a single wheel in target/wheels/, plus a report on stderr.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

start=$(date +%s)

# Stage 1: the frontend bundle. rust-embed embeds frontend/dist at compile time,
# and build.rs fails loudly if it is missing (story 6), so this must run first.
# All build chatter goes to stderr; stdout is reserved for the wheel path so the
# pipeline can capture it via command substitution.
echo ">> [1/2] building frontend bundle" >&2
npm --prefix frontend install --no-audit --no-fund >&2
npm --prefix frontend run build >&2

# Stage 2: the maturin wheel. Ships only the compiled binary (bindings = "bin"),
# which already embeds the bundle from stage 1.
echo ">> [2/2] building maturin wheel" >&2
rm -rf target/wheels
maturin build --release --out target/wheels >&2

end=$(date +%s)

wheels=("$REPO_ROOT"/target/wheels/*.whl)
WHEEL="${wheels[0]}"
size_bytes=$(stat -c%s "$WHEEL")
size_kib=$(( (size_bytes + 1023) / 1024 ))
elapsed=$(( end - start ))

{
    echo
    echo "=== packaging pipeline: build report ==="
    echo "wheel:            $(basename "$WHEEL")"
    echo "wheel size:       ${size_kib} KiB (${size_bytes} bytes)"
    echo "build time:       ${elapsed}s (frontend + wheel, from clean checkout)"
    echo "========================================"
} >&2

# stdout carries only the wheel path, so callers can pipe it to the next stage.
echo "$WHEEL"
