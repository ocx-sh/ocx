#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/install
# title: Install a tool
# description: Download a package into the content-addressed store and create a candidate symlink.
set -euo pipefail

ocx package install "$PKG_CMAKE"
