#!/usr/bin/env bash
#
# The whole Track C packaging pipeline in one command (spec #1, issue #3,
# story 5): build the wheel from a clean checkout, then prove it pip-installs
# and serves the embedded page in a toolchain-free venv.
#
# CI runs exactly this; run it locally to reproduce CI without pushing.
#
# Usage: scripts/pipeline.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

WHEEL="$("$REPO_ROOT/scripts/build-wheel.sh")"
"$REPO_ROOT/scripts/clean-install-proof.sh" "$WHEEL"

echo >&2
echo ">> packaging pipeline complete: build + clean-install proof both green." >&2
