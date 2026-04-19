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

Rewrites the commits in `main..HEAD` so the branch can be fast-forwarded onto `main` as a clean Conventional Commits sequence. This is the **rebasing phase** of the two-phase branch workflow — see [workflow-git.md](../../rules/workflow-git.md) for the full model.

The sibling `/commit` skill handles the **working phase** (saving progress during development). `/finalize` takes the potentially messy result of that phase and produces what will actually appear in the main-branch changelog.

## When To Use

| Situation | Use |
|---|---|
| Branch has multiple `Checkpoint` commits | `/finalize` |
| Branch has working-phase bundles ("chore(claude): bundle skill + rules + agents") | `/finalize` |
| Rebasing branch onto current `main` before the fast-forward | `/finalize` |
| Branch is really one changelog entry with noise in between | `/finalize --squash-all` |
| Everything is already one-concern-per-commit and rebased | don't — just fast-forward |

## Flags

- `--squash-all` — skip the per-commit analysis and collapse `main..HEAD` into a single commit. The skill drafts one message covering the whole diff and asks for approval.
- `--dry-run` — produce the classification and rebase plan, show it, but do not execute. Useful for reviewing what would change.

## Workflow

### 1. Snapshot state (parallel batch)

- `git rev-parse --abbrev-ref HEAD` — current branch
- `git status --porcelain=v1` — is the working tree clean?
- `git fetch origin main --quiet 2>/dev/null` followed by `git rev-parse main` and `git rev-parse origin/main 2>/dev/null` — detect if local `main` lags behind `origin/main`
- `git log --oneline main..HEAD` — commits to finalize
- `git log -1 --pretty=%s main` — tip of main (for reference in the plan)
- `git rev-list --count main..HEAD` — commit count
- `git diff --stat main..HEAD` — overall shape of the diff
- `git merge-base --is-ancestor main HEAD; echo $?` — is local `main` already an ancestor of HEAD? (`0` = yes, branch is already based on current local main; `1` = no, the branch needs rebasing onto main as part of finalize)

**Refuse to proceed if**:

- Current branch is `main` — tell the user to switch to a feature/worktree branch first.
- `main..HEAD` is empty — nothing to finalize.
- Working tree is dirty — tell the user to commit or stash with `/commit` first. Do not auto-stash; the user needs to consciously save their state.

**Rebase target — always current local `main`.** `/finalize` produces a branch that fast-forwards onto local `main` HEAD. Two cases:

1. **Branch already based on current `main`** (`merge-base --is-ancestor main HEAD` returns 0). The rebase only rewrites the commits in `main..HEAD` for hygiene — no parent change, no risk of conflicts from upstream drift.
2. **Branch is behind `main`** (returns 1). The rebase does double duty: it rewrites the commits in `main..HEAD` for hygiene **and** moves the branch onto current `main` HEAD. Conflicts here are real and must be resolved by the human (see Step 4).

In both cases the underlying command is `git rebase -i main`, which always replays `merge-base..HEAD` onto current `main`.

**`origin/main` handling.** If local `main` is behind `origin/main`, surface this before drafting the plan and ask the user how to proceed:

| Option | Effect |
|---|---|
| **Fast-forward local `main` first, then finalize** (default) | `git fetch origin main && git checkout main && git merge --ff-only origin/main && git checkout <branch>` then continue. Ensures the branch lands on top of the latest published main. |
| **Finalize against current local `main`** | Skip the fetch. The branch will fast-forward onto local main but may still be behind origin once published. |
| **Abort** | Stop without changes. |

Never touch `origin/main` directly (no force-push, no remote updates beyond `fetch`).

### 2. Classify each commit in `main..HEAD`

For every commit in `main..HEAD`, assign one of these categories:

| Category | Signal | Action |
|---|---|---|
| **Keep** | Clean Conventional Commits subject, single concern, useful changelog entry | Leave alone |
| **Reword** | Single concern but subject is non-conventional, typo'd, or mis-typed (e.g. `fix:` for a real `feat:`) | Rewrite subject only, keep diff |
| **Squash** | Two or more adjacent commits cover the same concern (fix + follow-up fixup, feat + missed test, rolling checkpoint amendments) | Collapse into one with a drafted message |
| **Split** | One commit mixes genuinely unrelated concerns | Offer `git reset HEAD^` workflow; defer to human if the split is non-trivial |
| **Drop** | Empty commit, reverted earlier in the branch, pure noise | Remove |
| **Checkpoint** | Subject is exactly `Checkpoint` | Must be reworded (single concern inside) or squashed into a neighbour |

