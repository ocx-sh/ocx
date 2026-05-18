#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/versions-select
# title: Install and select a version
# description: Install a specific version and set it as current with package select.
set -euo pipefail

ocx package install "$PKG_CMAKE"
ocx package select "$PKG_CMAKE"
