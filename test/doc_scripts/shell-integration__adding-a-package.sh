#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: in-depth/shell-integration/adding-a-package
# title: Adding a tool applies it at the next prompt
# description: ocx add writes the project's ocx.toml and ocx.lock, and both are on the hook's watch set; the tool lands on PATH at the next prompt with no eval step of your own.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
mkdir demo
cd demo
ocx init
eval "$(ocx self activate --shell=bash)"
ocx shell state
ocx add "$PKG_ASTRAL_SH_UV"
ocx shell state
# endregion cast
