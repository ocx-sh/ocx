#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Cascading rolling tags
# doc: authoring/package-cascade
set -euo pipefail
cd "$SCENARIO_TMP"
# region cast
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz
ocx package push -n -c -p linux/amd64 -m metadata.json -i mytool:1.0.0 mytool-1.0.0.tar.xz
ocx package create build-v2 -m metadata.json -o mytool-1.0.1.tar.xz
ocx package push -c -p linux/amd64 -m metadata.json -i mytool:1.0.1 mytool-1.0.1.tar.xz
ocx index update mytool
ocx index list mytool
# endregion cast
