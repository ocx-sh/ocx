#!/usr/bin/env bash
# Requirement (3): machine-lane proof (G-19) — a new tag's announce
# auto-merges with zero human clicks; tag-push-to-merge and tag-push-to-served
# latency measured and recorded.
#
# Usage: run_machine_lane.sh <tag>
#
# Use a tag distinct from run_sequence.sh's, so this is independent evidence
# rather than a re-read of the same pull request.
#
# Consumes from env.sh: GH_REPO_PUBLISHER, GH_REPO_INDEX, INDEX_FORK,
# E2E_NAMESPACE, E2E_PACKAGE, BOT_ACTOR_IDS.

set -euo pipefail
IFS=$'\n\t'

# shellcheck source=./env.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

# Fail the proof, recording why. Args: <notes>.
fail_proof() {
    local secret_args=()
    mapfile -t secret_args < <(evidence_secret_args)
    evidence record --scenario machine_lane --status fail \
        --notes "$1" \
        "${secret_args[@]}" \
        --out "$RESULTS_DIR/${RUN_ID}-machine_lane.record.json"
    ocx_fail "$1"
}

# Wait until the served root names this tag — the t0 -> t_serve leg.
_serves_tag() {
    curl -fsS "$INDEX_SITE/p/$E2E_NAMESPACE/$E2E_PACKAGE.json" | grep -q "\"$1\""
}

main() {
    local tag="${1:?usage: run_machine_lane.sh <tag>}"
    local checkout sha t0 pr merge_line merged_at lane latency_merge latency_serve t_serve
    local secret_args=()

    checkout="$(publisher_checkout)"

    ocx_step "pushing new tag $tag to $GH_REPO_PUBLISHER"
    t0="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    git -C "$checkout" fetch origin
    git -C "$checkout" tag "$tag" origin/main ||
        ocx_fail "tag $tag already exists — pick a new one, or retract it per README 'Cleaning up a rehearsal'"
    git -C "$checkout" push origin "refs/tags/$tag"
    sha="$(git -C "$checkout" rev-parse "refs/tags/$tag^{commit}")"

    ocx_step "waiting for the publisher's announce at ${sha:0:7}"
    poll_run "$GH_REPO_PUBLISHER" e2e-publish "$tag" "$sha" "$t0" >/dev/null ||
        fail_proof "publisher CI did not conclude successfully for tag $tag"

    # $t0 predates the push, so nothing an earlier rehearsal left on this
    # per-package branch can answer — including its merged pull request, which
    # this driver would otherwise take the merge latency and lane verdict off.
    pr="$(poll_pr "$t0")" || fail_proof "no pull request appeared on $ANNOUNCE_BRANCH for tag $tag"
    ocx_step "waiting for PR #$pr to auto-merge — take no action, that is the proof"
    merge_line="$(poll_merge "$pr")" ||
        fail_proof "PR #$pr did not auto-merge within ${POLL_DEADLINE_SECONDS}s — see PLAYBOOKS.md playbook 3"
    merged_at="${merge_line##* }"

    # The lane verdict comes from numeric actor ids on the merged PR's reviews
    # and events, never from a login string (Key Decision D-3). --require-machine
    # turns the verdict into an exit code, so this assertion never depends on
    # how the JSON happens to be spaced.
    local events
    events="$(pr_events "$pr")"
    lane="$(printf '%s' "$events" | evidence classify-lane --bot-actor-ids "$BOT_ACTOR_IDS")"
    ocx_step "lane classification: $lane"
    printf '%s' "$events" |
        evidence classify-lane --bot-actor-ids "$BOT_ACTOR_IDS" --require-machine >/dev/null ||
        fail_proof "PR #$pr did not merge through the machine lane with zero human clicks: $lane"

    ocx_step "waiting for $INDEX_SITE to serve tag $tag"
    poll_until "$POLL_DEADLINE_SECONDS" "$INDEX_SITE to serve $tag" _serves_tag "$tag" ||
        fail_proof "$INDEX_SITE never served tag $tag after the merge"
    t_serve="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

    latency_merge="$(evidence latency --start "$t0" --end "$merged_at")"
    latency_serve="$(evidence latency --start "$t0" --end "$t_serve")"

    mapfile -t secret_args < <(evidence_secret_args)
    evidence record --scenario machine_lane --status pass \
        --pr-url "$(gh pr view "$pr" --repo "$GH_REPO_INDEX" --json url --jq '.url')" \
        --latency "$latency_merge" \
        --notes "tag $tag; $lane; tag-push to merge ${latency_merge}s, tag-push to served ${latency_serve}s" \
        "${secret_args[@]}" \
        --out "$RESULTS_DIR/${RUN_ID}-machine_lane.record.json"

    ocx_done "machine lane proved: PR #$pr auto-merged, zero human clicks, ${latency_merge}s to merge / ${latency_serve}s to served"
}

main "$@"
