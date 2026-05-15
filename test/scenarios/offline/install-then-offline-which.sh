#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline which succeeds after online install
set -euo pipefail

ocx install --select "$PKG_HELLO"
ocx --offline which "$PKG_HELLO"
