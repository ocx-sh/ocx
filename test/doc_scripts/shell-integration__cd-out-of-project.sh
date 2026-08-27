#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: in-depth/shell-integration/cd-out-of-project
# title: Leaving a project takes its tools back off PATH
# description: cd out of a consented project and the next prompt reverts every entry it applied, restoring the environment the shell had before.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
mkdir demo
cd demo
ocx init
ocx add "$PKG_ASTRAL_SH_UV"
eval "$(ocx self activate --shell=bash)"
ocx shell state
cd ..
ocx shell state
# endregion cast
