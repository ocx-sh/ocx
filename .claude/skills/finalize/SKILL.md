---
name: finalize
description: Use when the user says "finalize", "/finalize", "prepare this branch for main", "clean up history", "squash the branch", or landing a branch on `main`. Rewrites Checkpoint working-phase commits into clean Conventional Commits before fast-forward merge. Flags: `--squash-all`, `--dry-run`.
user-invocable: true
disable-model-invocation: true
argument-hint: "[--squash-all | --dry-run]"
triggers:
  - "finalize"
  - "prepare this branch"
  - "prepare the branch"
  - "clean up history"
  - "squash the branch"
---

# /finalize — Prepare Branch for Fast-Forward Merge

Rewrites commits in `main..HEAD` so branch fast-forwards onto `main` as clean Conventional Commits sequence. **Rebasing phase** of two-phase branch workflow — see [workflow-git.md](../../rules/workflow-git.md) for full model.

Sibling `/commit` skill handles **working phase** (save progress during dev). `/finalize` takes messy result, produces what lands in main-branch changelog.

## When To Use

| Situation | Use |
|---|---|
| Branch has multiple `Checkpoint` commits | `/finalize` |
| Branch has working-phase bundles ("chore(claude): bundle skill + rules + agents") | `/finalize` |
| Rebasing branch onto current `main` before fast-forward | `/finalize` |
| Branch is one changelog entry with noise between | `/finalize --squash-all` |
| Already one-concern-per-commit and rebased | don't — fast-forward |

## Flags

- `--squash-all` — skip per-commit analysis, collapse `main..HEAD` into single commit. Skill drafts one message for whole diff, asks approval.
- `--dry-run` — produce classification + rebase plan, show, no execute. Useful for review.

## Workflow

### 1. Snapshot state (parallel batch)

- `git rev-parse --abbrev-ref HEAD` — current branch
- `git status --porcelain=v1` — working tree clean?
- `git fetch origin main --quiet 2>/dev/null` then `git rev-parse main` and `git rev-parse origin/main 2>/dev/null` — detect if local `main` lags `origin/main`
- `git log --oneline main..HEAD` — commits to finalize
- `git log -1 --pretty=%s main` — tip of main (plan reference)
- `git rev-list --count main..HEAD` — commit count
- `git diff --stat main..HEAD` — diff shape
- `git merge-base --is-ancestor main HEAD; echo $?` — local `main` ancestor of HEAD? (`0` = yes, branch based on current local main; `1` = no, needs rebase onto main as part of finalize)

**Refuse to proceed if**:

- Current branch is `main` — tell user switch to feature/worktree branch first.
- `main..HEAD` empty — nothing to finalize.
- Working tree dirty — tell user commit or stash with `/commit` first. No auto-stash; user must consciously save state.

**Rebase target — always current local `main`.** `/finalize` produces branch fast-forwarding onto local `main` HEAD. Two cases:

1. **Branch already based on current `main`** (`merge-base --is-ancestor main HEAD` returns 0). Rebase only rewrites commits in `main..HEAD` for hygiene — no parent change, no upstream-drift conflict risk.
2. **Branch behind `main`** (returns 1). Rebase does double duty: rewrites commits in `main..HEAD` for hygiene **and** moves branch onto current `main` HEAD. Conflicts here real, human resolves (see Step 4).

Both cases use `git rebase -i main`, replays `merge-base..HEAD` onto current `main`.

**`origin/main` handling.** If local `main` behind `origin/main`, surface before drafting plan, ask user how to proceed:

| Option | Effect |
|---|---|
| **Fast-forward local `main` first, then finalize** (default) | `git fetch origin main && git checkout main && git merge --ff-only origin/main && git checkout <branch>` then continue. Branch lands on top of latest published main. |
| **Finalize against current local `main`** | Skip fetch. Branch fast-forwards onto local main but may lag origin once published. |
| **Abort** | Stop without changes. |

Never touch `origin/main` directly (no force-push, no remote updates beyond `fetch`).

### 2. Classify each commit in `main..HEAD`

For every commit in `main..HEAD`, assign one category:

| Category | Signal | Action |
|---|---|---|
| **Keep** | Clean Conventional Commits subject, single concern, useful changelog entry | Leave alone |
| **Reword** | Single concern but subject non-conventional, typo'd, or mis-typed (e.g. `fix:` for real `feat:`) | Rewrite subject only, keep diff |
| **Squash** | Two+ adjacent commits cover same concern (fix + follow-up fixup, feat + missed test, rolling checkpoint amendments) | Collapse into one with drafted message |
| **Split** | One commit mixes unrelated concerns | Offer `git reset HEAD^` workflow; defer to human if non-trivial |
| **Drop** | Empty commit, reverted earlier in branch, pure noise | Remove |
| **Checkpoint** | Subject exactly `Checkpoint` | Reword (single concern inside) or squash into neighbour |

**Classification heuristics:**

