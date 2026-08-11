#!/usr/bin/env bash
# Guided tour of what the `integrations` feature actually PRINTS.
#
# Every section is a banner plus the real command and its real output. The
# sections that state a NEGATIVE ("no namespace reaches the shell channel")
# assert it and exit non-zero when the assertion fails — a demo that only
# prints cannot tell a passing contract from a broken one.
#
# Prerequisites:
#   cd test && docker compose up -d
#   cargo build --release -p ocx
#   source test/manual/scripts/env.sh
#   test/manual/scripts/setup-integrations.sh
#
# Pass --no-managed to skip the managed-config section (the only section that
# writes to $OCX_HOME/config.toml; it restores the file on exit either way).
set -euo pipefail
IFS=$'\n\t'

# ── Pre-flight ────────────────────────────────────────────────────────────────

WITH_MANAGED=true
for arg in "$@"; do
    case "${arg}" in
        --no-managed) WITH_MANAGED=false ;;
        *)
            echo "unknown argument: ${arg}" >&2
            exit 64
            ;;
    esac
done

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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
MANUAL_ROOT="${REPO_ROOT}/test/manual"

if [[ -n "${OCX_BIN:-}" && -x "${OCX_BIN}" ]]; then
    : # caller-provided
elif [[ -x "${REPO_ROOT}/target/release/ocx" ]]; then
    OCX_BIN="${REPO_ROOT}/target/release/ocx"
elif [[ -x "${REPO_ROOT}/target/debug/ocx" ]]; then
    OCX_BIN="${REPO_ROOT}/target/debug/ocx"
else
    echo "error: no ocx binary found under ${REPO_ROOT}/target/{release,debug}/." >&2
    exit 1
fi
export OCX_BIN

# shellcheck source=./_lib.sh
# shellcheck disable=SC1091  # resolved at runtime via SCRIPT_DIR
source "${SCRIPT_DIR}/_lib.sh"

command -v jq >/dev/null || ocx_fail "jq is required for this tour"

ROOT_A="${NAMESPACE}/custom-tool:${TAG}"
ROOT_B="${NAMESPACE}/custom-other:${TAG}"

# Every namespace key declared anywhere in the rig. Used by the negative
# assertions: a channel that carries none of these carries no integrations.
# Anchored to the exact keys — a bare `vscode` would match the ambient PATH on
# any machine with VS Code installed, and pass or fail for the wrong reason.
NAMESPACE_PATTERN='com\.microsoft\.vscode|com\.jetbrains|sh\.ocx\.leaf|com\.example\.private'

banner() {
    printf '\n'
    ocx_step "════ $* ════"
}

# Run `ocx <args> | jq <filter>` and echo the WHOLE pipeline, filter included.
#
# The plain `ocx` wrapper echoes only `$ ocx <args>`, so a section that pipes
# its output through an unshown `jq` prints a shape the reader cannot
# reproduce — and if that filter constructs objects, a shape ocx never emits.
# Every filter passed here is therefore a SUBTREE selection or a row filter:
# it narrows what is shown, it never renames or rebuilds a key.
ocx_jq() {
    local filter="$1"
    shift
    local out="" arg
    for arg in "$@"; do
        out+=" $(ocx_quote "$arg")"
    done
    printf "%s$ ocx%s | jq '%s'%s\n" "$OCX_C_CMD" "$out" "$filter" "$OCX_C_RESET" >&2
    "${OCX_BIN:-ocx}" "$@" | jq "$filter"
}

# Assert the piped-in text carries NO namespace key. Args: channel description.
assert_no_namespace() {
    local text hits
    text="$(cat)"
    # `grep -c` exits 1 on no match, which is the PASSING case here — absorb it
    # so `set -e` does not abort before the assertion is evaluated.
    hits="$(printf '%s' "${text}" | grep -c -E "${NAMESPACE_PATTERN}" || true)"
    if [[ "${hits}" != "0" ]]; then
        printf '%s\n' "${text}" | grep -E "${NAMESPACE_PATTERN}" >&2 || true
        ocx_fail "$1 leaked an integration namespace (${hits} line(s)) — see above"
    fi
    ocx_done "OK — no namespace key reaches $1"
}

# ── 1. The JSON contract: the integrations array with attribution ──────────

banner "1. package env --format json — the integrations array"
cat <<EOF
Every row is one (package, namespace) pair with its interpolated payload.
Read the four rows below as the four axes of the feature:

  sh.ocx.leaf          declared by the INTERFACE dep    -> crosses to the consumer
  com.jetbrains        declared by the root             -> \${deps.leaf.installPath} resolved
  com.microsoft.vscode declared by the root             -> \${self.installPath} resolved
  com.microsoft.vscode declared by the patch COMPANION  -> same key, second row, NOT merged

