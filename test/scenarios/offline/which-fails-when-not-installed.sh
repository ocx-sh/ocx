#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline package which fails when package was never installed
set -euo pipefail

# Do NOT install the published package. --offline package which must fail.
if ocx --offline package which "$PKG_HELLO"; then
    echo "expected --offline package which to fail for uninstalled package" >&2
    exit 1
fi
