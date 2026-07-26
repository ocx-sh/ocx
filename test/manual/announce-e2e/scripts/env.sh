#!/usr/bin/env bash
# Shared configuration and helpers for the Track D announce E2E drivers.
# Sourced (never executed) by run_sequence.sh, run_idempotency.sh,
# run_update_union.sh, run_machine_lane.sh, clean_install_check.sh.
#
# Required environment (no defaults — the pilot namespace/package is Track C's
# choice, made at execution time, and a wrong default would announce into the
# real index):
#   GH_REPO_PUBLISHER   michael-herwig/ocx-e2e-publisher
#   GH_REPO_INDEX       ocx-sh/index
#   INDEX_FORK          <owner>/index — the fork announce opens its PR from
#   E2E_NAMESPACE       claimed pilot namespace
#   E2E_PACKAGE         package inside that namespace
#   BOT_ACTOR_IDS       comma-separated numeric GitHub actor ids of the index
#                       bot — never logins (Key Decision D-3)
#
# OCX_ANNOUNCE_TOKEN is required only by announce_capture(), which guards it
# itself: the polling-only drivers authenticate through `gh` and must stay
# runnable on a machine that carries no announce credential at all. See
# README.md "Prerequisites".

# shellcheck shell=bash

set -euo pipefail
IFS=$'\n\t'

: "${GH_REPO_PUBLISHER:?set GH_REPO_PUBLISHER before running}"
: "${GH_REPO_INDEX:?set GH_REPO_INDEX before running}"
: "${INDEX_FORK:?set INDEX_FORK before running}"
: "${E2E_NAMESPACE:?set E2E_NAMESPACE before running}"
: "${E2E_PACKAGE:?set E2E_PACKAGE before running}"
: "${BOT_ACTOR_IDS:?set BOT_ACTOR_IDS before running (numeric ids, comma-separated)}"

E2E_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_REPO_ROOT="$(cd "$E2E_SCRIPT_DIR/../../../.." && pwd)"
RESULTS_DIR="${RESULTS_DIR:-$E2E_SCRIPT_DIR/../results}"
RUN_ID="${RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
ANNOUNCE_BRANCH="indexbot-announce-${E2E_NAMESPACE}-${E2E_PACKAGE}"
INDEX_SITE="${INDEX_SITE:-https://index.ocx.sh}"

# The identifier prefix a `[registries]` key matches. NOT the namespace:
# `<ns>/<pkg>` resolves to `<prefix>/<ns>/<pkg>`, and both Context's index-source
# build and announce's own lookup key on the identifier's registry component.
# Keying config on `<ns>` parses fine, matches nothing, and silently degrades to
# plain-OCI resolution.
E2E_INDEX_PREFIX="${E2E_INDEX_PREFIX:-ocx.sh}"

# The single workflow-run observation poll_run polls on: "<conclusion>|<url>".
# The separator is a pipe, not a tab, because `read` collapses runs of IFS
# whitespace: on a tab-separated record an absent conclusion would shift the
# URL into the conclusion's slot. No conclusion or run URL contains a pipe.
# Assigned by _run_concluded, read by poll_run — see both below.
E2E_RUN_RECORD=""

# Secrets reach the evidence module by env var NAME, never by value — a value
# on argv is readable from /proc/<pid>/cmdline for the life of the call.
# Callers with no credential in scope pass EVIDENCE_NO_SECRETS instead.
EVIDENCE_SECRETS=(--secrets-env OCX_ANNOUNCE_TOKEN)
EVIDENCE_NO_SECRETS=(--no-secrets)

# Which of the two above a driver should use, decided once: redact when a token
# is actually in scope, state "nothing to redact" when it is not.
evidence_secret_args() {
    if [[ -n ${OCX_ANNOUNCE_TOKEN:-} ]]; then
        printf '%s\n' "${EVIDENCE_SECRETS[@]}"
    else
        printf '%s\n' "${EVIDENCE_NO_SECRETS[@]}"
    fi
}

# Colors, the `ocx` echo-wrapper, ocx_step / ocx_warn / ocx_done / ocx_fail.
# shellcheck source=../../scripts/_lib.sh
source "$E2E_SCRIPT_DIR/../../scripts/_lib.sh"

mkdir -p "$RESULTS_DIR"

