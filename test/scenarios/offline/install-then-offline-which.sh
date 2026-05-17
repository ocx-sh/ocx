#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline package which succeeds after online install
set -euo pipefail

ocx package install --select "$PKG_HELLO"
ocx --offline package which "$PKG_HELLO"
