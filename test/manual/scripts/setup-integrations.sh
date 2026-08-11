#!/usr/bin/env bash
# Idempotent setup for the `integrations` manual exploration.
#
# Publishes four packages under `dojo/` that make every axis of the feature
# visible, re-runs the patches rig (whose `corp-ca-bundle` companion now
# declares integrations of its own), and locks two toolchain-tier projects:
#
#   dojo/custom-leaf:1.0.0     namespace `sh.ocx.leaf`        — interface dep
#   dojo/custom-private:1.0.0  namespace `com.example.private` — private dep
#   dojo/custom-tool:1.0.0     namespaces `com.microsoft.vscode` + `com.jetbrains`
#                              (root; `${self.installPath}` and `${deps.leaf.installPath}`)
#   dojo/custom-other:1.0.0    namespace `com.microsoft.vscode` — SAME key as
#                              custom-tool, to show the no-merge rule
#
# Prerequisites:
#   cd test && docker compose up -d        (registry on localhost:5000)
#   cargo build --release -p ocx
#   source test/manual/scripts/env.sh
#
# Read test/manual/INTEGRATIONS.md for the walkthrough; run
# scripts/show-integrations.sh for the guided output tour.
set -euo pipefail
IFS=$'\n\t'

# ── Pre-flight ────────────────────────────────────────────────────────────────

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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
MANUAL_ROOT="${REPO_ROOT}/test/manual"

# Resolve the ocx binary: explicit OCX_BIN wins, else release, else debug.
if [[ -n "${OCX_BIN:-}" && -x "${OCX_BIN}" ]]; then
    : # caller-provided
elif [[ -x "${REPO_ROOT}/target/release/ocx" ]]; then
    OCX_BIN="${REPO_ROOT}/target/release/ocx"
elif [[ -x "${REPO_ROOT}/target/debug/ocx" ]]; then
    OCX_BIN="${REPO_ROOT}/target/debug/ocx"
else
    echo "error: no ocx binary found under ${REPO_ROOT}/target/{release,debug}/." >&2
    echo "       build one first: cargo build --release -p ocx" >&2
    exit 1
fi
export OCX_BIN

# shellcheck source=./_lib.sh
# shellcheck disable=SC1091  # resolved at runtime via SCRIPT_DIR
source "${SCRIPT_DIR}/_lib.sh"

cd "${MANUAL_ROOT}"
PKG_ROOT="packages"

# Short (CLI) and fully-qualified (metadata dep field) identifiers.
id() { echo "${NAMESPACE}/${1}:${TAG}"; }
fq() { echo "${REGISTRY}/${NAMESPACE}/${1}:${TAG}"; }

# Render `metadata.in.json` → `metadata.json`, substituting `@@KEY@@` tokens.
# Same contract as bootstrap.sh's helper of the same name.
render_meta() {
    local repo="$1"
    shift
    cp "${PKG_ROOT}/${repo}/metadata.in.json" "${PKG_ROOT}/${repo}/metadata.json"
    local sub
    for sub in "$@"; do
        # `|` as the sed delimiter — digests contain `:` and `/`.
        sed -i "s|@@${sub%%=*}@@|${sub#*=}|g" "${PKG_ROOT}/${repo}/metadata.json"
    done
}

# Build + push one single-layer package, then refresh its index entry so a
# DEPENDENT package's `create` can resolve the unpinned dep reference.
# Args: repo, [KEY=value...] template substitutions.
push_simple() {
    local repo="$1"
    shift
    render_meta "$repo" "$@"
    ocx_step "${repo}: create + push"
    (
        ocx_cd "${PKG_ROOT}/${repo}"
        mkdir -p out
        ocx package create --force -p "$PLATFORM" -m metadata.json -o "out/${repo}-${TAG}.tar.xz" build
        # No `-m`: push picks up the create-emitted sidecar carrying the
        # resolved dep pins and the auto-scanned `binaries` claim.
        ocx package push -n -c -p "$PLATFORM" -i "$(id "$repo")" "out/${repo}-${TAG}.tar.xz"
    )
    ocx index update "${NAMESPACE}/${repo}" >/dev/null
}

# ── Step 1: payload trees ────────────────────────────────────────────────────
#
# Each entrypoint dispatches through the composed PATH to
# `${installPath}/bin/<name>`, so the script for entrypoint X is `build/bin/X`.

