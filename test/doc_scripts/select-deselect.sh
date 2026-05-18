#!/usr/bin/env bash
# state: setup:multi-version
# cast: true
# title: Switching and removing the active version
# doc: getting-started/select-deselect
set -euo pipefail
# region cast
ocx package install "$PKG_CORRETTO"
ocx package install "$REPO_CORRETTO:25.0.0"
ocx package select "$PKG_CORRETTO"
ocx package which --current "$PKG_CORRETTO"
ocx package select "$REPO_CORRETTO:25.0.0"
ocx package which --current "$PKG_CORRETTO"
ocx package deselect "$PKG_CORRETTO"
# endregion cast
