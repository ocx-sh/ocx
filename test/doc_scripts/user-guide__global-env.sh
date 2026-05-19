#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/global-env
# title: Inspect the global toolchain environment
# description: Emit the composed global toolchain environment in plain, JSON, and shell-eval forms.
set -euo pipefail

# region cast
ocx --global add "$PKG_CMAKE"
ocx --global env
ocx --format json --global env
ocx --global env --shell=bash
# endregion cast

# Verification — outside the displayed region (drift-gated, never shown/cast).
plain_out="$(ocx --global env)"
[[ -n "$plain_out" ]] || {
    echo "ERROR: ocx --global env returned empty output" >&2
    exit 1
}

json_out="$(ocx --format json --global env)"
[[ -n "$json_out" ]] || {
    echo "ERROR: ocx --format json --global env returned empty output" >&2
    exit 1
}

shell_out="$(ocx --global env --shell=bash)"
[[ -n "$shell_out" ]] || {
    echo "ERROR: ocx --global env --shell=bash returned empty output" >&2
    exit 1
}
