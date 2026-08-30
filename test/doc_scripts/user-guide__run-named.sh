#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/run-named
# title: Run a specific named binding
# description: Pass a binding name to run only that tool from the composed scope.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add "$PKG_KITWARE_CMAKE"
# `ocx add` names the binding after the repository basename, so a two-segment
# identifier still binds as `cmake` — the binding name is what `ocx exec` takes.
ocx exec cmake -- cmake --version
# endregion cast
