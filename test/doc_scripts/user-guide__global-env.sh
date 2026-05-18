#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/global-env
# title: Inspect the global toolchain environment
# description: Emit the composed global toolchain environment in JSON, plain, and shell-eval forms.
set -euo pipefail

# region cast
ocx --global add "$PKG_CMAKE"
ocx --global env
ocx --global env --format plain
ocx --global env --shell=bash
# endregion cast

# Verification — outside the displayed region (drift-gated, never shown/cast).
json_out="$(ocx --global env)"
[[ -n "$json_out" ]] || {
    echo "ERROR: ocx --global env returned empty output" >&2
    exit 1
}

plain_out="$(ocx --global env --format plain)"
[[ -n "$plain_out" ]] || {
    echo "ERROR: ocx --global env --format plain returned empty output" >&2
    exit 1
}

shell_out="$(ocx --global env --shell=bash)"
[[ -n "$shell_out" ]] || {
    echo "ERROR: ocx --global env --shell=bash returned empty output" >&2
    exit 1
}
