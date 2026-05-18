#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/global-add
# title: Add tools to the global toolchain
# description: Use the root --global flag to add packages to $OCX_HOME/ocx.toml.
set -euo pipefail

ocx --global add "$PKG_CMAKE"
