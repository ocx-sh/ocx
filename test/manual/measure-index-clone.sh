#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# Release gates 1 and 2 of `.claude/artifacts/plan_index_claim_command.md`,
# measured against the public index repository.
#
# WHAT IS MEASURED. `git init .` followed by
# `git fetch --filter=blob:none <remote> <base>:<tracking>` - the operation
# `GitWorkspace::fetch_refs` in `crates/ocx_lib/src/forge/git_workspace.rs`
# actually performs. Deliberately NOT `git clone --filter=blob:none`: a clone
# materialises a working tree, so it backfills nearly every blob reachable from
# HEAD and transfers several times what ocx does. Measuring a clone would put a
# number in the release notes that no ocx run can produce.
#
# Gate 1 - blobless-fetch cost. Wall clock, on-disk size and bytes transferred,
#   each reported beside an unfiltered fetch of the same refspec taken on the
#   same host in the same run.
#
# Gate 2 - proof the filter APPLIED, not merely that the result came out small.
#   A server that silently ignores an unsupported filter leaves the size
#   compare looking correct and the cost rationale defeated; only the
#   unfiltered negative control tells the two apart (plan DV-5). Two
#   independent discriminators, both required, each asserted on BOTH sides:
#     (a) `git rev-list --objects --all --missing=print` reports a non-zero
#         missing-object count on the filtered side and exactly zero on the
#         control - so the counter is shown able to read zero, and a green is
#         distinguishable from a counter that never ran;
#     (b) a GIT_TRACE_PACKET capture shows the client sending `filter
#         blob:none` on the filtered fetch and sending no filter at all on the
#         control, while the server advertises the `filter` capability to
#         both - so "server refused the filter" stays distinguishable from
#         "client never asked for one".
#   The script exits non-zero if either half of the discrimination fails.
#
# SCOPE. This measures a GitHub-hosted repository. It does NOT prove GitLab
# behaviour, and no result here may be cited as if it did. Partial clone
# against a self-managed GitLab is release gate 4, which runs on the
# https://github.com/ocx-sh/ocx/issues/411 reporter's instance and cannot be
# executed from this repository.
#
# Usage: test/manual/measure-index-clone.sh [REPO_URL]
#        REPO_URL defaults to https://github.com/ocx-sh/index
#
# Needs network access and git >= 2.31. Protocol v2 is pinned so the
# packet-trace assertions are deterministic; v2 is also git's own default, so
# what is measured is what an ordinary fetch negotiates.
set -euo pipefail

# LC_ALL=C keeps git's progress lines English (they are parsed below) and pins
# EPOCHREALTIME's decimal separator to '.'.
export LC_ALL=C

REPO="${1:-https://github.com/ocx-sh/index}"

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
info() { printf '  %s\n' "$*"; }
ok() { printf '\033[32m  ok: %s\033[0m\n' "$*"; }
bad() {
    printf '\033[31m  FAIL: %s\033[0m\n' "$*" >&2
    failures=$((failures + 1))
}
die() {
    printf '\033[31mFAIL: %s\033[0m\n' "$*" >&2
    exit 1
}

failures=0

command -v git >/dev/null || die "git is not on PATH"

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/ocx-measure-index-clone.XXXXXX")"
FILTERED_DIR="$WORKDIR/filtered"
CONTROL_DIR="$WORKDIR/control"
FILTERED_TRACE="$WORKDIR/filtered.packet-trace"
CONTROL_TRACE="$WORKDIR/control.packet-trace"

# Bash 5 exposes EPOCHREALTIME; older shells fall back to whole seconds.
now_ms() {
    awk -v t="${EPOCHREALTIME:-0}" \
        'BEGIN { if (t > 0) printf "%d\n", t * 1000; else printf "%d\n", systime() * 1000 }'
}

# Bytes of packfile written by the fetch. Reported alongside git's own
# "Receiving objects" figure because the two are measured differently: the
# progress line is git's wire accounting, the packfile is what landed on disk.
pack_bytes() {
    find "$1/.git/objects/pack" -name '*.pack' -exec cat {} + 2>/dev/null | wc -c
}

disk_kib() { du -sk "$1" | cut -f1; }

# Total bytes git reports receiving, summed over EVERY completed fetch round.
# Reading only the last line would under-report any multi-round form. Prints
# "<bytes> <rounds>". Approximate: git rounds each figure to two decimals.
received_bytes() {
    tr '\r' '\n' <"$1" |
        sed -nE 's/.*Receiving objects: 100% \([0-9]+\/[0-9]+\), ([0-9.]+) (bytes|KiB|MiB|GiB).*/\1 \2/p' |
        awk '{
                 u = ($2 == "bytes") ? 1 : ($2 == "KiB") ? 1024 : ($2 == "MiB") ? 1048576 : 1073741824
                 total += $1 * u
                 rounds++
             }
             END { printf "%d %d\n", total + 0, rounds + 0 }'
}

# Missing-object count. --missing=print makes rev-list refuse to lazily fetch,
# and GIT_NO_LAZY_FETCH is belt and braces for older behaviour.
missing_objects() {
    GIT_NO_LAZY_FETCH=1 git -C "$1" rev-list --objects --all --missing=print 2>/dev/null |
        awk '/^\?/ { n++ } END { print n + 0 }'
}