com.example.private, declared by the PRIVATE dep, is absent — a private edge
never reaches the consumer's surface.
EOF
ocx_jq '.integrations' --format json package env "${ROOT_A}"

# ── 2. Plain output: the availability hint line ──────────────────────────────

banner "2. package env (plain) — the availability hint"
cat <<'EOF'
The entries table is byte-stable: integrations add no column and no second
table. Availability is one hint line under it, and it names NAMESPACES, so it
dedupes — the four rows above are three distinct namespaces here.
EOF
ocx package env "${ROOT_A}"

# ── 3. The --self surface carries zero integrations ────────────────────────

banner "3. package env --self --format json — must be []"
cat <<'EOF'
Interface surface only, at every depth. This is a SURFACE-level rule, not a
visibility one: no visibility value produces it, and `--self` composes zero
integrations even for the root's own declared namespaces.
EOF
self_rows="$(ocx_jq '.integrations' --format json package env --self "${ROOT_A}")"
printf '%s\n' "${self_rows}"
[[ "${self_rows}" == "[]" ]] || ocx_fail "--self must carry zero integrations, got: ${self_rows}"
ocx_done "OK — --self carries zero integrations"

# ── 4. The eval-safe and CI channels carry no namespace ──────────────────────

banner "4. package env --shell=bash / --ci=gitlab — no namespace, by design"
cat <<'EOF'
Both channels are env-only wire formats. An integrations payload is arbitrary
JSON with no env-var shape, so neither channel carries one; `--format json` is
the only path to the payload.
EOF
ocx package env --shell=bash "${ROOT_A}" | assert_no_namespace "--shell=bash"
ocx package env --ci=gitlab "${ROOT_A}" | assert_no_namespace "--ci=gitlab"

# ── 5. Two roots declaring one namespace: two rows, unmerged ─────────────────

banner "5. two roots, one namespace — two rows, never merged"
cat <<'EOF'
custom-tool and custom-other both declare `com.microsoft.vscode`. Where
devcontainer.json's closest analogue, customizations, merges, this one
concatenates. Three rows for one key below (both roots plus the companion)
— the consuming application adjudicates, not ocx.
EOF
ocx_jq '.integrations | map(select(.namespace == "com.microsoft.vscode"))' \
    --format json package env "${ROOT_A}" "${ROOT_B}"

# ── 6. The toolchain tier: ocx env from a project directory ──────────────────

banner "6. toolchain tier — ocx env from projects/integrations/"
cat <<'EOF'
Same envelope, keyed by the project's lock instead of raw identifiers. The
project `ocx.toml` binds custom-tool, custom-other and the patched base-tool.
EOF
(
    ocx_cd "${MANUAL_ROOT}/projects/integrations"
    ocx_jq '.integrations' --format json env
    printf '\n'
    ocx env
)

# ── 7. inspect --closure: declarations vs. what actually crosses ─────────────

banner "7. package inspect --closure --format json"
cat <<'EOF'
Two different things, deliberately:

  closure.deps[].integrations   the dep's OWN declared namespace keys,
                                  unfiltered — the private dep appears here.
  closure.surface.interface       the {namespace, package} rows that actually
                                  cross — the private dep is gone.
  closure.surface.private         [] — same surface rule as `--self`.

The closure envelope carries no `value`: a closure node is not installed, so
${installPath} has no concrete payload yet. The key is `namespace`, matching
the flat envelope — never `name`, which is reserved for PATH-resolving claims.
EOF
ocx_jq '.packages[].closure.deps' --format json package inspect --closure "${ROOT_A}"
printf '\n'
ocx_jq '.packages[].closure.surface.interface.integrations' \
    --format json package inspect --closure "${ROOT_A}"
printf '\n'
ocx_jq '.packages[].closure.surface.private.integrations' \
    --format json package inspect --closure "${ROOT_A}"

printf '\n'
ocx package inspect --closure "${ROOT_A}"

# ── 8. The patch tier: a companion contributes like a package ────────────────

banner "8. patch tier — the companion's row and its attribution"
cat <<'EOF'
`patches/corp-ca-bundle` is a companion, not a dependency: no package declares
it, a descriptor admits it. It contributes integrations exactly like a
package, attributed to its own identifier — no carrier-specific exception.
EOF
ocx_jq '.integrations | map(select(.package | contains("corp-ca-bundle")))' \
    --format json package env "${ROOT_A}"

