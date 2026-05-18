#!/usr/bin/env bash
# state: setup:publisher
# cast: true
# title: Reusing a layer across packages
# doc: authoring/package-layer-reuse
set -euo pipefail
cd "$SCENARIO_TMP"
# region cast
ocx package create base -m metadata.json -o base.tar.xz
ocx package create build -m metadata.json -o tool-v1.tar.xz
ocx package push -n -p linux/amd64 -m metadata.json -i mytool:1.0.0 base.tar.xz tool-v1.tar.xz
BASE_DIGEST=$(sha256sum base.tar.xz | awk '{print $1}')
ocx package create build-v2 -m metadata.json -o tool-v2.tar.xz
ocx package push -p linux/amd64 -m metadata.json -i mytool:1.0.1 "sha256:${BASE_DIGEST}.tar.xz" tool-v2.tar.xz
# endregion cast
