#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# git resolves every refspec itself and feeds them in on stdin as
# `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines, so a redirection,
# an alias or a bare `git push` reaches the gate identically. `task git:hooks`
# copies this file to `.git/hooks/pre-push`.
#
# Why this hook is not a prek hook, while `commit-msg` is: prek hands a
# declared hook no stdin at all, and skips the pre-push run outright for a
# branch DELETE and for a `git push --all` that only creates refs — measured,
# not inferred. Deleting the trunk is a change to the trunk, so the gate must
# see those; only git's own stdin carries them.
#
# Two consumers, one stdin: the trunk gate and Git LFS both need those lines,
# and stdin can only be read once, so it is read here and replayed to each.
# The gate runs FIRST and `set -e` stops the hook on a refusal — a refused push
# must not upload LFS objects for a push that will not happen.
set -euo pipefail
IFS=$'\n\t'
root="$(git rev-parse --show-toplevel)"

refs="$(cat)"
[ -n "$refs" ] || exit 0

printf '%s\n' "$refs" | python3 "$root/scripts/commit_gate.py" --check-push "$root"

# `git lfs install` owns `post-checkout`, `post-commit` and `post-merge` on its
# own again, but not this one: it is ours, so it owes LFS the delegation its
# generated hook would have done. Same hard failure as the stock hook — this
# repository tracks its images in LFS, and pushing pointers without their
# objects is worse than refusing.
command -v git-lfs >/dev/null 2>&1 || {
    printf >&2 "\n%s\n\n" "This repository is configured for Git LFS but 'git-lfs' was not found on your path."
    exit 2
}
printf '%s\n' "$refs" | git lfs pre-push "$@"
