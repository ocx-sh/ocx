#!/usr/bin/env bash
# Requirement (2): idempotency proof (C6) — an immediate re-run of the
# identical announce reports status "unchanged", opens zero pull requests and
# adds zero commits to the announce branch.
#
# Usage: run_idempotency.sh [extra announce args...]
#
# Runs announce locally with `--refresh` — "re-observe every already-committed
# tag" is exactly the identical-inputs re-run C6 is about. Needs
# OCX_ANNOUNCE_TOKEN in the environment; see README.md "Prerequisites".
#
# Consumes from env.sh: GH_REPO_INDEX, INDEX_FORK, E2E_NAMESPACE, E2E_PACKAGE.

set -euo pipefail
IFS=$'\n\t'

# shellcheck source=./env.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

# Pull request count on the announce branch: 0 or 1 by construction, but a
# second PR is precisely the C4/C6 failure this counts. Counts exactly what
# pr_floor and pr_number select — E2E_PR_SELECT over pr_list's window — so a
# foreign fork's pull request can never read as movement here while being
# invisible to the matchers.
pr_count() {
    pr_list "[.[] | select($E2E_PR_SELECT)] | length"
}

# Head commit of the announce branch on the fork, or "absent".
branch_head() {
    gh api "repos/$INDEX_FORK/git/ref/heads/$ANNOUNCE_BRANCH" --jq '.object.sha' 2>/dev/null ||
        printf 'absent\n'
}

main() {
    local prs_before head_before capture status prs_after head_after failures=()

    prs_before="$(pr_count)"
    head_before="$(branch_head)"
    ocx_step "before: $prs_before pull request(s), branch head ${head_before:0:7}"

    ocx_step "re-announcing $E2E_NAMESPACE/$E2E_PACKAGE with identical inputs"
    capture="$(announce_capture idempotency \
        --package "$E2E_NAMESPACE/$E2E_PACKAGE" \
        --refresh \
        --fork "$INDEX_FORK" \
        --index-repo "$GH_REPO_INDEX" \
        "$@")"

    status="$(evidence classify-report --file "$capture")"
    prs_after="$(pr_count)"
    head_after="$(branch_head)"

    [[ $status == "unchanged" ]] ||
        failures+=("status is \"$status\", not \"unchanged\" — the C6 contract broke, escalate to Track A")
    [[ $prs_after == "$prs_before" ]] ||
        failures+=("pull request count moved $prs_before -> $prs_after")
    [[ $head_after == "$head_before" ]] ||
        failures+=("branch head moved ${head_before:0:7} -> ${head_after:0:7}")

    if ((${#failures[@]} > 0)); then
        printf 'idempotency assertion failed: %s\n' "${failures[@]}" >&2
        evidence record --scenario idempotency --status fail \
            --notes "${failures[*]}" \
            "${EVIDENCE_SECRETS[@]}" \
            --out "$RESULTS_DIR/${RUN_ID}-idempotency.record.json"
        exit 1
    fi

    evidence record --scenario idempotency --status pass \
        --notes "status=unchanged; $prs_before PR(s) unchanged; branch head ${head_before:0:7} unchanged" \
        "${EVIDENCE_SECRETS[@]}" \
        --out "$RESULTS_DIR/${RUN_ID}-idempotency.record.json"

    ocx_done "idempotency proved: unchanged, zero new PRs, zero new commits"
}

main "$@"
