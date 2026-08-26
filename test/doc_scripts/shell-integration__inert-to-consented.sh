#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: in-depth/shell-integration/inert-to-consented
# title: A path grant activates a project before any lock exists
# description: A freshly scaffolded ocx.toml has no lock and no stamp yet; a path grant on the exact checkout activates it anyway.
set -euo pipefail

cd -P "$SCENARIO_TMP"

# region cast
eval "$(ocx self activate --shell=bash)"
ocx init
ocx shell state
export OCX_CONSENT_PATHS="$PWD"
ocx shell state
# endregion cast
