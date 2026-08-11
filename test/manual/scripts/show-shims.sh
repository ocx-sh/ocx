#!/usr/bin/env bash
# Guided tour of lazy package loading — what a SHIM actually is on disk, when
# it materializes, and what survives each add/remove step.
#
# Every section is a banner plus the real command and its real output. Sections
# that state a NEGATIVE ("no package directory exists yet", "no candidate
# symlink is created") assert it and exit non-zero when the assertion fails — a
# demo that only prints cannot tell a passing contract from a broken one.
#
# Prerequisites:
#   cd test && docker compose up -d
#   cargo build --release -p ocx
#   source test/manual/scripts/env.sh
#   test/manual/scripts/bootstrap.sh
#
# The tour runs in its own throwaway OCX_HOME (a mktemp dir), NOT the one
# env.sh exports: it calls `ocx clean --force`, which would collect whatever
# else you had staged there. Pass --keep to leave that directory behind for
# poking at afterwards.
set -euo pipefail
IFS=$'\n\t'

# ── Pre-flight ────────────────────────────────────────────────────────────────

KEEP=false
for arg in "$@"; do
    case "${arg}" in
        --keep) KEEP=true ;;
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

NAMESPACE="${OCX_MANUAL_NAMESPACE:-dojo}"
TAG="${OCX_MANUAL_TAG:-1.0.0}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

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

# A generated shim re-enters ocx as `${OCX_BINARY_PIN:-ocx}`. Without the pin it
# resolves `ocx` off PATH — which on a developer machine is usually the RELEASED
# ocx, and a released build predating this feature answers `unrecognized
# subcommand 'shim'` (exit 64). Pin it to the binary under test so the tour
# exercises this tree rather than whatever is installed.
export OCX_BINARY_PIN="${OCX_BIN}"

# shellcheck source=./_lib.sh
# shellcheck disable=SC1091  # resolved at runtime via SCRIPT_DIR
source "${SCRIPT_DIR}/_lib.sh"

command -v jq >/dev/null || ocx_fail "jq is required for this tour"

OCX_HOME="$(mktemp -d "${TMPDIR:-/tmp}/ocx-shim-tour-XXXXXX")"
export OCX_HOME

cleanup() {
    if [[ "${KEEP}" == true ]]; then
        printf 'kept: %s\n' "${OCX_HOME}" >&2
    else
        rm -rf -- "${OCX_HOME}"
    fi
}
trap cleanup EXIT

TOOLKIT="${NAMESPACE}/multi-entry-toolkit:${TAG}"
HELLO="${NAMESPACE}/single-layer-hello:${TAG}"
APP="${NAMESPACE}/deps-app:${TAG}"

banner() {
    printf '\n%s══ %s%s\n' "${OCX_C_STEP}" "$*" "${OCX_C_RESET}"
}

# Absolute path of the shim tree ocx generated for <short-id>, derived from the
# PATH entry the composer emitted rather than rebuilt from a digest by hand —
# an independently reconstructed path can agree with a stale layout and pass.
shim_root_of() {
    ocx --format json package env --lazy-mode always "$1" |
        jq -r --arg home "${OCX_HOME}" '
            .entries[] | select(.key == "PATH") | .value
            | select(startswith($home + "/shims/"))' |
        head -1 | sed 's:/bin$::'
}

# ── §1 · A deferred compose downloads nothing ─────────────────────────────────

banner "§1 · compose with --lazy-mode always: names on PATH, no content"

ocx package env --lazy-mode always "${TOOLKIT}" || ocx_fail "deferred compose failed"

toolkit_shim="$(shim_root_of "${TOOLKIT}")"
[[ -n "${toolkit_shim}" ]] || ocx_fail "no shims/ entry on the composed PATH"

ocx_step "the shim tree ocx wrote"
find "${toolkit_shim}" | sed "s:${OCX_HOME}:\$OCX_HOME:"

ocx_step "assert: no package content directory exists yet"
if [[ -d "${OCX_HOME}/packages" ]]; then
    ocx_fail "packages/ exists after a deferred compose — content was materialized"
fi
ocx_done "packages/ is absent — only metadata was fetched"

ocx_step "assert: every declared name has a launcher"
for name in tool-a tool-b tool-c tool-d; do
    [[ -x "${toolkit_shim}/bin/${name}" ]] || ocx_fail "no launcher for ${name}"
done
ocx_done "4/4 declared names are on PATH before a single content byte exists"

ocx_step "what one launcher contains"
cat "${toolkit_shim}/bin/tool-a"

# ── §2 · First invocation materializes ────────────────────────────────────────

banner "§2 · first use downloads; second use is quiet"

ocx_step "run tool-a through the shim directory"
PATH="${toolkit_shim}/bin:${PATH}" tool-a || ocx_fail "shim invocation failed"

ocx_step "assert: content is now materialized"
[[ -d "${OCX_HOME}/packages" ]] || ocx_fail "packages/ still absent after first use"
ocx_done "packages/ now exists"

