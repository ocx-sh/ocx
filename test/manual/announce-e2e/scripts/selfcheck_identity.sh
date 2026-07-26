#!/usr/bin/env bash
# Self-check for run and pull-request *identity* in env.sh — which observation
# on GitHub is the one this driver's own push produced.
#
# Both matchers used to be able to lock onto an artifact from an earlier
# rehearsal and certify the current run against it: the worst failure a harness
# has, because it reports green off stale data. Neither can be exercised live
# without burning a tag and an owner merge click, so the matching logic is
# driven here against recorded `gh` JSON, with `gh` stubbed on `PATH` — the
# pattern selfcheck_g2.sh established for `curl`.
#
# Covered: the freshness floor on both matchers, how the floor itself is read,
# newest-wins when several observations survive it, the polling gate (an
# unconcluded run is not a verdict), and the head-repository/branch guards on
# the pull-request side.
#
# The floor is a GitHub-issued counter, never a timestamp, so every fixture
# below carries a `createdAt` the matchers must ignore. The two cases named
# "createdAt is never consulted" hand a stale artifact the fresh-looking
# creation time a backward-skewed local clock would have made look valid, and
# require it to stay excluded.
#
# Be precise about what those two discriminate: that the current matchers key on
# `.databaseId` / `.number` and read no other field. They do NOT reproduce clock
# skew, and cannot — the matchers take a counter, so there is no timestamp left
# to skew, and replaying these fixtures against the old timestamp comparison
# hands it a number where it expects an RFC3339 string, which is a type
# mismatch and not a faithful repro. The skew defect was proven separately,
# against the old code with a real timestamp floor; this file only pins that the
# replacement is clock-free.
#
# NOT covered — deliberately: whether `gh` really returns these fields, and
# whether announce opens a pull request at all. Only the selection logic is
# under test, never the transport.
#
# Needs `jq` to evaluate the drivers' own `--jq` programs offline (`gh` embeds
# its own). Test-only: no driver gains a jq dependency.
#
# Usage: selfcheck_identity.sh    (no network, no credentials, no live state)

set -euo pipefail
IFS=$'\n\t'

command -v jq >/dev/null ||
    {
        printf 'selfcheck_identity.sh needs jq to replay the drivers --jq programs offline\n' >&2
        exit 1
    }

TMP="$(mktemp -d)"
trap 'rm -rf -- "$TMP"' EXIT

# The live-run variables env.sh demands. Nothing here reaches a network: the
# stub `gh` below is the only I/O, and env.sh is a pure sourcing surface.
export GH_REPO_PUBLISHER="stub/publisher" GH_REPO_INDEX="stub/index" \
    INDEX_FORK="stub/fork" E2E_NAMESPACE="ns" E2E_PACKAGE="pkg" \
    BOT_ACTOR_IDS="1" RESULTS_DIR="$TMP/results"

# Serves $STUB_GH_FIXTURE through whatever `--jq` program the caller passed, so
# the expression under test is the one the driver ships. `gh --jq` emits raw
# strings, hence `jq -r`. Every other flag is irrelevant to selection.
mkdir -p "$TMP/bin"
cat >"$TMP/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -uo pipefail
filter=""
while (($#)); do
    case "$1" in
        --jq) filter="$2" && shift 2 ;;
        *) shift ;;
    esac
done
if [[ -z $filter ]]; then
    printf 'stub gh: called without --jq\n' >&2
    exit 1
fi
jq -r "$filter" <"$STUB_GH_FIXTURE"
STUB
chmod +x "$TMP/bin/gh"
PATH="$TMP/bin:$PATH"
export PATH

STUB_GH_FIXTURE="$TMP/fixture.json"
export STUB_GH_FIXTURE

# shellcheck source=./env.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"

CASES=0
FAILURES=0

# The floors the driver would have read before acting: the highest run id and
# pull-request number GitHub had already issued. Everything at or below them
# belongs to an earlier rehearsal.
RUN_FLOOR="100"
PR_FLOOR="141"

# Creation times, present only so the cases can prove they are not consulted.
BEFORE="2025-12-31T23:59:00Z"
AFTER="2026-01-01T00:00:30Z"
SHA="0123456789abcdef0123456789abcdef01234567"
OTHER_SHA="fedcba9876543210fedcba9876543210fedcba98"

