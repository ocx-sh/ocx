#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Publishing a multi-platform package
# doc: authoring/package-multi-platform
set -euo pipefail
cd "$SCENARIO_TMP"
# region cast
ocx package create build -i acme/mytool:1.0.0 -p linux/amd64 -m metadata.json -o .
ocx package create build -i acme/mytool:1.0.0 -p linux/arm64 -m metadata.json -o .
ocx package push -n -c -p linux/amd64 -i acme/mytool:1.0.0 mytool-1.0.0-linux-amd64.tar.xz
ocx package push -c -p linux/arm64 -i acme/mytool:1.0.0 mytool-1.0.0-linux-arm64.tar.xz
ocx index update acme/mytool
ocx index list acme/mytool --platforms
ocx package install acme/mytool:1.0.0
ocx package exec acme/mytool:1.0.0 -- mytool
# endregion cast
