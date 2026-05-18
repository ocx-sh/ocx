#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/ci-oci
# title: CI with direct OCI identifiers
# description: Pull a package without symlinks and emit its resolved environment as JSON for CI consumption.
set -euo pipefail

# region cast
ocx package pull "$PKG_CMAKE"
ocx package env "$PKG_CMAKE"
# endregion cast

# Verification — outside the displayed region (drift-gated, never shown/cast).
env_out="$(ocx package env "$PKG_CMAKE")"
[[ -n "$env_out" ]] || {
    echo "ERROR: ocx package env returned empty output" >&2
    exit 1
}