ocx_step "scaffolding payloads"
scaffold_bin "${PKG_ROOT}/custom-leaf/build" custom-leaf <<'EOF'
#!/usr/bin/env bash
echo "custom-leaf (CUSTOM_LEAF_HOME=${CUSTOM_LEAF_HOME:-unset})"
EOF
scaffold_bin "${PKG_ROOT}/custom-private/build" custom-private <<'EOF'
#!/usr/bin/env bash
echo "custom-private (CUSTOM_PRIVATE_HOME=${CUSTOM_PRIVATE_HOME:-unset})"
EOF
scaffold_bin "${PKG_ROOT}/custom-tool/build" custom-tool <<'EOF'
#!/usr/bin/env bash
echo "custom-tool (CUSTOM_TOOL_HOME=${CUSTOM_TOOL_HOME:-unset}) -> $(custom-leaf)"
EOF
scaffold_bin "${PKG_ROOT}/custom-other/build" custom-other <<'EOF'
#!/usr/bin/env bash
echo "custom-other (CUSTOM_OTHER_HOME=${CUSTOM_OTHER_HOME:-unset})"
EOF

# ── Step 2: publish, dependencies first ──────────────────────────────────────

push_simple custom-leaf
push_simple custom-private
push_simple custom-tool \
    "LEAF_FQ=$(fq custom-leaf)" \
    "PRIVATE_FQ=$(fq custom-private)"
push_simple custom-other

# ── Step 3: the patch tier ───────────────────────────────────────────────────
#
# setup-patches.sh is idempotent and owns the whole patch rig (companions,
# bases, descriptors, `[patches]` config). `corp-ca-bundle`'s committed
# metadata declares a `com.microsoft.vscode` block, so re-running it is what
# publishes the integrations-bearing companion — nothing is duplicated here.

ocx_step "delegating to setup-patches.sh for the patch tier"
"${SCRIPT_DIR}/setup-patches.sh"

# The companion is bound to `custom-tool` by a BASE-SPECIFIC descriptor, not
# by the `match: "*"` global one setup-patches.sh publishes. The global
# descriptor lives at the registry-wide singleton `<registry>/global:__ocx.patch`,
# so any other writer of the same registry — a concurrent `task test` run, a
# colleague's rig — silently replaces it, last-writer-wins, and this rig then
# shows someone else's companion. A base-specific descriptor is addressed by
# the base's own repository and cannot collide that way.
ocx_step "publishing the integrations companion descriptor to custom-tool's path"
ocx patch publish \
    --descriptor "${PKG_ROOT}/patches/descriptors/integrations-companion.json" \
    "${REGISTRY}/${NAMESPACE}/custom-tool:${TAG}"

# ── Step 4: install the roots + lock the toolchain projects ──────────────────

ocx_step "installing roots"
ocx package install --select "$(id custom-tool)" >/dev/null
ocx package install --select "$(id custom-other)" >/dev/null

# Refresh descriptors for every installed base. Install-time discovery only
# fires on a fresh install, so a re-run over an already-installed root would
# otherwise keep composing whatever descriptor it cached the first time.
#
# `-p` is REQUIRED here. A bare `patch sync` fans out over the concrete ship
# matrix, and every package in this rig is published for one platform only —
# the fan-out then fails the whole sync with "required companion install
# failed ... package not found" for the platforms that were never pushed.
ocx_step "syncing patch descriptors"
ocx patch sync -p "$PLATFORM" >/dev/null

PROJECTS_ROOT="${MANUAL_ROOT}/projects"
for project in integrations integrations-no-patches; do
    ocx_step "locking projects/${project}"
    (
        ocx_cd "${PROJECTS_ROOT}/${project}"
        ocx lock
    )
done

# ── Done ─────────────────────────────────────────────────────────────────────

echo
ocx_done "integrations setup complete."
echo
echo "OCX_HOME : ${OCX_HOME}"
echo
echo "Published:"
echo "  ${REGISTRY}/${NAMESPACE}/custom-leaf:${TAG}     ns sh.ocx.leaf         (interface dep of custom-tool)"
echo "  ${REGISTRY}/${NAMESPACE}/custom-private:${TAG}  ns com.example.private (private dep of custom-tool)"
echo "  ${REGISTRY}/${NAMESPACE}/custom-tool:${TAG}     ns com.microsoft.vscode + com.jetbrains"
echo "  ${REGISTRY}/${NAMESPACE}/custom-other:${TAG}    ns com.microsoft.vscode (same key as custom-tool)"
echo "  ${REGISTRY}/patches/corp-ca-bundle:${TAG}       ns com.microsoft.vscode (patch companion)"
echo
echo "Next:"
echo "  test/manual/scripts/show-integrations.sh    # the guided output tour"
echo "  read test/manual/INTEGRATIONS.md"
