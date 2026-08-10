#!/bin/sh
# Tests for the two guards the release workflow puts in front of `publish`:
# `release-version.sh` (a tag that disagrees with the source version is refused)
# and `check-wheel.sh` (the built wheel is the one that tag names).
#
# Both guards are shell, so their test is shell too — a few seconds in CI, no
# scaffolding around GitHub Actions. Every case drives the real script as a
# process and asserts only its exit code and its message, the same seam the
# workflow observes.
#
# Usage: scripts/release-checks.test.sh
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failures=0

# Run a command, then assert its exit code and (optionally) that its combined
# output mentions a given string. Everything the tests know about a guard comes
# through here.
check() {
    description="$1"
    want_status="$2"
    want_text="$3"
    shift 3

    output="$("$@" 2>&1)" && status=0 || status=$?

    if [ "$status" -ne "$want_status" ]; then
        printf 'FAIL %s: exited %s, want %s\n%s\n' \
            "$description" "$status" "$want_status" "$output" >&2
        failures=$((failures + 1))
        return
    fi
    if [ -n "$want_text" ]; then
        case "$output" in
            *"$want_text"*) ;;
            *)
                printf 'FAIL %s: output does not mention %s\n%s\n' \
                    "$description" "$want_text" "$output" >&2
                failures=$((failures + 1))
                return
                ;;
        esac
    fi
    printf 'ok   %s\n' "$description"
}

# --- release-version.sh -----------------------------------------------------
#
# Fixture manifests, so a case knows the version independently of the parser
# under test. Both Cargo layouts the family uses are covered: a single package
# (this repo) and a workspace root that declares the version for its members
# (uncompose-project).

workspace_manifest="$work/workspace.toml"
cat > "$workspace_manifest" <<'TOML'
[workspace]
resolver = "2"
members = ["crates/core", "crates/cli"]

[workspace.package]
version = "1.2.3"
edition = "2021"

[workspace.lints.clippy]
all = "warn"
TOML

package_manifest="$work/package.toml"
cat > "$package_manifest" <<'TOML'
[package]
name = "uncompose-compare"
version = "4.5.6"
edition = "2021"

[dependencies]
clap = { version = "4", features = ["derive"] }
TOML

version_of() { "$here/release-version.sh" --manifest "$1" ${2+"$2"}; }

check "prints the workspace version with no tag to check" 0 "1.2.3" \
    version_of "$workspace_manifest"
check "prints a single package's version" 0 "4.5.6" \
    version_of "$package_manifest"

# A dependency's `version = "4"` must never be mistaken for the package's.
check "reads the version from the package section only" 0 "4.5.6" \
    version_of "$package_manifest"

check "accepts the tag that agrees with the source version" 0 "1.2.3" \
    version_of "$workspace_manifest" v1.2.3
check "accepts a full ref as pushed by git" 0 "1.2.3" \
    version_of "$workspace_manifest" refs/tags/v1.2.3

# The refusal this whole guard exists for.
check "refuses a tag that disagrees with the source version" 1 "1.2.4" \
    version_of "$workspace_manifest" v1.2.4
check "names the source version in the refusal" 1 "1.2.3" \
    version_of "$workspace_manifest" v1.2.4
check "refuses a version-shaped tag with no v" 1 "vX.Y.Z" \
    version_of "$workspace_manifest" 1.2.3
check "refuses a two-part tag" 1 "vX.Y.Z" \
    version_of "$workspace_manifest" v1.2
check "refuses a prerelease tag" 1 "vX.Y.Z" \
    version_of "$workspace_manifest" v1.2.3-rc.1
check "refuses a tag with trailing junk" 1 "vX.Y.Z" \
    version_of "$workspace_manifest" v1.2.3.dev1

check "fails when the manifest is missing" 1 "" \
    version_of "$work/absent.toml" v1.2.3

empty_manifest="$work/empty.toml"
printf '[workspace]\nmembers = []\n' > "$empty_manifest"
check "fails when the manifest declares no version" 1 "" \
    version_of "$empty_manifest"

# The default manifest is this repo's own, so the workflow can call the script
# with nothing but a tag.
check "defaults to the repo's Cargo.toml" 0 "" "$here/release-version.sh"

# --- check-wheel.sh ---------------------------------------------------------
#
# Wheels are named `{distribution}-{version}-{python}-{abi}-{platform}.whl`, so
# a name alone says whether the artifact about to be published is the one the
# tag names, built for the platform v0.1 supports. The fixtures are empty files:
# nothing here opens a wheel.

pyproject="$work/pyproject.toml"
cat > "$pyproject" <<'TOML'
[project]
name = "uncompose-compare"
requires-python = ">=3.8"
dynamic = ["version"]
TOML

wheels() {
    dir="$work/wheels"
    rm -rf "$dir"
    mkdir -p "$dir"
    for name in "$@"; do
        : > "$dir/$name"
    done
    printf '%s' "$dir"
}

check_wheel() { "$here/check-wheel.sh" --pyproject "$pyproject" "$1" "$2"; }

manylinux="uncompose_compare-1.2.3-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl"

check "accepts the manylinux wheel the tag names" 0 "" \
    check_wheel "$(wheels "$manylinux")" 1.2.3
check "refuses a wheel built from a different version" 1 "1.2.3" \
    check_wheel "$(wheels "uncompose_compare-1.2.4-py3-none-manylinux_2_17_x86_64.whl")" 1.2.3
check "refuses another project's wheel" 1 "uncompose_compare" \
    check_wheel "$(wheels "uncompose_project-1.2.3-py3-none-manylinux_2_17_x86_64.whl")" 1.2.3
check "refuses a wheel that is not manylinux" 1 "manylinux" \
    check_wheel "$(wheels "uncompose_compare-1.2.3-py3-none-linux_x86_64.whl")" 1.2.3
check "refuses an empty wheel directory" 1 "" \
    check_wheel "$(wheels)" 1.2.3
check "refuses more than one wheel" 1 "" \
    check_wheel "$(wheels "$manylinux" "uncompose_compare-1.2.3-py3-none-any.whl")" 1.2.3

# --- result -----------------------------------------------------------------

if [ "$failures" -ne 0 ]; then
    printf '\n%s check(s) failed\n' "$failures" >&2
    exit 1
fi
printf '\nall release checks passed\n'