**Classification heuristics:**

- Look at the subject line first. Non-conventional subject → Reword candidate.
- Use `git show --stat <sha>` to understand the diff shape. Small diff touching one module → likely Keep/Reword. Large diff touching many modules → inspect body for multi-concern signals (`- bullet lists`, `and`, `also`, `plus`, bundle language).
- `chore(claude):` commits that only touch `.claude/` or `CLAUDE.md` belong in working-phase anyway, but on a branch being finalized they should still be individually legitimate `chore(claude):` entries, not bundles.
- Use `git log --format=%B <sha> -1` to read the full message when the subject is ambiguous.

### 3. Draft the rebase plan

Present as a numbered list the user can scan. Example:

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
| **Execute plan** (default) | Run the scripted rebase |
| **Edit plan** | Let the user adjust before executing (describe the change in prose, redraw) |
| **Abort** | Stop without changes |

If the user picked `--dry-run`, print the plan and stop here regardless of the choice.

### 4. Execute the rebase (non-interactive)

Use a scripted non-interactive `git rebase -i` pattern — **never** launch `$EDITOR`. The base is always current local `main`, so the same command both rewrites internal history (per the plan) and replays the branch onto the latest local main HEAD.

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

- Generate the rebase-todo file in a temp directory. Each line is `pick|reword|squash|fixup|drop <sha> <subject>`.
- For reword/squash, pre-write the target message to files named by SHA; the `GIT_EDITOR` script reads the matching file and overwrites the message.
- On rebase conflict: stop, print the conflicted file list, tell the user what to do (`git status`, edit, `git rebase --continue`), and exit. Do **not** try to auto-resolve.
- On success: show the rewritten `git log --oneline main..HEAD` and the final commit count.
- If anything goes wrong and the user wants to back out: `git rebase --abort` (if still rebasing) or `git reset --hard $START_SHA` (if rebase completed but user is unhappy). Offer both explicitly.

### 5. `--squash-all` mode

When `--squash-all` is passed, skip steps 2–4 and instead:

1. Draft a single Conventional Commits message covering the full `main..HEAD` diff. Read `git log main..HEAD` for inspiration on the scope/concern but remember the result is **one** commit — the subject must describe the real user-visible change, not enumerate the fixups.
2. Show the drafted message. Ask for approval.
3. Execute:
   ```sh
   git reset --soft main
   git commit -m "$(cat <<'EOF'
   <drafted message>
   EOF
   )"
   ```
4. Report the new HEAD sha + subject.

Squash-all is the right answer when the branch really is one changelog entry — e.g. a feature developed across 8 checkpoints plus 3 fixups. It is the wrong answer when the branch contains genuinely independent concerns (a refactor + an unrelated bug fix) — in that case, decline and use the per-commit plan instead.

### 6. Quality gate after rebase

After the rebase, `task verify` must still pass on the new HEAD. Run it. If it fails:

1. Show the failure.
2. The user has to fix it (rebases can expose test interactions that weren't visible in the messy history).
3. On fix, they re-run `/commit` (working phase for the fix) then `/finalize` again if more shaping is needed.

Do not push. Never push.

### 7. Report

One to three sentences:

- Starting commit count → final commit count
- Whether `task verify` passed
- What the user should do next (`git checkout main && git merge --ff-only <branch>` — but only if they asked about the next step; otherwise just stop)

## Safety Rules

- **Always capture `START_SHA`** before the first destructive operation. Offer `git reset --hard $START_SHA` as the recovery command if the user is unhappy.
- **Never force-push.** This skill only rewrites local history. The human decides when to push.
- **Never touch commits that already exist on `origin/<branch>` without explicit confirmation.** Rewriting published history is fine on a feature branch the human controls, but the skill must flag it (`git rev-list origin/<branch>..HEAD` to see what's local-only vs published).
- **Never auto-resolve conflicts.** If the rebase hits a conflict, stop and hand back to the user.
- **Never launch `$EDITOR`.** Always use the scripted `GIT_SEQUENCE_EDITOR` + `GIT_EDITOR` pattern with pre-written files.

## References

- [workflow-git.md](../../rules/workflow-git.md) — shared branch/commit hygiene: branching model, two-phase model, Checkpoint convention, land-ready definition
- [`commit_reference.md`](../commit/commit_reference.md) — Conventional Commits v1.0.0 cheat sheet (types, scopes, footers, breaking changes)
- `/commit` skill (`../commit/SKILL.md`) — the working-phase sibling
- CLAUDE.md "Landing a feature" — the manual procedure this skill automates
