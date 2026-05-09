#!/usr/bin/env bash
# scenario: BasicPackage
# title: --offline find fails when package was never installed
set -euo pipefail

# Do NOT install the published package. --offline find must fail.
if ocx --offline find "$PKG_HELLO"; then
    echo "expected --offline find to fail for uninstalled package" >&2
    exit 1
fi
