#!/usr/bin/env bash
# Clean-machine proof: build docker/clean-machine.Dockerfile and resolve
# `ocx package install <ns>/<pkg>` inside it, against the rendered index.ocx.sh,
# with the namespace configured index-kind. Uses a release-shaped ocx binary —
# never test/bin/ocx, whose __testing feature unlocks internal seams
# (Key Decision D-7).
#
# Usage: clean_install_check.sh
#
# Stages the ocx binary into the build context from OCX_BINARY (default
# target/release/ocx). It must be a dev-channel-equivalent build: index-kind
# resolution is not released yet, and ocx 0.4.3 rejects this image's config
# with `unknown field 'index', expected 'url'`.
#
# Consumes from env.sh: E2E_NAMESPACE, E2E_PACKAGE.

set -euo pipefail
IFS=$'\n\t'

# shellcheck source=./env.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

IMAGE_TAG="ocx-announce-e2e-clean:$RUN_ID"
LOG_FILE="$RESULTS_DIR/${RUN_ID}-clean_install.log"
OCX_BINARY="${OCX_BINARY:-$E2E_REPO_ROOT/target/release/ocx}"
BUILD_CONTEXT="$(mktemp -d)"

# The image and the staged context are per-run scratch; leaving them behind
# would accumulate one layer stack and one 40MB binary copy per rehearsal.
cleanup() {
    rm -rf "$BUILD_CONTEXT"
    docker image rm -f "$IMAGE_TAG" >/dev/null 2>&1 || true
}
trap cleanup EXIT

main() {
    local secret_args=()
    local build_args=(
        --build-arg "E2E_NAMESPACE=$E2E_NAMESPACE"
        --build-arg "E2E_PACKAGE=$E2E_PACKAGE"
        --build-arg "INDEX_SITE=$INDEX_SITE"
        --build-arg "E2E_INDEX_PREFIX=$E2E_INDEX_PREFIX"
    )
    mapfile -t secret_args < <(evidence_secret_args)

    [[ -x $OCX_BINARY ]] ||
        ocx_fail "no ocx binary at $OCX_BINARY — build one (cargo build --release -p ocx) or set OCX_BINARY"
    install -m 0755 "$OCX_BINARY" "$BUILD_CONTEXT/ocx"
    cp "$E2E_SCRIPT_DIR/../docker/clean-machine.Dockerfile" "$BUILD_CONTEXT/"

    ocx_step "building $IMAGE_TAG from $OCX_BINARY ($("$OCX_BINARY" version))"
    docker build "${build_args[@]}" \
        -f "$BUILD_CONTEXT/clean-machine.Dockerfile" \
        -t "$IMAGE_TAG" "$BUILD_CONTEXT"

    # The container log lands in a committed-adjacent directory, so it goes
    # through the same redaction every other captured artifact does — a
    # registry auth error can echo a credential back.
    ocx_step "resolving $E2E_NAMESPACE/$E2E_PACKAGE on a machine that has never seen ocx"
    if ! docker run --rm "$IMAGE_TAG" 2>&1 |
        evidence redact "${secret_args[@]}" | tee "$LOG_FILE"; then
        ocx_warn "container config dump follows for debugging:"
        docker run --rm --entrypoint sh "$IMAGE_TAG" -c 'cat ~/.ocx/config.toml' >&2 || true
        ocx_fail "clean-machine install failed; full log at $LOG_FILE"
    fi

    grep -q "$E2E_PACKAGE" "$LOG_FILE" ||
        ocx_fail "install output never named $E2E_PACKAGE; full log at $LOG_FILE"

    evidence record --scenario clean_install --status pass \
        --notes "resolved $E2E_NAMESPACE/$E2E_PACKAGE from $INDEX_SITE in a clean debian container" \
        "${secret_args[@]}" \
        --out "$RESULTS_DIR/${RUN_ID}-clean_install.record.json"

    ocx_done "clean-machine install resolved $E2E_NAMESPACE/$E2E_PACKAGE"
}

main "$@"