- Subject line first. Non-conventional → Reword candidate.
- `git show --stat <sha>` for diff shape. Small diff, one module → likely Keep/Reword. Large diff, many modules → inspect body for multi-concern signals (`- bullet lists`, `and`, `also`, `plus`, bundle language).
- `chore(claude):` commits touching only `.claude/` or `CLAUDE.md` belong in working-phase, but on finalized branch should still be individually legitimate `chore(claude):` entries, not bundles.
- `git log --format=%B <sha> -1` for full message when subject ambiguous.

### 3. Draft the rebase plan

Present as numbered list user can scan. Example:

```
Rebase plan for evelynn (5 commits in main..HEAD):

  1. KEEP    a2a7072 feat(mirror): per-platform asset_type override
  2. SQUASH  b1c2d3e Checkpoint              → fold into (3)
  3. REWORD  e4f5g6h chore(claude): wip             → feat(cli): add --remote flag to catalog
  4. DROP    9a8b7c6 Revert previous wip fix  (undone later in the branch)
  5. KEEP    1234567 docs(guide): document --remote flag

Target base: main (039c066)
Final commit count: 3
```

Use `AskUserQuestion` with three options:

| Option | Effect |
|---|---|
| **Execute plan** (default) | Run scripted rebase |
| **Edit plan** | Let user adjust before exec (describe change in prose, redraw) |
| **Abort** | Stop without changes |

If user picked `--dry-run`, print plan and stop regardless of choice.

### 4. Execute the rebase (non-interactive)

Use scripted non-interactive `git rebase -i` pattern — **never** launch `$EDITOR`. Base always current local `main`, so same command both rewrites internal history (per plan) and replays branch onto latest local main HEAD.

```sh
# Capture starting state for emergency rollback
START_SHA=$(git rev-parse HEAD)

# Build a rebase-todo file from the plan, then run rebase with
# GIT_SEQUENCE_EDITOR pointing at `cat` on that file, and
# GIT_EDITOR pointing at a script that replaces any commit message
# during reword/squash with the pre-drafted one.
GIT_SEQUENCE_EDITOR="cp $TODO_FILE" \
GIT_EDITOR="$MSG_SCRIPT" \
git rebase -i main
```

Implementation notes:

- Generate rebase-todo file in temp dir. Each line is `pick|reword|squash|fixup|drop <sha> <subject>`.
- For reword/squash, pre-write target message to files named by SHA; `GIT_EDITOR` script reads matching file, overwrites message.
- On rebase conflict: stop, print conflicted file list, tell user what to do (`git status`, edit, `git rebase --continue`), exit. Do **not** auto-resolve.
- On success: show rewritten `git log --oneline main..HEAD` and final commit count.
- Backout: `git rebase --abort` (if still rebasing) or `git reset --hard $START_SHA` (if rebase done but user unhappy). Offer both explicitly.

### 5. `--squash-all` mode

When `--squash-all` passed, skip steps 2–4, instead:

1. Draft single Conventional Commits message covering full `main..HEAD` diff. Read `git log main..HEAD` for scope/concern inspiration but result is **one** commit — subject describes real user-visible change, not enumerate fixups.
2. Show drafted message. Ask approval.
3. Execute:
   ```sh
   git reset --soft main
   git commit -m "$(cat <<'EOF'
   <drafted message>
   EOF
   )"
   ```
4. Report new HEAD sha + subject.

Squash-all right when branch really one changelog entry — e.g. feature across 8 checkpoints + 3 fixups. Wrong when branch has independent concerns (refactor + unrelated bug fix) — decline, use per-commit plan instead.

### 6. Quality gate after rebase

After rebase, `task verify` must still pass on new HEAD. Run it. On fail:

1. Show failure.
2. User fixes (rebases can expose test interactions invisible in messy history).
3. On fix, rerun `/commit` (working phase for fix) then `/finalize` again if more shaping needed.

No push. Never push.

### 7. Report

One to three sentences:

- Starting commit count → final commit count
- Whether `task verify` passed
- Next step for user (`git checkout main && git merge --ff-only <branch>` — only if asked; else stop)

## Safety Rules

- **Always capture `START_SHA`** before first destructive op. Offer `git reset --hard $START_SHA` as recovery command if user unhappy.
- **Never force-push.** Skill only rewrites local history. Human decides push.
- **Never touch commits already on `origin/<branch>` without explicit confirmation.** Rewriting published history fine on feature branch human controls, but skill must flag (`git rev-list origin/<branch>..HEAD` for local-only vs published).
- **Never auto-resolve conflicts.** On conflict, stop, hand back to user.
- **Never launch `$EDITOR`.** Always scripted `GIT_SEQUENCE_EDITOR` + `GIT_EDITOR` pattern with pre-written files.

## References

- [workflow-git.md](../../rules/workflow-git.md) — shared branch/commit hygiene: branching model, two-phase model, Checkpoint convention, land-ready definition
- [`commit_reference.md`](../commit/commit_reference.md) — Conventional Commits v1.0.0 cheat sheet (types, scopes, footers, breaking changes)
- `/commit` skill (`../commit/SKILL.md`) — working-phase sibling
- CLAUDE.md "Landing a feature" — manual procedure this skill automates