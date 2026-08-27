#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: in-depth/shell-integration/cd-into-project
# title: Landing inside a consented project
# description: A shell that started outside the project resolves nothing; cd into it and the next prompt applies its tools.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
mkdir demo
cd demo
ocx init
ocx add "$PKG_ASTRAL_SH_UV"
cd ..
eval "$(ocx self activate --shell=bash)"
ocx shell state
cd demo
ocx shell state
# endregion cast
