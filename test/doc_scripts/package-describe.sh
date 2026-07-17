#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Attaching package descriptions
# doc: authoring/package-describe
set -euo pipefail
cd "$SCENARIO_TMP"
# region cast
ocx package create build -m metadata.json -o mytool-1.0.0.tar.xz -p linux/amd64
ocx package push -n -i mytool:1.0.0 mytool-1.0.0.tar.xz
ocx package describe --readme README.md --title "mytool" --description "A small example tool" mytool
ocx package info mytool
# endregion cast
