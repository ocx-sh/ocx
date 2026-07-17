#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Test a package locally before pushing
# doc: authoring/package-test
set -euo pipefail
cd "$SCENARIO_TMP"
# region cast
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz -p linux/amd64
ocx package test -i mytool:1.0.0 mytool-1.0.0.tar.xz -- mytool
ocx package test --keep -i mytool:1.0.0 mytool-1.0.0.tar.xz -- mytool
ocx package push -n -i mytool:1.0.0 mytool-1.0.0.tar.xz
# endregion cast
