#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Bundling a package
# doc: authoring/package-create
set -euo pipefail
cd "$SCENARIO_TMP"
# region cast
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
# endregion cast
