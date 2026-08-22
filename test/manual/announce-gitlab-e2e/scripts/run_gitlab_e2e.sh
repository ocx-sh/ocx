#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# GitLab announce E2E driver. Creates a throwaway index project, announces a
# package published to the local acceptance registry into it, asserts the
# contract, and deletes the project.
#
# See README.md for prerequisites and what this proves.
set -euo pipefail

GITLAB_HOST="${GITLAB_HOST:-gitlab.com}"
OCX_BIN="${OCX_BIN:-$(cd "$(dirname "$0")/../../../.." && pwd)/test/bin/ocx}"
REGISTRY="${REGISTRY:-localhost:5000}"
RUN_ID="$(date +%s)"
PROJECT="ocx-announce-e2e-${RUN_ID}"
PACKAGE_REPO="e2e-widget-${RUN_ID}"
NAMESPACE_PREFIX="acme"
PACKAGE="${NAMESPACE_PREFIX}/${PACKAGE_REPO}"

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
fail() {
    printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2
    exit 1
}
ok() { printf '\033[32m  ok: %s\033[0m\n' "$*"; }

command -v glab >/dev/null || fail "glab is not on PATH"
command -v jq >/dev/null || fail "jq is not on PATH"
[ -x "$OCX_BIN" ] || fail "no ocx binary at $OCX_BIN (set OCX_BIN)"

step "Checking the credential"
glab auth status --hostname "$GITLAB_HOST" >/dev/null 2>&1 ||
    fail "glab is not authenticated against $GITLAB_HOST — run: glab auth login --hostname $GITLAB_HOST"
# The token is read from glab's own store so it never appears on a command line.
TOKEN="$(glab config get token --host "$GITLAB_HOST")"
[ -n "$TOKEN" ] || fail "glab has no token for $GITLAB_HOST"
GITLAB_NAMESPACE="${GITLAB_NAMESPACE:-$(glab api user | jq -r .username)}"
[ -n "$GITLAB_NAMESPACE" ] && [ "$GITLAB_NAMESPACE" != "null" ] || fail "could not resolve the token's namespace"
ok "authenticated to $GITLAB_HOST as $GITLAB_NAMESPACE"

INDEX_PATH="${GITLAB_NAMESPACE}/${PROJECT}"
INDEX_COORDINATE="${GITLAB_HOST}/${INDEX_PATH}"
ENCODED_INDEX="$(printf '%s' "$INDEX_PATH" | jq -sRr @uri)"

cleanup() {
    if [ "${KEEP_PROJECT:-}" = "1" ]; then
        printf '\nKEEP_PROJECT=1 — leaving %s in place\n' "$INDEX_COORDINATE"
        return
    fi
    printf '\nDeleting %s\n' "$INDEX_COORDINATE"
    glab api --method DELETE "projects/${ENCODED_INDEX}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

step "Creating the throwaway index project"
glab api --method POST projects \
    -f "name=${PROJECT}" -f "path=${PROJECT}" -f visibility=private \
    -f initialize_with_readme=true >/dev/null
ok "created $INDEX_COORDINATE"

# The default branch is read from the API rather than assumed: an instance may
# be configured with something other than `main`, and every later assertion
# compares against it.
DEFAULT_BRANCH="$(glab api "projects/${ENCODED_INDEX}" | jq -r .default_branch)"
ok "default branch is $DEFAULT_BRANCH"

step "Publishing a package to the local registry"
export OCX_HOME="${TMPDIR:-/tmp}/ocx-gitlab-e2e-${RUN_ID}"
mkdir -p "$OCX_HOME"
# The one sanctioned SSRF escape hatch, scoped to this throwaway home.
cat >"$OCX_HOME/config.toml" <<EOF
[registries."${REGISTRY}"]
trusted_hosts = ["${REGISTRY%%:*}"]
EOF
WORK="$OCX_HOME/pkg"
mkdir -p "$WORK/bin"
printf '#!/bin/sh\necho e2e\n' >"$WORK/bin/widget"
chmod +x "$WORK/bin/widget"
"$OCX_BIN" package create "$WORK" -o "$OCX_HOME/widget.tar.zst" --force >/dev/null
"$OCX_BIN" package push -i "${REGISTRY}/${PACKAGE_REPO}:1.0.0" -n "$OCX_HOME/widget.tar.zst" >/dev/null
ok "pushed ${REGISTRY}/${PACKAGE_REPO}:1.0.0"

step "Seeding the claimed-but-empty index root"
ROOT_PATH="p/${PACKAGE}.json"
ROOT_JSON="$(jq -nc --arg name "ocx.sh/${PACKAGE}" --arg repo "oci://${REGISTRY}/${PACKAGE_REPO}" \
    '{name: $name, repository: $repo, tags: {}}')"
