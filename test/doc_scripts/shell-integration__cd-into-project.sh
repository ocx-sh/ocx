#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: in-depth/shell-integration/cd-into-project
# title: Landing inside a consented project
# description: Standing outside any project, OCX resolves nothing; cd into a fresh one, scaffold it, and the next prompt recognizes it immediately.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
eval "$(ocx self activate --shell=bash)"
ocx shell state
mkdir -p project
cd project
ocx init
ocx add "$PKG_ASTRAL_SH_UV"
ocx shell state
# endregion cast
