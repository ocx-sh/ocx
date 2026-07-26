#!/usr/bin/env bash
# Requirement (4): update-not-overwrite proof (C4) — two announces with the
# first pull request left unmerged produce a single pull request whose
# committed tag set is a superset of both runs' tags.
#
# Usage: run_update_union.sh <tag-a> <tag-b>
#
# Both tags must already exist on the registry — announce observes tags, it
# does not create them. Uses `--tags-file` (union-add) for both runs:
# `--tags` replaces the curated set, which would drop tag-a on the second run
# and prove nothing.
#
# Needs OCX_ANNOUNCE_TOKEN; see README.md "Prerequisites".
#
# Consumes from env.sh: GH_REPO_INDEX, INDEX_FORK, E2E_NAMESPACE, E2E_PACKAGE.

set -euo pipefail
IFS=$'\n\t'

# shellcheck source=./env.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

WORK_DIR="$(mktemp -d)"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

# One announce driven by a single-tag tags-file. Args: <label> <tag>.
announce_tag() {
    local label="$1" tag="$2"
    local tags_file="$WORK_DIR/$label.tags"
    printf '%s\n' "$tag" >"$tags_file"
    announce_capture "$label" \
        --package "$E2E_NAMESPACE/$E2E_PACKAGE" \
        --tags-file "$tags_file" \
        --fork "$INDEX_FORK" \
        --index-repo "$GH_REPO_INDEX"
}

main() {
    local tag_a="${1:?usage: run_update_union.sh <tag-a> <tag-b>}"
    local tag_b="${2:?usage: run_update_union.sh <tag-a> <tag-b>}"
    local pr_a pr_b committed union_result

    ocx_step "announce #1 with $tag_a"
    announce_tag union-1 "$tag_a" >/dev/null
    pr_a="$(poll_pr)" || ocx_fail "announce #1 opened no pull request"
    ocx_done "announce #1 opened PR #$pr_a"

    # The union is only proved while #1 is still open — a merged PR would make
    # run #2 a fresh branch off main and the second announce trivially a
    # superset of itself.
    [[ "$(gh pr view "$pr_a" --repo "$GH_REPO_INDEX" --json state --jq '.state')" == "OPEN" ]] ||
        ocx_fail "PR #$pr_a is no longer open — someone merged it mid-run; restart the scenario"

    ocx_step "announce #2 with $tag_b, PR #$pr_a deliberately left unmerged"
    announce_tag union-2 "$tag_b" >/dev/null
    pr_b="$(poll_pr)" || ocx_fail "no pull request after announce #2"

    if [[ $pr_b != "$pr_a" ]]; then
        evidence record --scenario update_union --status fail \
            --notes "announce #2 opened PR #$pr_b instead of updating #$pr_a — C4 contract broke, escalate to Track A" \
            "${EVIDENCE_SECRETS[@]}" \
            --out "$RESULTS_DIR/${RUN_ID}-update_union.record.json"
        ocx_fail "announce #2 opened PR #$pr_b instead of updating #$pr_a (C4 broken)"
    fi

    committed="$(committed_tags "$ANNOUNCE_BRANCH" | paste -sd, -)"
    ocx_step "committed tags on $ANNOUNCE_BRANCH: $committed"

    if ! union_result="$(evidence pr-union --committed "$committed" --expected "$tag_a,$tag_b")"; then
        evidence record --scenario update_union --status fail \
            --notes "committed tags [$committed] are not a superset of [$tag_a,$tag_b]: $union_result" \
            "${EVIDENCE_SECRETS[@]}" \
            --out "$RESULTS_DIR/${RUN_ID}-update_union.record.json"
        ocx_fail "committed tags are not a superset of both runs: $union_result"
    fi

    evidence record --scenario update_union --status pass \
        --pr-url "$(gh pr view "$pr_a" --repo "$GH_REPO_INDEX" --json url --jq '.url')" \
        --notes "single PR #$pr_a; committed [$committed] superset of [$tag_a,$tag_b]" \
        "${EVIDENCE_SECRETS[@]}" \
        --out "$RESULTS_DIR/${RUN_ID}-update_union.record.json"

    ocx_done "update-union proved: one PR (#$pr_a) carrying both tags"
}

main "$@"
