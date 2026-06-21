#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# title: Running packages with patch overlays
# doc: user-guide/patches-consumer
# description: Configure the [patches] tier via OCX_CONFIG, run ocx patch sync to install companions, and inspect the composed environment with --show-patches.
set -euo pipefail

# Write a [patches] config that points at the test registry.
# In production, site administrators place this in /etc/ocx/config.toml or
# the user config file at $XDG_CONFIG_HOME/ocx/config.toml.
PATCHES_CONFIG="$OCX_HOME/config.toml"
cat >"$PATCHES_CONFIG" <<'TOML'
[patches]
registry = "localhost:5000/site-patches"
path = "{registry}/{repository}"
required = false
TOML
export OCX_CONFIG="$PATCHES_CONFIG"

# region cast
ocx --global patch sync

ocx package env "$PKG_CMAKE" --show-patches

ocx package exec "$PKG_CMAKE" -- cmake --version
# endregion cast

# Verification — outside the displayed region.
cmake_out="$(ocx package exec "$PKG_CMAKE" -- cmake --version)"
[[ -n "$cmake_out" ]] || {
    echo "ERROR: cmake --version returned empty output" >&2
    exit 1
}
