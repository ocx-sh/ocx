#!/usr/bin/env bash
# Idempotently build + push every manual-testing package to the local
# `registry:2` from `test/docker-compose.yml`. Source `scripts/env.sh`
# before running so `$OCX_DEFAULT_REGISTRY`, `$OCX_INSECURE_REGISTRIES`
# and `$OCX_HOME` are pointed at the local rig.
#
# Each package is pushed under the namespace `dojo/<name>:1.0.0`.
# Templated metadata files (`metadata.in.json`) are rendered into
# `metadata.json` after their `@@…@@` placeholders are substituted with the
# fully-qualified `<fq>@<digest>` of an upstream dep.
set -euo pipefail

if [[ -z "${OCX_DEFAULT_REGISTRY:-}" ]]; then
    echo "error: source test/manual/scripts/env.sh first" >&2
    exit 1
fi
if [[ "${OCX_DEFAULT_REGISTRY}" != localhost:* ]]; then
    echo "error: this rig only targets localhost; OCX_DEFAULT_REGISTRY=${OCX_DEFAULT_REGISTRY}" >&2
    exit 1
fi

REGISTRY="${OCX_DEFAULT_REGISTRY}"
NAMESPACE="${OCX_MANUAL_NAMESPACE:-dojo}"
TAG="${OCX_MANUAL_TAG:-1.0.0}"
PLATFORM="${OCX_MANUAL_PLATFORM:-linux/amd64}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
MANUAL_ROOT="${REPO_ROOT}/test/manual"

# Run from test/manual/packages/<repo>/ for each package so echoed `ocx`
# calls look like the real-world workflow: `cd packages/foo`, then
# `ocx package create -m metadata.json -o out/foo-1.0.0.tar.xz build`.
# `out/` is gitignored.
cd "$MANUAL_ROOT"
PKG_ROOT="packages"

# Pull in the colored `ocx` wrapper + log helpers. Sets OCX_BIN to the
# release binary in test/bin/ so all `ocx` calls below print the exact
# invocation in dim grey before exec.
# shellcheck disable=SC2034  # consumed by ocx() in _lib.sh
OCX_BIN="${REPO_ROOT}/test/bin/ocx"
# shellcheck source=./_lib.sh
# shellcheck disable=SC1091  # _lib.sh resolved at runtime via SCRIPT_DIR
source "${SCRIPT_DIR}/_lib.sh"

# Short, registry-relative identifier for CLI use (`package push -i`,
# `index update`, `package install`, `package exec`). The CLI resolves these
# against the consumer's
# `OCX_DEFAULT_REGISTRY`.
id() { echo "${NAMESPACE}/${1}:${TAG}"; }
# Fully-qualified identifier — required only inside `metadata.json` dep
# `identifier` fields. ocx rejects short identifiers there with
# "identifier must include an explicit registry".
fq() { echo "${REGISTRY}/${NAMESPACE}/${1}:${TAG}"; }

# Extra `ocx package create` flags for the NEXT push_simple call only —
# reset to empty after each one. Used to opt a package out of the
# interface-binaries auto-scan (`--no-bin-scan`) so the rig also carries an
# `undeclared` node for `ocx package inspect --closure` (tri-state `binaries`).
CREATE_FLAGS=()

# Push a single-source-tree package. Args: repo, source-subdir (relative
# to the package root, e.g. "build"). Trailing args are KEY=value template
# substitutions for `metadata.in.json` (`@@KEY@@` → value).
#
# `create` writes a RESOLVED sidecar next to the bundle
# (`out/<repo>-<tag>-metadata.json`) carrying the auto-scanned `binaries`
# claim; `push` is deliberately called WITHOUT `-m` so it discovers that
# sidecar instead of the hand-authored `metadata.json`. Passing `-m
# metadata.json` here would publish metadata with no `binaries` field.
push_simple() {
    local repo="$1" src="$2"
    shift 2
    local bundle="out/${repo}-${TAG}.tar.xz"

    render_meta "$repo" "$@"
    ocx_step "${repo}: create + push"
    (
        ocx_cd "${PKG_ROOT}/${repo}"
        mkdir -p out
        ocx package create --force "${CREATE_FLAGS[@]}" -p "$PLATFORM" -m metadata.json -o "$bundle" "$src"
        ocx package push -n -c -p "$PLATFORM" -i "$(id "$repo")" "$bundle"
    )
    CREATE_FLAGS=()
    index_update "$repo"
}

# Refresh the local index entry for a just-pushed package. Required before a
# DEPENDENT package is created: `ocx package create -p` resolves unpinned
# dependencies through the index. Also what makes `ocx package which` work.
index_update() {
    ocx index update "${NAMESPACE}/${1}" >/dev/null
}

