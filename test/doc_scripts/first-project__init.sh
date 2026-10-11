#!/usr/bin/env bash
# state: setup:cmake-versions
# doc: first-project/init
# title: Create a project
# description: Create a project directory and an empty ocx.toml.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
mkdir my-project && cd my-project
ocx init
# endregion cast
