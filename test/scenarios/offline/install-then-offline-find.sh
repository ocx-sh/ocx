#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline find succeeds after online install
set -euo pipefail

ocx install --select "$PKG_HELLO"
ocx --offline find "$PKG_HELLO"