glab api --method POST "projects/${ENCODED_INDEX}/repository/files/$(printf '%s' "$ROOT_PATH" | jq -sRr @uri)" \
    -f "branch=${DEFAULT_BRANCH}" -f "content=${ROOT_JSON}" \
    -f "commit_message=seed ${PACKAGE} root" >/dev/null
ok "seeded $ROOT_PATH"

BASE_HEAD="$(glab api "projects/${ENCODED_INDEX}/repository/branches/${DEFAULT_BRANCH}" | jq -r .commit.id)"
BRANCH="indexbot-announce-${PACKAGE//\//-}"

announce() {
    OCX_ANNOUNCE_TOKEN="$TOKEN" "$OCX_BIN" --format json package announce \
        --forge gitlab --index-repo "$INDEX_COORDINATE" --package "$PACKAGE" "$@"
}

step "1. First announce opens a merge request"
REPORT="$(announce --tags 1.0.0)"
echo "$REPORT" | jq .
[ "$(jq -r .status <<<"$REPORT")" = "updated" ] || fail "expected status=updated"
MR_URL="$(jq -r .pull_request_url <<<"$REPORT")"
MR_NUMBER="$(jq -r .pull_request_number <<<"$REPORT")"
[ "$MR_URL" != "null" ] || fail "no merge request reported"
case "$MR_URL" in *"/-/merge_requests/"*) ;; *) fail "not a GitLab merge-request URL: $MR_URL" ;; esac
ok "merge request !$MR_NUMBER at $MR_URL"

step "2. The committed root carries the announced tag"
COMMITTED="$(glab api "projects/${ENCODED_INDEX}/repository/files/$(printf '%s' "$ROOT_PATH" | jq -sRr @uri)/raw?ref=${BRANCH}")"
jq -e '.tags["1.0.0"]' <<<"$COMMITTED" >/dev/null || fail "1.0.0 is not in the committed root"
ok "root on $BRANCH carries 1.0.0"

step "3. The branch is based on the upstream default-branch head"
FIRST_PARENT="$(glab api "projects/${ENCODED_INDEX}/repository/branches/${BRANCH}" | jq -r '.commit.parent_ids[0]')"
[ "$FIRST_PARENT" = "$BASE_HEAD" ] ||
    fail "branch parent $FIRST_PARENT is not the upstream head $BASE_HEAD"
ok "based on $BASE_HEAD"

step "4. An identical rerun changes nothing"
HEAD_BEFORE="$(glab api "projects/${ENCODED_INDEX}/repository/branches/${BRANCH}" | jq -r .commit.id)"
REPORT="$(announce --tags 1.0.0)"
[ "$(jq -r .status <<<"$REPORT")" = "unchanged" ] || fail "expected status=unchanged on a rerun"
HEAD_AFTER="$(glab api "projects/${ENCODED_INDEX}/repository/branches/${BRANCH}" | jq -r .commit.id)"
[ "$HEAD_BEFORE" = "$HEAD_AFTER" ] || fail "an unchanged run advanced the branch"
OPEN_MRS="$(glab api "projects/${ENCODED_INDEX}/merge_requests?state=opened&source_branch=${BRANCH}" | jq 'length')"
[ "$OPEN_MRS" = "1" ] || fail "expected exactly one open merge request, found $OPEN_MRS"
ok "unchanged, branch unmoved, still one merge request"

step "5. A second tag accumulates into the same merge request"
"$OCX_BIN" package push -i "${REGISTRY}/${PACKAGE_REPO}:2.0.0" -n "$OCX_HOME/widget.tar.zst" >/dev/null
REPORT="$(announce --tags 1.0.0,2.0.0)"
[ "$(jq -r .status <<<"$REPORT")" = "updated" ] || fail "expected status=updated for a new tag"
[ "$(jq -r .pull_request_number <<<"$REPORT")" = "$MR_NUMBER" ] ||
    fail "a second merge request was opened instead of reusing !$MR_NUMBER"
COMMITTED="$(glab api "projects/${ENCODED_INDEX}/repository/files/$(printf '%s' "$ROOT_PATH" | jq -sRr @uri)/raw?ref=${BRANCH}")"
jq -e '.tags["1.0.0"] and .tags["2.0.0"]' <<<"$COMMITTED" >/dev/null ||
    fail "both tags must survive in the branch"
ok "both tags in !$MR_NUMBER"

printf '\n\033[32mAll assertions passed against %s\033[0m\n' "$GITLAB_HOST"
