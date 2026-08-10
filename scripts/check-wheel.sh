#!/bin/sh
# Check that the wheel about to be published is the one the tag names.
#
# Usage: scripts/check-wheel.sh [--pyproject <path>] <wheel-dir> <version>
#
# `scripts/release-version.sh` checks the tag against the *source*; this checks the
# built artifact, so however the build got its version, a wheel that does not
# carry the released one never reaches PyPI. It also refuses a wheel that is not
# manylinux — PyPI rejects bare `linux_x86_64`, and finding that out at the
# upload step would mean a tag with no release behind it.
#
# Wheel names are `{distribution}-{version}-{python}-{abi}-{platform}.whl`
# (PEP 427), which is all this needs; no wheel is opened.
set -eu

pyproject=""
while [ $# -gt 0 ]; do
    case "$1" in
        --pyproject)
            pyproject="${2:?--pyproject needs a path}"
            shift 2
            ;;
        -*)
            echo "check-wheel.sh: unknown option: $1" >&2
            exit 2
            ;;
        *) break ;;
    esac
done

wheel_dir="${1:?usage: check-wheel.sh [--pyproject <path>] <wheel-dir> <version>}"
version="${2:?usage: check-wheel.sh [--pyproject <path>] <wheel-dir> <version>}"

if [ -z "$pyproject" ]; then
    pyproject="$(cd "$(dirname "$0")/.." && pwd)/pyproject.toml"
fi
if [ ! -f "$pyproject" ]; then
    echo "check-wheel.sh: no pyproject.toml at $pyproject" >&2
    exit 1
fi

# The distribution name of record is the one pip resolves, so read it from
# pyproject rather than assuming it. `-` becomes `_` in a wheel name (PEP 427).
name="$(awk -F'"' '
    /^[[:space:]]*\[/ { owned = ($0 ~ /^[[:space:]]*\[project\][[:space:]]*$/); next }
    owned && /^[[:space:]]*name[[:space:]]*=/ { print $2; exit }
' "$pyproject")"
if [ -z "$name" ]; then
    echo "check-wheel.sh: no [project] name in $pyproject" >&2
    exit 1
fi
distribution="$(printf '%s' "$name" | tr '.-' '__')"

count=0
wheel=""
for candidate in "$wheel_dir"/*.whl; do
    [ -e "$candidate" ] || continue
    wheel="$candidate"
    count=$((count + 1))
done
if [ "$count" -ne 1 ]; then
    echo "check-wheel.sh: expected exactly one wheel in $wheel_dir, found $count" >&2
    exit 1
fi

base="$(basename "$wheel" .whl)"
wheel_distribution="${base%%-*}"
rest="${base#*-}"
wheel_version="${rest%%-*}"
platform="${base##*-}"

if [ "$wheel_distribution" != "$distribution" ]; then
    echo "check-wheel.sh: $base is not a $distribution wheel" >&2
    exit 1
fi
if [ "$wheel_version" != "$version" ]; then
    echo "check-wheel.sh: $base carries version $wheel_version, but this release is $version" >&2
    exit 1
fi
case "$platform" in
    *manylinux*) ;;
    *)
        echo "check-wheel.sh: $base is tagged $platform, not manylinux; PyPI would reject it" >&2
        exit 1
        ;;
esac

printf '%s: %s wheel for version %s, tagged %s\n' "$base" "$distribution" "$version" "$platform"