ocx_step "assert: the shim tree SURVIVES materialization"
[[ -x "${toolkit_shim}/bin/tool-b" ]] || ocx_fail "shim tree vanished after materialization"
ocx_done "still there — the next compose emits the same slot"

ocx_step "second invocation: no download line"
PATH="${toolkit_shim}/bin:${PATH}" tool-b || ocx_fail "second shim invocation failed"

ocx_step "assert: materializing created NO candidate symlink"
if [[ -d "${OCX_HOME}/symlinks" ]] && [[ -n "$(find "${OCX_HOME}/symlinks" -type l -print -quit)" ]]; then
    ocx_fail "a candidate symlink was created; first use must not install"
fi
ocx_done "symlinks/ holds nothing — first use is not an install"

# ── §3 · A dependency closure is ONE shim tree ────────────────────────────────

banner "§3 · a closure defers as one tree, not one per package"

ocx package env --lazy-mode always "${APP}" || ocx_fail "deferred compose of deps-app failed"
app_shim="$(shim_root_of "${APP}")"
[[ -n "${app_shim}" ]] || ocx_fail "no shims/ entry for deps-app"

ocx_step "launchers in the ROOT's shim tree"
ls -1 "${app_shim}/bin"

ocx_step "assert: the interface dep's name is here, the private dep's is not"
[[ -x "${app_shim}/bin/app" ]] || ocx_fail "root's own name missing"
[[ -x "${app_shim}/bin/mid" ]] || ocx_fail "interface dep 'mid' missing"
[[ -x "${app_shim}/bin/leaf-a" ]] || ocx_fail "transitive interface dep 'leaf-a' missing"
[[ ! -e "${app_shim}/bin/leaf-b" ]] || ocx_fail "private dep 'leaf-b' leaked onto the shim surface"
ocx_done "the shim surface equals the interface surface — same algebra as \`ocx env\`"

ocx_step "assert: no separate shim tree per dependency"
tree_count="$(find "${OCX_HOME}/shims" -name bin -type d | wc -l)"
[[ "${tree_count}" -eq 2 ]] || ocx_fail "expected 2 shim trees (toolkit + deps-app), found ${tree_count}"
ocx_done "2 trees for 2 composed roots — deps do not get their own"

# ── §4 · Advisories are warnings, never decisions ─────────────────────────────

banner "§4 · advisories: what a deferred compose warns about"

ocx_step "single-layer-hello roots HELLO_HOME at \${installPath}"
ocx --format json package env --lazy-mode always "${HELLO}" | jq '.advisories'

ocx_step "assert: the advisory did not block the compose"
hello_shim="$(shim_root_of "${HELLO}")"
[[ -x "${hello_shim}/bin/hello" ]] || ocx_fail "advisory suppressed the shim"
ocx_done "composed anyway — an advisory is diagnostic, nothing steers on it"

ocx_step "assert: an eager compose reports no advisories (nothing is deferred)"
eager="$(ocx --format json package env --lazy-mode never "${HELLO}" | jq -c '.advisories // []')"
[[ "${eager}" == "[]" || "${eager}" == "null" ]] || ocx_fail "eager compose reported advisories: ${eager}"
ocx_done "empty — an eagerly composed tool has nothing to defer"

# ── §5 · Refusals ─────────────────────────────────────────────────────────────

banner "§5 · what lazy loading refuses"

ocx_step "--self selects the private view; a shim is a consumer-facing launcher"
set +e
ocx package env --self --lazy-mode always "${TOOLKIT}" >/dev/null 2>"${OCX_HOME}/self.err"
rc=$?
set -e
[[ "${rc}" -eq 64 ]] || ocx_fail "--self × always exited ${rc}, expected 64"
cat "${OCX_HOME}/self.err"
ocx_done "exit 64 with a reason, not a stack trace"

# ── §6 · add / remove / re-add ────────────────────────────────────────────────

banner "§6 · uninstall, clean, recompose"

ocx_step "uninstall a tool that only ever materialized through its shim"
ocx package uninstall "${TOOLKIT}" || ocx_fail "uninstall failed"
ocx_done "reported 'absent' and exited 0 — there was no candidate to remove"

ocx_step "clean --force collects shim trees too"
ocx clean --force

ocx_step "assert: the shim tree is gone"
[[ ! -e "${toolkit_shim}/bin" ]] || ocx_fail "clean --force left the shim tree behind"
ocx_done "collected"

ocx_step "recompose regenerates it"
ocx package env --lazy-mode always "${TOOLKIT}" >/dev/null || ocx_fail "recompose failed"

ocx_step "assert: same path, same launchers"
[[ -x "${toolkit_shim}/bin/tool-a" ]] || ocx_fail "recompose did not regenerate at the same path"
ls -1 "${toolkit_shim}/bin"
ocx_done "identical path — the tree is derived from the pinned digest, not from state"

ocx_step "assert: it works after regeneration"
PATH="${toolkit_shim}/bin:${PATH}" tool-c || ocx_fail "regenerated shim does not run"

banner "done"
ocx_done "every section above asserted its own contract; a silent run is a passing run"