# Poll deadlines, seconds. Ten minutes is the ceiling for anything a machine
# drives; the namespace claim waits on a human review click, so it gets an
# hour. ponytail: two constants, not a per-call policy object — raise them
# through the environment if a run needs longer.
POLL_DEADLINE_SECONDS="${POLL_DEADLINE_SECONDS:-600}"
CLAIM_DEADLINE_SECONDS="${CLAIM_DEADLINE_SECONDS:-3600}"

# Run the pure-logic evidence module. PYTHONPATH points at test/src so the
# module imports without dragging src/__init__.py's registry helpers in.
evidence() {
    (cd "$E2E_REPO_ROOT/test" && PYTHONPATH=src uv run python -m announce_e2e.evidence "$@")
}

# Retry a command until it succeeds or the deadline passes. Backoff 2s,
# doubling, capped at 30s. Args: <deadline-seconds> <description> <command...>
poll_until() {
    local deadline="$1" what="$2"
    shift 2
    local delay=2 elapsed=0
    while true; do
        if "$@"; then
            return 0
        fi
        if ((elapsed >= deadline)); then
            ocx_warn "gave up waiting for $what after ${elapsed}s"
            return 1
        fi
        sleep "$delay"
        elapsed=$((elapsed + delay))
        delay=$((delay * 2))
        ((delay > 30)) && delay=30
        true # keep the arithmetic test above from tripping `set -e`
    done
}

# Run an announce and capture its report, redacted, under results/.
# Args: <step-label> <announce args...>. Echoes the captured path.
#
# `--format` is a ROOT flag on ocx, never a subcommand flag.
announce_capture() {
    : "${OCX_ANNOUNCE_TOKEN:?announce needs OCX_ANNOUNCE_TOKEN — see README.md Prerequisites}"
    local step="$1"
    shift
    local out="$RESULTS_DIR/${RUN_ID}-${step}.json"
    # pipefail carries an announce failure through the redaction filter, so a
    # failed run never leaves an empty-but-successful capture behind.
    ocx --format json package announce "$@" |
        evidence redact "${EVIDENCE_SECRETS[@]}" >"$out"
    printf '%s\n' "$out"
}

