#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/isolation
# title: Project vs OCI-tier isolation
# description: Demonstrate that ocx run reads only the project file and ocx package exec reads no project file at all.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_CMAKE"
ocx run -- cmake --version
ocx package exec "$PKG_CMAKE" -- cmake --version
# endregion cast
