#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Publishing a package
# doc: authoring/package-push
set -euo pipefail
cd "$SCENARIO_TMP"
# region cast
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package push -n -p linux/amd64 -m metadata.json -i mytool:1.0.0 mytool-1.0.0.tar.xz
ocx index update mytool
ocx index list mytool
# endregion cast