banner "8b. the same composition with the patch tier opted out"
cat <<'EOF'
There is no `--no-patches` FLAG. The opt-out is the project-tier
`[package."<id>"] no-patches = true`, so this is a toolchain-tier A/B between
two sibling project directories.

Worth knowing before you read the diff: two descriptors admit this companion —
a base-specific one on `dojo/custom-tool` and setup-patches.sh's global
`match: "*"` one, which attaches it to EVERY admitted package. The
composition then dedupes the contribution by (package, namespace), so opting
out one base leaves the row in place via any other admitted package.
projects/integrations-no-patches therefore opts out all four admitted
packages, custom-leaf included.
EOF
(
    ocx_cd "${MANUAL_ROOT}/projects/integrations-no-patches"
    ocx_jq '.integrations' --format json env
)
opted_out="$(
    cd "${MANUAL_ROOT}/projects/integrations-no-patches" &&
        "${OCX_BIN}" --format json env 2>/dev/null |
        jq '[.integrations[] | select(.package | contains("corp-ca-bundle"))] | length'
)"
[[ "${opted_out}" == "0" ]] || ocx_fail "no-patches project still carries ${opted_out} companion row(s)"
ocx_done "OK — the companion's row is gone under the opt-out"

# ── 9. Managed config: the companion arrives by SITE POLICY ─────────────────

if [[ "${WITH_MANAGED}" == false ]]; then
    banner "9. managed config — SKIPPED (--no-managed)"
    exit 0
fi

banner "9. managed config — the patch tier by site policy, not local config"
cat <<'EOF'
The corporate story: an operator publishes a config.toml package carrying the
[patches] pointer, and a machine adopts it with `ocx config setup`. The local
config.toml then carries NO [patches] section at all — the companion, and its
integrations, arrive because site policy says so.

This section rewrites $OCX_HOME/config.toml and restores it on exit.
EOF

CONFIG_FILE="${OCX_HOME}/config.toml"
CONFIG_BACKUP="$(mktemp)"
cp "${CONFIG_FILE}" "${CONFIG_BACKUP}"
restore_config() {
    cp "${CONFIG_BACKUP}" "${CONFIG_FILE}"
    rm -f "${CONFIG_BACKUP}"
    ocx_step "restored ${CONFIG_FILE}"
}
trap restore_config EXIT

SITE_CONFIG="${MANUAL_ROOT}/managed-config/site-config.toml"
MANAGED_ID="${REGISTRY}/corp/ocx-config:1.0.0"
mkdir -p "$(dirname "${SITE_CONFIG}")"
cat >"${SITE_CONFIG}" <<EOF
# Published as ${MANAGED_ID} by show-integrations.sh.
# The whole of site policy: point every machine at the patch registry.

[patches]
registry = "${REGISTRY}"
required = true
EOF

ocx config push -n -i "${MANAGED_ID}" "${SITE_CONFIG}"

printf '# local config: NO [patches] section — the patch tier arrives by site policy.\n' \
    >"${CONFIG_FILE}"

ocx_step "before adopting: no [patches] anywhere — the companion must be absent"
# Assertion, not narrative: run the binary directly so no `$ ocx …` line is
# echoed for a command whose output the reader never sees.
before="$("${OCX_BIN}" --format json package env "${ROOT_A}" 2>/dev/null |
    jq '[.integrations[] | select(.package | contains("corp-ca-bundle"))] | length')"
printf 'companion rows: %s\n' "${before}"
[[ "${before}" == "0" ]] || ocx_fail "expected no companion row before adoption, got ${before}"

ocx config setup --managed-config "${MANAGED_ID}"
printf '\nresulting %s:\n' "${CONFIG_FILE}"
cat "${CONFIG_FILE}"

ocx_step "after adopting: the companion contributes, on site policy alone"
ocx_jq '.integrations | map(select(.package | contains("corp-ca-bundle")))' \
    --format json package env "${ROOT_A}"
after="$("${OCX_BIN}" --format json package env "${ROOT_A}" 2>/dev/null |
    jq '[.integrations[] | select(.package | contains("corp-ca-bundle"))] | length')"
[[ "${after}" == "1" ]] || ocx_fail "expected exactly one companion row after adoption, got ${after}"
ocx_done "OK — 0 rows before adoption, 1 after; the managed tier is what supplied the patch registry"

ocx config setup --managed-config ""

printf '\n'
ocx_done "tour complete."