# The publisher checkout tags are pushed from. Track C may already have one
# under .agents/worktrees/ — reuse it rather than creating a second checkout of
# a foreign repo (plan Risks). Its worktree may carry a `-<slug>` suffix, hence
# the glob rather than a fixed path.
publisher_checkout() {
    local candidate
    if [[ -n ${PUBLISHER_WORKTREE:-} ]]; then
        printf '%s\n' "$PUBLISHER_WORKTREE"
        return 0
    fi
    for candidate in "$E2E_REPO_ROOT/.agents/worktrees/ocx-e2e-publisher"*; do
        if [[ -e $candidate/.git ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    ocx_fail "no publisher checkout found — set PUBLISHER_WORKTREE to a clone of $GH_REPO_PUBLISHER"
}

# Echo the number of the pull request whose head is the announce branch on the
# fork, or return 1. Matches head repository owner as well as branch name — a
# same-named branch on another fork is a different pull request.
pr_number() {
    local number
    number="$(gh pr list --repo "$GH_REPO_INDEX" --state all --limit 20 \
        --json number,headRefName,headRepositoryOwner \
        --jq "[.[] | select(.headRefName == \"$ANNOUNCE_BRANCH\" and .headRepositoryOwner.login == \"${INDEX_FORK%%/*}\")] | first | .number // empty")"
    [[ -n $number ]] || return 1
    printf '%s\n' "$number"
}

# Wait for the announce branch's pull request to exist. Echoes its number.
poll_pr() {
    poll_until "$POLL_DEADLINE_SECONDS" "a pull request on $ANNOUNCE_BRANCH" \
        pr_number >/dev/null
    pr_number
}

# Wait for every check on a pull request to settle, then require them all
# green. Args: <pr-number>.
poll_check() {
    local pr="$1" pending
    poll_until "$POLL_DEADLINE_SECONDS" "checks on PR #$pr" _checks_settled "$pr" || return 1
    pending="$(gh pr checks "$pr" --repo "$GH_REPO_INDEX" --json name,state \
        --jq '[.[] | select(.state != "SUCCESS" and .state != "SKIPPED") | .name] | join(", ")')"
    if [[ -n $pending ]]; then
        ocx_warn "PR #$pr checks not green: $pending"
        return 1
    fi
}

_checks_settled() {
    # grep exits 1 when no check is still PENDING — that is the success case.
    ! gh pr checks "$1" --repo "$GH_REPO_INDEX" --json state --jq '.[].state' |
        grep -qx 'PENDING'
}

# Wait for one workflow run to conclude. Args: <repo> <workflow> <ref> <sha>.
# Echoes the run URL, and only on success; returns 1 otherwise.
#
# <workflow> and <ref> are part of a run's identity because a commit sha alone
# is not: a tag push and a branch push at one sha are two runs, and a merge on
# the index's main is one run per workflow. <ref> is the run's headBranch,
# which for a tag push is the tag name. Every field comes from one observation,
# because two `gh` calls can select different runs.
poll_run() {
    local repo="$1" workflow="$2" ref="$3" sha="$4" conclusion url
    E2E_RUN_RECORD=""
    poll_until "$POLL_DEADLINE_SECONDS" "$workflow to conclude at ${sha:0:7} ($ref) in $repo" \
        _run_concluded "$repo" "$workflow" "$ref" "$sha" || return 1
    IFS='|' read -r conclusion url <<<"$E2E_RUN_RECORD"
    if [[ $conclusion != "success" ]]; then
        ocx_warn "$workflow at ${sha:0:7} ($ref) in $repo concluded $conclusion — $url"
        return 1
    fi
    printf '%s\n' "$url"
}

# Assigns E2E_RUN_RECORD in the caller's shell: poll_until invokes probes as
# `"$@"` in the current shell, so only the `gh` call is a subshell.
#
# The gate is a non-empty conclusion — the field poll_run judges on — not a
# `completed` status: a run is briefly `completed` with a null conclusion, and
# polling through that window is what keeps the read unambiguous. `first // {}`
# keeps the not-started case (no run matches) a valid empty record rather than
# a jq error; it, the in-progress case, and a failed `gh` all leave the
# conclusion empty and are all polled on.
_run_concluded() {
    E2E_RUN_RECORD="$(gh run list --repo "$1" --workflow "$2" --branch "$3" --limit 50 \
        --json headSha,conclusion,url \
        --jq "[.[] | select(.headSha == \"$4\")] | first // {} |
              [.conclusion // \"\", .url // \"\"] | join(\"|\")")"
    [[ $E2E_RUN_RECORD == ?*"|"* ]]
}

# Wait for a pull request to merge. Args: <pr-number>. Echoes
# "<merge-commit-sha> <mergedAt>".
poll_merge() {
    poll_until "$POLL_DEADLINE_SECONDS" "PR #$1 to merge" _pr_merged "$1" || return 1
    gh pr view "$1" --repo "$GH_REPO_INDEX" --json mergeCommit,mergedAt \
        --jq '"\(.mergeCommit.oid) \(.mergedAt)"'
}

_pr_merged() {
    [[ "$(gh pr view "$1" --repo "$GH_REPO_INDEX" --json state --jq '.state')" == "MERGED" ]]
}

# Tag names committed in the root on a ref of the fork. Args: <ref>.
# One tag per line.
committed_tags() {
    gh api "repos/$INDEX_FORK/contents/p/$E2E_NAMESPACE/$E2E_PACKAGE.json?ref=$1" \
        --header 'Accept: application/vnd.github.raw' --jq '.tags | keys[]'
}

# Merge a pull request's reviews and issue events into the single JSON array
# `evidence classify-lane` consumes. Args: <pr-number>.
pr_events() {
    local objects
    objects="$(
        {
            gh api "repos/$GH_REPO_INDEX/pulls/$1/reviews" --jq '.[]'
            gh api "repos/$GH_REPO_INDEX/issues/$1/events" --jq '.[]'
        } | paste -sd, -
    )"
    printf '[%s]' "$objects"
}

# Playbook 1: show the digest the floating dev tag currently resolves to, next
# to where the published digest is recorded. Args: <reference>, e.g.
# dev.ocx.sh/ocx/cli:0.5.0-dev.
check_floating_tag() {
    ocx_step "resolving $1"
    ocx --format json package inspect "$1"
    ocx_warn "compare that digest against the latest Deploy Dev run's published digest:"
    printf '  gh run list --repo ocx-sh/ocx --workflow "Deploy Dev" --limit 5\n' >&2
    ocx_warn "if they differ, the deploy failed to advance the floating tag — re-run"
    ocx_warn "Deploy Dev. Never hand-pin a _<TS> build segment (register §7 step 5)."
}
