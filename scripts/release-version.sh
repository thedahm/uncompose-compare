#!/bin/sh
# Resolve the version a release publishes, and refuse a tag that disagrees with it.
#
# Usage:
#   scripts/release-version.sh                    print the source version
#   scripts/release-version.sh <tag|ref>          print it, or fail if <tag> disagrees
#   scripts/release-version.sh --manifest <path>  read another Cargo.toml (tests)
#
# The version is declared in exactly one place: `[package] version` in
# Cargo.toml. `pyproject.toml` takes it from there (`dynamic = ["version"]`, see
# ADR-0011), so the wheel's version is the crate's by construction and there is
# nothing to keep in sync. A tag is only a *claim* about that number; this is
# where the claim is checked, before anything is built or published, so a
# mis-tagged release fails instead of publishing a lie (uncompose#97).
set -eu

manifest=""
tag=""
while [ $# -gt 0 ]; do
    case "$1" in
        --manifest)
            manifest="${2:?--manifest needs a path}"
            shift 2
            ;;
        -*)
            echo "release-version.sh: unknown option: $1" >&2
            exit 2
            ;;
        *)
            if [ -n "$tag" ]; then
                echo "release-version.sh: unexpected argument: $1" >&2
                exit 2
            fi
            tag="$1"
            shift
            ;;
    esac
done

if [ -z "$manifest" ]; then
    manifest="$(cd "$(dirname "$0")/.." && pwd)/Cargo.toml"
fi
if [ ! -f "$manifest" ]; then
    echo "release-version.sh: no Cargo manifest at $manifest" >&2
    exit 1
fi

# The first `version` in `[package]` or `[workspace.package]` — never a
# dependency's, which lives under a different table.
version="$(awk -F'"' '
    /^[[:space:]]*\[/ {
        owned = ($0 ~ /^[[:space:]]*\[(workspace\.)?package\][[:space:]]*$/)
        next
    }
    owned && /^[[:space:]]*version[[:space:]]*=/ { print $2; exit }
' "$manifest")"

if [ -z "$version" ]; then
    echo "release-version.sh: no [package] version in $manifest" >&2
    exit 1
fi

if [ -n "$tag" ]; then
    tag="${tag#refs/tags/}"

    # v0.1 releases are plain vX.Y.Z. Prerelease tags are deliberately not
    # accepted: Cargo's semver prereleases and PEP 440's do not spell the same
    # version the same way, so a `v0.1.0-rc.1` tag could not be compared to a
    # Cargo version honestly. Rehearsals go to TestPyPI from a manual run of the
    # release workflow instead (docs/releasing.md).
    if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
        echo "release-version.sh: '$tag' is not a release tag (expected vX.Y.Z)" >&2
        exit 1
    fi

    if [ "${tag#v}" != "$version" ]; then
        echo "release-version.sh: tag $tag disagrees with the source version $version." >&2
        echo "Set the version in Cargo.toml to ${tag#v}, or move the tag to v$version." >&2
        exit 1
    fi
fi

printf '%s\n' "$version"
