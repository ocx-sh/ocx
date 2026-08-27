#!/usr/bin/env bash
# state: setup:basic
# cast: true
# doc: in-depth/shell-integration/inert-to-consented
# title: A path grant activates a checkout that carries no stamp
# description: A consent stamp is keyed on the directory, so a checkout that arrived by any route other than an ocx command has the lock and none of the consent; a path grant on that exact directory is what makes it stop being inert.
set -euo pipefail

cd -P "$SCENARIO_TMP"

# region cast
mkdir project
cd project
ocx init
ocx add "$PKG_ASTRAL_SH_UV"
cd ..
mv project clone
export OCX_CONSENT_PATHS="$PWD/clone"
eval "$(ocx self activate --shell=bash)"
cd clone
ocx shell state
# endregion cast
