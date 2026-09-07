#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: in-depth/shell-integration/toolchain-state
# title: Reading the toolchain state
# description: ocx shell state names the resolved toolchain home and the effective activate and pinned settings, past the whole ladder.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
mkdir demo
cd demo
ocx init
ocx add "$PKG_KITWARE_CMAKE"
ocx pull
eval "$(ocx self activate --shell=bash)"
ocx shell state
# endregion cast