report() {
    if [[ $1 == pass ]]; then
        printf 'ok   %s\n' "$2"
    else
        FAILURES=$((FAILURES + 1))
        printf 'FAIL %s\n     %s\n' "$2" "$3"
    fi
}

# Args: <description> <expected stdout> <command...>. An empty expectation
# means the command must fail and print nothing — the "keep polling" verdict.
expect() {
    local what="$1" want="$2" got status=0
    shift 2
    got="$("$@" 2>&1)" || status=$?
    if [[ -z $want ]]; then
        if ((status == 0)); then
            report fail "$what" "expected failure, got: $got"
        elif [[ -n $got ]]; then
            report fail "$what" "expected no output, got: $got"
        else
            report pass "$what"
        fi
    elif ((status != 0)); then
        report fail "$what" "expected '$want', command failed: $got"
    elif [[ $got == "$want" ]]; then
        report pass "$what"
    else
        report fail "$what" "expected '$want', got: $got"
    fi
    CASES=$((CASES + 1))
}

# Write the array `gh` would have returned. Args: <element>...
fixture() {
    local IFS=,
    printf '[%s]' "$*" >"$STUB_GH_FIXTURE"
}

# One `gh run list` element. Args: <databaseId> <headSha> <createdAt>
# <conclusion, empty for a run still in flight> <url>.
run_obj() {
    local conclusion="null"
    [[ -z $4 ]] || conclusion="\"$4\""
    printf '{"databaseId": %s, "headSha": "%s", "createdAt": "%s", "conclusion": %s, "url": "%s"}' \
        "$1" "$2" "$3" "$conclusion" "$5"
}

# One `gh pr list` element. Args: <number> <createdAt> <state> <headRefName>
# <headRepositoryOwner>.
pr_obj() {
    printf '{"number": %s, "createdAt": "%s", "state": "%s", "headRefName": "%s", "headRepositoryOwner": {"login": "%s"}}' \
        "$1" "$2" "$3" "$4" "$5"
}

# _run_concluded assigns E2E_RUN_RECORD in its caller's shell rather than
# printing it; echo it so one expectation helper covers both matchers.
run_record() {
    _run_concluded "$@" || return 1
    printf '%s\n' "$E2E_RUN_RECORD"
}

