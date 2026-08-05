#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Publishing a package
# doc: authoring/package-push
set -euo pipefail
cd "$SCENARIO_TMP"
# region cast
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz -p linux/amd64 -i mytool:1.0.0
ocx package push -n mytool-1.0.0.tar.xz
ocx index update mytool
ocx index list mytool
# endregion cast
