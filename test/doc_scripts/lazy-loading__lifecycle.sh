#!/usr/bin/env bash
# state: setup:full-catalog
# cast: true
# doc: lazy-loading/lifecycle
# title: Defer a package's content until first use
# description: Compose two packages as shims with --lazy-mode always; only the one actually invoked materializes.
set -euo pipefail

cd "$SCENARIO_TMP"
# region cast
ocx init
ocx add --no-pull "$PKG_NODEJS_NODE" "$PKG_ASTRAL_SH_UV"
ocx env --lazy-mode always
ocx package which --lazy-mode always "$PKG_NODEJS_NODE" "$PKG_ASTRAL_SH_UV"
ocx exec --lazy-mode always -- node --version
ocx package which --lazy-mode always "$PKG_NODEJS_NODE" "$PKG_ASTRAL_SH_UV"
# endregion cast
# The runner ships its own node: a composed environment that lost the package would let it answer and still exit 0.
[[ "$(ocx exec --lazy-mode always -- node)" == marker-* ]] || {
    echo "node did not resolve to the provisioned package" >&2
    exit 1
}

# node was invoked, so its shim materialized into a real package. uv was
# composed but never invoked, so it is still a shim — proving the deferral
# is per-package, not an all-or-nothing property of the compose.
node_which=$(ocx --format json package which --lazy-mode always "$PKG_NODEJS_NODE")
node_kind=$(grep -o -m1 '"kind":[[:space:]]*"[a-z]*"' <<<"$node_which")
[[ "$node_kind" == *package* ]] || {
    echo "ERROR: expected node to be materialized after invocation, got: $node_kind" >&2
    exit 1
}

uv_which=$(ocx --format json package which --lazy-mode always "$PKG_ASTRAL_SH_UV")
uv_kind=$(grep -o -m1 '"kind":[[:space:]]*"[a-z]*"' <<<"$uv_which")
[[ "$uv_kind" == *shim* ]] || {
    echo "ERROR: expected uv to still be a shim (never invoked), got: $uv_kind" >&2
    exit 1
}
