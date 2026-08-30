#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: lazy-loading/lifecycle
# title: Defer a tool's content until first use
# description: Compose two tools as shims with --lazy-mode always; only the one actually invoked materializes.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add --no-pull "$PKG_KITWARE_CMAKE" "$PKG_ASTRAL_SH_UV"
ocx env --lazy-mode always
ocx package which --lazy-mode always "$PKG_KITWARE_CMAKE" "$PKG_ASTRAL_SH_UV"
ocx exec --lazy-mode always -- cmake --version
ocx package which --lazy-mode always "$PKG_KITWARE_CMAKE" "$PKG_ASTRAL_SH_UV"
# endregion cast

# cmake was invoked, so its shim materialized into a real package. uv was
# composed but never invoked, so it is still a shim — proving the deferral
# is per-tool, not an all-or-nothing property of the compose.
cmake_kind="$(
    ocx --format json package which --lazy-mode always "$PKG_KITWARE_CMAKE" |
        grep -o '"kind":[[:space:]]*"[a-z]*"' | head -1
)"
[[ "$cmake_kind" == *package* ]] || {
    echo "ERROR: expected cmake to be materialized after invocation, got: $cmake_kind" >&2
    exit 1
}

uv_kind="$(
    ocx --format json package which --lazy-mode always "$PKG_ASTRAL_SH_UV" |
        grep -o '"kind":[[:space:]]*"[a-z]*"' | head -1
)"
[[ "$uv_kind" == *shim* ]] || {
    echo "ERROR: expected uv to still be a shim (never invoked), got: $uv_kind" >&2
    exit 1
}
