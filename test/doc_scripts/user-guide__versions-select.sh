#!/usr/bin/env bash
# state: setup:node-versions
# doc: user-guide/versions-select
# title: Install and select a version
# description: Install a specific version and set it as current with package select.
set -euo pipefail

ocx package install "$PKG_NODEJS_NODE"
ocx package select "$PKG_NODEJS_NODE"
