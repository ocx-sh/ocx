#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: in-depth/shell-integration/cd-out-of-project
# title: Leaving a project's directory
# description: cd back out of a consented project and OCX no longer resolves a project from the current directory.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
eval "$(ocx self activate --shell=bash)"
ocx init
ocx add "$PKG_ASTRAL_SH_UV"
ocx shell state
cd ..
ocx shell state
# endregion cast
