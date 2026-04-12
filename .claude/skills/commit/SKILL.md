---
name: commit
description: Commit the current working changes on a feature or worktree branch. Opinionated for OCX's working-phase posture — minimise commit count, bundle freely, amend rolling Checkpoints when appropriate, detect stranded Checkpoints in recent history, offer a one-time feature-branch + PR prompt per branch. For rebasing-phase cleanup before landing on main, use `/finalize`. Use when the user says "commit", "/commit", "save progress", or asks to land changes.
user-invocable: true
disable-model-invocation: true
argument-hint: "[optional context or --pr | --no-pr | --amend]"
---

# /commit — Working-Phase Commit Workflow

Commit the working tree during active development on a feature or worktree branch. This is the **working phase** half of OCX's two-phase branch workflow — the rebasing-phase cleanup that prepares the branch for `main` lives in `/finalize`.

See [workflow-git.md](../../rules/workflow-git.md) for the full model: branching rules, two-phase model, Checkpoint convention, land-ready definition. This skill assumes those conventions and implements the working-phase posture on top of them.

## Working-Phase Posture

- **Minimise commit count.** Bundle freely. Amend rolling Checkpoints.
- **One concern per commit is relaxed.** An honest bundle message (`chore(claude): bundle skill + rule + agent tweaks`) is better than a fake one-concern narrative. `/finalize` will split or reword it later.
- **Don't badger the user about splitting.** The worktree branches (`goat`, `evelynn`, `sion`, `soraka`) are explicitly working-phase by default. Cleanup comes at finalize time.

For the contract of a branch that's ready to land on `main`, see the "Land-Ready Definition" in [workflow-git.md](../../rules/workflow-git.md). That's `/finalize`'s job, not `/commit`'s.

## Inputs

Optional free-text argument. Treat it as a hint about intent (e.g., `fix ocx install symlink bug`), not a full message — you still draft the final message. Flags:

- `/commit --amend` — explicitly amend HEAD (bypasses the Checkpoint-only safeguard)
- `/commit --pr` — force the PR-branch prompt even if previously declined
- `/commit --no-pr` — skip + record the PR skip

For rebasing-phase cleanup (squash, reword, split `main..HEAD`), use `/finalize` instead.

## Workflow

### 1. Snapshot state (parallel batch)

- `git status --porcelain=v1` (never `-uall`)
- `git diff --staged` and `git diff --stat`
- `git log -5 --oneline` — recent commits + scan for stranded `Checkpoint` in the window
- `git rev-parse --abbrev-ref HEAD` — current branch
- `git rev-list --count main..HEAD 2>/dev/null` — commits ahead of main
- `git config --get branch.<current>.ocx-skip-pr-prompt` — PR prompt already answered?
- `git log -1 --pretty=%s` — is HEAD itself a Checkpoint?
- `gh pr view --json number,state 2>/dev/null` — open PR for this branch?

If the working tree is clean **and** HEAD is not a Checkpoint, stop with "nothing to commit".

### 2. Checkpoint scan (window, not just HEAD)

Look at the last 5 commits for any with subject exactly `Checkpoint`.

**Case A — HEAD itself is `Checkpoint`**:

Two sub-cases based on working tree state:

| Sub-case | Behavior |
|---|---|
| **Dirty tree** (unstaged/untracked changes exist) | **Auto-amend** — stage all changed files by name and `git commit --amend --no-edit`. No question. Rolling Checkpoints absorb all active changes as a union. Report what was folded in. |
| **Clean tree** (no changes, Checkpoint holds accumulated work) | The Checkpoint itself is the deliverable. Draft a conventional commit message from the Checkpoint's diff (`git diff main..HEAD`), show it, and amend: `git commit --amend -m "<drafted message>"`. This is the "finalize in place" path — the user called `/commit` to give the Checkpoint a real name. |

**Case B — `Checkpoint` exists at HEAD~1..HEAD~5 but not HEAD (stranded)**: warn the user. It means a previous session made real work inside a Checkpoint then landed other commits on top without finalizing it. Offer:

| Option | Effect |
|---|---|
| **Commit on top, leave stranded** (default in working-phase) | Normal commit. The stranded Checkpoint will be handled by `/finalize` before landing |
| **Hand off to `/finalize` now** | Stop and tell the user to run `/finalize` for a full rebase plan — safer than rewording one commit in isolation |
| **Skip for now** | Ignore, continue |

**Case C — no Checkpoint in the window**: proceed.

### 3. Commit strategy

On a worktree branch, prefer one of:

