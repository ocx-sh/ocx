#!/usr/bin/env bash
# state: setup:full-catalog
# doc: user-guide/install-link
# title: Install a package at a path you choose
# description: Write the package link at a path of your own with install --link, so ocx clean keeps the package.
set -euo pipefail

ocx package install --link ./.tools/cmake "$PKG_KITWARE_CMAKE"