# Push a multi-layer package. Args: repo, layer-source-subdirs... (relative
# to the package root). No template substitutions supported on multi-layer
# packages today; add a `--` separator if that need ever arises.
push_multi_layer() {
    local repo="$1"
    shift
    local bundles=()
    local idx=0 layer_src
    render_meta "$repo"
    ocx_step "${repo}: create + push (${#@} layers)"
    (
        ocx_cd "${PKG_ROOT}/${repo}"
        mkdir -p out
        local sidecar=""
        for layer_src in "$@"; do
            local b="out/${repo}-${TAG}-layer${idx}.tar.gz"
            ocx package create --force -p "$PLATFORM" -m metadata.json -o "$b" "$layer_src"
            bundles+=("$b")
            sidecar="out/${repo}-${TAG}-layer${idx}-metadata.json"
            idx=$((idx + 1))
        done
        # Each `create` writes its own sidecar, so push cannot infer one —
        # name the LAST layer's explicitly. That layer ships `bin/`, so its
        # auto-scanned `binaries` claim is the meaningful one.
        ocx package push -n -c -p "$PLATFORM" -m "$sidecar" -i "$(id "$repo")" "${bundles[@]}"
    )
    index_update "$repo"
}

# Render `metadata.in.json` → `metadata.json` for a package by substituting
# `@@KEY@@` tokens. Every package is required to ship a `metadata.in.json`,
# even when it has no substitutions — the rendered `metadata.json` is the
# build artifact and is gitignored. Args: repo, [key1=value1 ...]
render_meta() {
    local repo="$1"
    shift
    local tmpl="${PKG_ROOT}/${repo}/metadata.in.json"
    local out="${PKG_ROOT}/${repo}/metadata.json"
    cp "$tmpl" "$out"
    for sub in "$@"; do
        local k="${sub%%=*}" v="${sub#*=}"
        # Use `|` as the sed delimiter — digests contain `:` and `/`.
        sed -i "s|@@${k}@@|${v}|g" "$out"
    done
}

# Materialize the (gitignored) per-package source trees before any
# `ocx package create`. Only metadata.in.json + multi-layer-app .so stubs
# are committed; the bin/ payloads are generated artifacts.
scaffold_payloads "$PKG_ROOT"

# ── 1. Leaf packages with no deps ─────────────────────────────────────────
push_simple single-layer-hello build
push_simple multi-entry-toolkit build
push_simple deps-leaf-a build
# leaf-b publishes NO `binaries` claim (scan skipped, field undeclared) so
# `inspect --closure` has a closure entry where the `binaries` key is absent —
# "not determined", distinct from an asserted-empty `[]`.
CREATE_FLAGS=(--no-bin-scan)
push_simple deps-leaf-b build

# ── 2. Multi-layer (no deps) ──────────────────────────────────────────────
push_multi_layer multi-layer-app layer-base layer-libs layer-app

# ── 3. Two-tier dep chain ─────────────────────────────────────────────────
# Deps are templated as plain `<registry>/<ns>/<repo>:<tag>` references;
# `ocx package create -p` resolves each one to a PLATFORM MANIFEST digest and
# writes the pins into the sidecar. Never hand-pin a tag's index digest — it
# is rewritten on every platform push and garbage-collected.
push_simple deps-mid build \
    "LEAF_A_FQ=$(fq deps-leaf-a)"

# ── 4. App with mixed-visibility deps ─────────────────────────────────────
# deps-app → leaf-a is a DIAMOND: also reachable through mid. The direct
# edge is `private`, the path through mid is `interface`, so leaf-a's
# effective visibility is the merge of both — and the closure renders it
# once, with `(*)` marking the repeat visit.
push_simple deps-app build \
    "MID_FQ=$(fq deps-mid)" \
    "LEAF_B_FQ=$(fq deps-leaf-b)" \
    "LEAF_A_FQ=$(fq deps-leaf-a)"

# ── 5. Entrypoint that targets a dep's binary via ${deps.NAME.installPath} ─
push_simple cross-layer-entrypoint build \
    "LEAF_A_FQ=$(fq deps-leaf-a)"

# ── 6. Baked args + ${installPath} interpolation demo ─────────────────────
# No @@...@@ substitutions; metadata.in.json used verbatim.
# content/ (committed) ships scripts/hello.sh; the entrypoint bakes its path.
push_simple baked-args-demo content

echo
ocx_done "bootstrap done. Try:"
echo "  ocx package exec ${NAMESPACE}/single-layer-hello:${TAG} -- hello"
echo "  ocx package exec ${NAMESPACE}/multi-entry-toolkit:${TAG} -- tool-a"
echo "  ocx package exec ${NAMESPACE}/deps-app:${TAG} -- app"
echo "  ocx package inspect --closure ${NAMESPACE}/deps-app:${TAG}    # closure, no install"