# One `git init` + `git fetch`, traced and timed, in the shape fetch_refs uses.
# $1 label, $2 target dir, $3 trace file, rest: extra fetch args.
run_fetch() {
    local label="$1" dir="$2" trace="$3"
    shift 3
    local start end
    mkdir -p "$dir"
    git -C "$dir" init --quiet . >/dev/null
    start="$(now_ms)"
    GIT_TRACE_PACKET="$trace" git -C "$dir" -c protocol.version=2 fetch --progress "$@" \
        "$REPO" "$BASE_REF:refs/ocx-measure/base" \
        >"$WORKDIR/$label.stdout" 2>"$WORKDIR/$label.stderr" ||
        die "fetch ($label) failed; see $WORKDIR/$label.stderr"
    end="$(now_ms)"
    printf '%s\n' "$((end - start))"
}

report_side() {
    local label="$1" dir="$2" ms="$3"
    local wire bytes rounds
    wire="$(received_bytes "$WORKDIR/$label.stderr")"
    bytes="${wire% *}"
    rounds="${wire#* }"
    info "$label wall clock:        $(awk -v m="$ms" 'BEGIN { printf "%.2f s", m / 1000 }')"
    info "$label on-disk size:      $(disk_kib "$dir") KiB"
    info "$label bytes transferred: $(awk -v b="$bytes" 'BEGIN { printf "%.2f MiB", b / 1048576 }')" \
        "over $rounds fetch round(s) (git progress); $(pack_bytes "$dir") bytes on disk as packfile(s)"
}

step "Environment"
info "git:  $(git --version)"
info "repo: $REPO"
info "work: $WORKDIR"

# The default branch, resolved from the remote rather than assumed, so the two
# sides fetch an identical refspec.
BASE_REF="$(git ls-remote --symref "$REPO" HEAD | sed -n 's#^ref: refs/heads/\([^[:space:]]*\).*#\1#p' | head -n 1)"
[ -n "$BASE_REF" ] || die "could not resolve the default branch of $REPO"
info "base: $BASE_REF"

step "Gate 1 - blobless fetch (--filter=blob:none), the shape ocx performs"
filtered_ms="$(run_fetch filtered "$FILTERED_DIR" "$FILTERED_TRACE" --filter=blob:none)"
report_side filtered "$FILTERED_DIR" "$filtered_ms"

step "Gate 2 - unfiltered negative control (same host, same refspec, same run)"
control_ms="$(run_fetch control "$CONTROL_DIR" "$CONTROL_TRACE")"
report_side control "$CONTROL_DIR" "$control_ms"

step "Gate 2a - missing-object discrimination"
filtered_missing="$(missing_objects "$FILTERED_DIR")"
control_missing="$(missing_objects "$CONTROL_DIR")"
info "filtered missing objects: $filtered_missing"
info "control  missing objects: $control_missing"
if [ "$filtered_missing" -gt 0 ]; then
    ok "filtered fetch omits $filtered_missing objects - the filter took effect"
else
    bad "filtered fetch is missing no objects; the server ignored --filter=blob:none"
fi
if [ "$control_missing" -eq 0 ]; then
    ok "control fetch is complete - the counter can read zero, so it discriminates"
else
    bad "control fetch reports $control_missing missing objects; the control is not a control"
fi

step "Gate 2b - packet-trace discrimination"
# An outbound packet line is `packet: <command>> <payload>`; git labels it with
# the running command, not a fixed prefix. Counting the client's own request
# line is what separates "asked and got" from "never asked": the needle cannot
# appear in the control's trace unless the control requested a filter, and the
# control run's argv carries no filter at all, so the detector cannot match its
# own invocation.
filtered_req="$(awk '/packet:[ \t]+[a-z-]+> filter blob:none/ { n++ } END { print n + 0 }' "$FILTERED_TRACE")"
control_req="$(awk '/packet:[ \t]+[a-z-]+> filter / { n++ } END { print n + 0 }' "$CONTROL_TRACE")"
filtered_cap="$(awk '/packet:[ \t]+[a-z-]+< fetch=/ && /filter/ { n++ } END { print n + 0 }' "$FILTERED_TRACE")"
control_cap="$(awk '/packet:[ \t]+[a-z-]+< fetch=/ && /filter/ { n++ } END { print n + 0 }' "$CONTROL_TRACE")"
info "filtered client filter requests: $filtered_req   server filter advertisements: $filtered_cap"
info "control  client filter requests: $control_req   server filter advertisements: $control_cap"
if [ "$filtered_req" -gt 0 ]; then
    ok "filtered fetch negotiated 'filter blob:none'"
else
    bad "filtered fetch sent no filter request; the trace does not support the measurement"
fi
if [ "$control_req" -eq 0 ]; then
    ok "control fetch exercised no filter - the needle is absent when unasked"
else
    bad "control fetch sent $control_req filter request(s); the traces do not discriminate"
fi
if [ "$filtered_cap" -gt 0 ] && [ "$control_cap" -gt 0 ]; then
    ok "server advertises the 'filter' capability to both sides"
else
    bad "server did not advertise 'filter' (filtered=$filtered_cap control=$control_cap); a small fetch would prove nothing"
fi

step "Result"
info "packet traces kept for audit:"
info "  $FILTERED_TRACE"
info "  $CONTROL_TRACE"
rm -rf "$FILTERED_DIR" "$CONTROL_DIR"

if [ "$failures" -gt 0 ]; then
    die "$failures gate-2 discrimination check(s) failed"
fi
ok "gates 1 and 2 recorded; GitLab behaviour is release gate 4 and is NOT covered here"