1. **Amend HEAD** when HEAD is a fresh local commit you authored (not yet touching `main`'s reach) **and** the new WIP is clearly a continuation of the same concern. Ask first — amending is destructive to commit hashes.
2. **New commit on top** when HEAD already represents a distinct concern.
3. **Start a Checkpoint** (`git commit -m "Checkpoint"`) when the user says "save progress" or the WIP is not yet coherent enough to name.

On a non-worktree branch: never auto-amend. Always create a new commit unless `/commit --amend` was passed.

On `main`: stop. Tell the user to create a feature branch first.

### 4. Draft the commit message

Follow **Conventional Commits v1.0.0**. See [`commit_reference.md`](./commit_reference.md) for the full cheat sheet and [workflow-git.md](../../rules/workflow-git.md) for the quick rules. Key points for working phase:

- `chore:` for AI/tooling files (`.claude/`, `CLAUDE.md`, skills, rules, hooks, taskfiles) — keeps them out of the changelog
- Imperative mood, lowercase description, no trailing period, subject ≤72 chars
- Body explains **why**, not what, when non-obvious
- Breaking: `!` before the colon **and** a `BREAKING CHANGE:` footer
- **Never** `Co-Authored-By` (OCX convention)

**Working-phase bundles are legitimate.** If the staged diff covers multiple concerns, a `chore(claude): bundle <what>` style subject with a body that honestly lists the clusters is acceptable. Flag at the end of the body that it will be split or reworded during `/finalize`.

Show the drafted message to the user for approval before committing.

### 5. PR-branch prompt (first time per branch only)

Skip entirely if **any** of these are true:

- `git config --get branch.<current>.ocx-skip-pr-prompt` returns `true`
- An open PR already exists for the current branch
- The current branch is a fixed worktree branch (`goat`, `evelynn`, `sion`, `soraka`) — record the skip and move on
- The current branch is `main` — stop and tell the user to create a feature branch first

Otherwise, `AskUserQuestion`:

1. **Create feature branch + PR** — derive a name from the commit subject (e.g. `feat/relative-symlinks`), branch from HEAD, commit there, `gh pr create` with title from subject and a body summary
2. **Stay on current branch** — commit here. Record: `git config branch.<current>.ocx-skip-pr-prompt true`. Never ask again unless `/commit --pr`
3. **Cancel** — abort

### 6. Stage and commit

- Stage files **by name**, never `git add -A` / `.`. This prevents both accidentally-committed secrets **and** the bug where pre-staged files from a previous session get swept into a commit whose message doesn't describe them.
- Warn before staging anything matching `.env*`, `*credentials*`, `*.pem`, `*.key`, or `token` patterns; require explicit confirmation.
- **`--amend` must fold the dirty tree into HEAD.** When `/commit --amend` is invoked and the working tree has uncommitted changes, those changes **must** be staged and included in the amend — an `--amend` with nothing staged silently becomes a message-only amend that drops the user's active work. Always `git add <files>` before `git commit --amend`, even when the user only asked to "amend". After the amend, run `git show --stat HEAD` and confirm the expected files appear in the diff stat before reporting success.
- Pre-commit hook (`pre_commit_verification.py`) blocks commits without fresh `task verify`. When it blocks, run `task verify` (not `--no-verify`), then mark state in a separate `Bash` call (combining with the commit in one `&&` chain does not satisfy the hook):
  ```sh
  echo $(date +%s) > .claude/hooks/.state/commit-verified
  ```
  Then retry. **On retry, re-run `git add` too** — a blocked Bash invocation ran nothing, not even the part before `&&`, so staging is gone if it was chained.
- Commit with a HEREDOC:

  ```sh
  git commit -m "$(cat <<'EOF'
  <type>[scope]: <description>

  <optional body explaining why>
  EOF
  )"
  ```

- **Never** `--no-verify`, `--no-gpg-sign`, or any hook-skipping flags. If a hook fails, fix the root cause and create a **new** commit (not `--amend` — the previous commit stands).
- **Never push.** The human decides when to push.

### 7. Report

One or two sentences: commit hash, subject, whether the stranded-Checkpoint case was handled, whether a PR was opened. Nothing else.

## References

- [workflow-git.md](../../rules/workflow-git.md) — shared branch/commit hygiene: branching model, two-phase model, Checkpoint convention, land-ready definition
- [`commit_reference.md`](./commit_reference.md) — Conventional Commits v1.0.0 cheat sheet
- `/finalize` skill (`../finalize/SKILL.md`) — the rebasing-phase sibling for cleaning up `main..HEAD` before landing
- CLAUDE.md — worktree layout, "Landing a feature" section
