#!/usr/bin/env bash
# state: setup:basic
# doc: entry-points/example-install
# title: End-to-end example — install and select a tool
# description: Install a package with --select and print its resolved environment in eval-safe form.
set -euo pipefail

# region cast
ocx package install --select "$PKG_UV"

# Print the eval-safe env for the selected package.
# In a shell profile, this lets launchers declared in the package's metadata
# appear on $PATH.  The global toolchain form (eval "$(ocx --global env --shell=bash)")
# is used when the package is managed via ocx.toml.
ocx package env --shell=bash "$PKG_UV"
# endregion cast

# Verification — outside the displayed region (drift-gated, never shown/cast).
out="$(ocx package env --shell=bash "$PKG_UV")"
[[ -n "$out" ]] || {
    echo "expected non-empty env output" >&2
    exit 1
}
