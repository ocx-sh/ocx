#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: in-depth/shell-integration/adding-a-package
# title: Adding a tool self-consents the project
# description: ocx add is one of the six commands that write a consent stamp on first run; once written, the per-prompt hook applies the project's tools on your very next prompt.
set -euo pipefail

cd "$SCENARIO_TMP"

# region cast
eval "$(ocx self activate --shell=bash)"
ocx init
ocx shell state
ocx add "$PKG_ASTRAL_SH_UV"
ocx shell state
# endregion cast