main_selfcheck() {
    # --- The floors themselves: what the driver reads before it acts. ---

    fixture
    expect "floor — a repository with no run yet floors at 0" "0" \
        run_floor stub/repo e2e-publish 1.0.5

    fixture "$(run_obj 200 "$SHA" "$AFTER" success https://run/older)" \
        "$(run_obj 300 "$SHA" "$AFTER" success https://run/newest)"
    expect "floor — the run floor is the highest id, not the first listed" "300" \
        run_floor stub/repo e2e-publish 1.0.5

    fixture
    expect "floor — a branch with no pull request yet floors at 0" "0" \
        pr_floor

    fixture "$(pr_obj 142 "$AFTER" MERGED "$ANNOUNCE_BRANCH" stub)" \
        "$(pr_obj 143 "$AFTER" OPEN "$ANNOUNCE_BRANCH" stub)"
    expect "floor — the pull-request floor is the highest number, not the first listed" "143" \
        pr_floor

    fixture "$(pr_obj 142 "$AFTER" OPEN "$ANNOUNCE_BRANCH" someone-else)" \
        "$(pr_obj 143 "$AFTER" OPEN indexbot-announce-ns-other stub)"
    expect "floor — foreign forks and other packages do not raise the floor" "0" \
        pr_floor

    # --- Bug (a): a run at or below the floor can never be this run's. ---

    fixture "$(run_obj 100 "$SHA" "$BEFORE" success https://run/old)"
    expect "run — a run the floor already counted is not a verdict" "" \
        run_record stub/repo e2e-publish 1.0.5 "$SHA" "$RUN_FLOOR"

    fixture "$(run_obj 100 "$SHA" "$AFTER" success https://run/STALE)"
    expect "run — createdAt is never consulted: a fresh-dated stale run stays stale" "" \
        run_record stub/repo e2e-publish 1.0.5 "$SHA" "$RUN_FLOOR"

    fixture "$(run_obj 100 "$SHA" "$BEFORE" success https://run/old)" \
        "$(run_obj 200 "$SHA" "$AFTER" '' https://run/new)"
    expect "run — retracted-and-repushed tag: poll the new run, not the old verdict" "" \
        run_record stub/repo e2e-publish 1.0.5 "$SHA" "$RUN_FLOOR"

    fixture "$(run_obj 100 "$SHA" "$BEFORE" success https://run/old)" \
        "$(run_obj 200 "$SHA" "$AFTER" failure https://run/new)"
    expect "run — the new run's own conclusion is the verdict" "failure|https://run/new" \
        run_record stub/repo e2e-publish 1.0.5 "$SHA" "$RUN_FLOOR"

    fixture "$(run_obj 200 "$SHA" "$AFTER" success https://run/new)"
    expect "run — a fresh concluded run is read whole" "success|https://run/new" \
        run_record stub/repo e2e-publish 1.0.5 "$SHA" "$RUN_FLOOR"

    fixture "$(run_obj 200 "$SHA" "$AFTER" failure https://run/older)" \
        "$(run_obj 300 "$SHA" "$AFTER" success https://run/newest)"
    expect "run — several fresh runs: newest databaseId wins, not list order" \
        "success|https://run/newest" \
        run_record stub/repo e2e-publish 1.0.5 "$SHA" "$RUN_FLOOR"

    fixture "$(run_obj 200 "$OTHER_SHA" "$AFTER" success https://run/other)"
    expect "run — another commit's run never matches" "" \
        run_record stub/repo e2e-publish 1.0.5 "$SHA" "$RUN_FLOOR"

    fixture
    expect "run — no run yet is a poll, not a failure" "" \
        run_record stub/repo e2e-publish 1.0.5 "$SHA" "$RUN_FLOOR"

    # --- Bug (b): a pull request at or below the floor is a rehearsal's. ---

    fixture "$(pr_obj 141 "$BEFORE" CLOSED "$ANNOUNCE_BRANCH" stub)"
    expect "pr — an earlier rehearsal's closed pull request never wins" "" \
        pr_number "$PR_FLOOR"

    fixture "$(pr_obj 141 "$BEFORE" MERGED "$ANNOUNCE_BRANCH" stub)"
    expect "pr — an earlier rehearsal's merged pull request never wins" "" \
        pr_number "$PR_FLOOR"

    fixture "$(pr_obj 141 "$AFTER" MERGED "$ANNOUNCE_BRANCH" stub)"
    expect "pr — createdAt is never consulted: a fresh-dated stale pull request stays stale" "" \
        pr_number "$PR_FLOOR"

    fixture "$(pr_obj 141 "$BEFORE" MERGED "$ANNOUNCE_BRANCH" stub)" \
        "$(pr_obj 142 "$AFTER" OPEN "$ANNOUNCE_BRANCH" stub)"
    expect "pr — this run's pull request wins over the rehearsal it follows" "142" \
        pr_number "$PR_FLOOR"

    fixture "$(pr_obj 142 "$AFTER" MERGED "$ANNOUNCE_BRANCH" stub)"
    expect "pr — the machine lane's own pull request is found after it auto-merged" "142" \
        pr_number "$PR_FLOOR"

    fixture "$(pr_obj 142 "$AFTER" OPEN "$ANNOUNCE_BRANCH" stub)" \
        "$(pr_obj 143 "$AFTER" OPEN "$ANNOUNCE_BRANCH" stub)"
    expect "pr — a second fresh pull request is seen (C4 breakage is detectable)" "143" \
        pr_number "$PR_FLOOR"

    fixture "$(pr_obj 142 "$AFTER" OPEN "$ANNOUNCE_BRANCH" someone-else)"
    expect "pr — a same-named branch on another fork is another pull request" "" \
        pr_number "$PR_FLOOR"

    fixture "$(pr_obj 142 "$AFTER" OPEN indexbot-announce-ns-other stub)"
    expect "pr — another package's announce branch never matches" "" \
        pr_number "$PR_FLOOR"

    fixture
    expect "pr — no pull request yet is a poll, not a failure" "" \
        pr_number "$PR_FLOOR"

    printf '\n%d cases, %d failures\n' "$CASES" "$FAILURES"
    ((FAILURES == 0))
}

main_selfcheck
