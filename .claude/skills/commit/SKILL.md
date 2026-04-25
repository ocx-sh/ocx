---
name: commit
description: Use when the user says "commit", "/commit", "save progress", or asks to land working changes on a feature or worktree branch. OCX working-phase posture — minimises commit count, can amend rolling Checkpoints, offers a one-time PR prompt. For pre-merge cleanup, use `/finalize`.
user-invocable: true
disable-model-invocation: true
argument-hint: "[optional context or --pr | --no-pr | --amend]"
triggers:
  - "commit"
  - "save progress"
  - "commit my changes"
  - "commit the changes"
  - "stage and commit"
---

# /commit — Working-Phase Commit Workflow

Commit working tree during active dev on feature/worktree branch. **Working phase** half of OCX two-phase branch workflow — rebasing-phase cleanup for `main` lives in `/finalize`.

See [workflow-git.md](../../rules/workflow-git.md) for full model: branching rules, two-phase model, Checkpoint convention, land-ready definition. Skill assumes those, implements working-phase posture on top.

## Working-Phase Posture

- **Minimise commit count.** Bundle freely. Amend rolling Checkpoints.
- **One concern per commit relaxed.** Honest bundle message (`chore(claude): bundle skill + rule + agent tweaks`) beats fake one-concern narrative. `/finalize` split/reword later.
- **Don't badger user about splitting.** Worktree branches (`goat`, `evelynn`, `sion`, `soraka`) explicitly working-phase by default. Cleanup at finalize time.

For land-ready branch contract, see "Land-Ready Definition" in [workflow-git.md](../../rules/workflow-git.md). That `/finalize` job, not `/commit`.

## Inputs

Optional free-text arg. Treat as intent hint (e.g., `fix ocx install symlink bug`), not full message — still draft final message. Flags:

- `/commit --amend` — explicitly amend HEAD (bypasses Checkpoint-only safeguard)
- `/commit --pr` — force PR-branch prompt even if previously declined
- `/commit --no-pr` — skip + record PR skip

For rebasing-phase cleanup (squash, reword, split `main..HEAD`), use `/finalize` instead.

## Workflow

### 1. Snapshot state (parallel batch)

- `git status --porcelain=v1` (never `-uall`)
- `git diff --staged` and `git diff --stat`
- `git log -5 --oneline` — recent commits + scan for stranded `Checkpoint` in window
- `git rev-parse --abbrev-ref HEAD` — current branch
- `git rev-list --count main..HEAD 2>/dev/null` — commits ahead of main
- `git config --get branch.<current>.ocx-skip-pr-prompt` — PR prompt already answered?
- `git log -1 --pretty=%s` — HEAD itself Checkpoint?
- `gh pr view --json number,state 2>/dev/null` — open PR for branch?

If working tree clean **and** HEAD not Checkpoint, stop with "nothing to commit".

### 2. Checkpoint scan (window, not just HEAD)

Look at last 5 commits for any with subject exactly `Checkpoint`.

**Case A — HEAD itself is `Checkpoint`**:

Two sub-cases by working tree state:

| Sub-case | Behavior |
|---|---|
| **Dirty tree** (unstaged/untracked changes exist) | **Auto-amend** — stage all changed files by name and `git commit --amend --no-edit`. No question. Rolling Checkpoints absorb all active changes as union. Report what folded in. |
| **Clean tree** (no changes, Checkpoint holds accumulated work) | Checkpoint itself is deliverable. Draft conventional commit message from Checkpoint diff (`git diff main..HEAD`), show, amend: `git commit --amend -m "<drafted message>"`. "Finalize in place" path — user called `/commit` to give Checkpoint real name. |

**Case B — `Checkpoint` exists at HEAD~1..HEAD~5 but not HEAD (stranded)**: warn user. Means previous session made real work inside Checkpoint then landed other commits on top without finalizing. Offer:

| Option | Effect |
|---|---|
| **Commit on top, leave stranded** (default in working-phase) | Normal commit. Stranded Checkpoint handled by `/finalize` before landing |
| **Hand off to `/finalize` now** | Stop, tell user run `/finalize` for full rebase plan — safer than rewording one commit in isolation |
| **Skip for now** | Ignore, continue |

**Case C — no Checkpoint in window**: proceed.

### 3. Commit strategy

On worktree branch, prefer one of:

1. **Amend HEAD** when HEAD is fresh local commit you authored (not yet touching `main`'s reach) **and** new WIP clearly continuation of same concern. Ask first — amending destructive to commit hashes.
2. **New commit on top** when HEAD already represents distinct concern.
3. **Start Checkpoint** (`git commit -m "Checkpoint"`) when user says "save progress" or WIP not yet coherent enough to name.

On non-worktree branch: never auto-amend. Always create new commit unless `/commit --amend` passed.

On `main`: stop. Tell user create feature branch first.

### 4. Draft the commit message

Follow **Conventional Commits v1.0.0**. See [`commit_reference.md`](./commit_reference.md) for full cheat sheet and [workflow-git.md](../../rules/workflow-git.md) for quick rules. Key points for working phase:

- `chore:` for AI/tooling files (`.claude/`, `CLAUDE.md`, skills, rules, hooks, taskfiles) — keeps out of changelog
- Imperative mood, lowercase description, no trailing period, subject ≤72 chars
- Body explains **why**, not what, when non-obvious
- Breaking: `!` before colon **and** `BREAKING CHANGE:` footer
- **Never** `Co-Authored-By` (OCX convention)

**Working-phase bundles legitimate.** If staged diff covers multiple concerns, `chore(claude): bundle <what>` style subject with body honestly listing clusters acceptable. Flag at body end that it will split or reword during `/finalize`.

Show drafted message to user for approval before committing.

### 5. PR-branch prompt (first time per branch only)

Skip entirely if **any** true:

- `git config --get branch.<current>.ocx-skip-pr-prompt` returns `true`
- Open PR already exists for current branch
- Current branch is fixed worktree branch (`goat`, `evelynn`, `sion`, `soraka`) — record skip, move on
- Current branch is `main` — stop, tell user create feature branch first

Otherwise, `AskUserQuestion`:

1. **Create feature branch + PR** — derive name from commit subject (e.g. `feat/relative-symlinks`), branch from HEAD, commit there, `gh pr create` with title from subject and body summary
2. **Stay on current branch** — commit here. Record: `git config branch.<current>.ocx-skip-pr-prompt true`. Never ask again unless `/commit --pr`
3. **Cancel** — abort

### 6. Stage and commit

- Stage files **by name**, never `git add -A` / `.`. Prevents accidentally-committed secrets **and** bug where pre-staged files from previous session get swept into commit whose message doesn't describe them.
- Warn before staging anything matching `.env*`, `*credentials*`, `*.pem`, `*.key`, or `token` patterns; require explicit confirmation.
- **`--amend` must fold dirty tree into HEAD.** When `/commit --amend` invoked and working tree has uncommitted changes, those changes **must** be staged and included in amend — `--amend` with nothing staged silently becomes message-only amend that drops user's active work. Always `git add <files>` before `git commit --amend`, even when user only asked to "amend". After amend, run `git show --stat HEAD` and confirm expected files appear in diff stat before reporting success.
- Pre-commit hook (`pre_commit_verification.py`) blocks commits without fresh `task verify`. When blocks, run `task verify` (not `--no-verify`), then mark state in separate `Bash` call (combining with commit in one `&&` chain does not satisfy hook):
  ```sh
  echo $(date +%s) > .claude/hooks/.state/commit-verified
  ```
  Then retry. **On retry, re-run `git add` too** — blocked Bash invocation ran nothing, not even part before `&&`, so staging gone if chained.
- Commit with HEREDOC:

  ```sh
  git commit -m "$(cat <<'EOF'
  <type>[scope]: <description>

  <optional body explaining why>
  EOF
  )"
  ```

- **Never** `--no-verify`, `--no-gpg-sign`, or any hook-skipping flags. If hook fails, fix root cause and create **new** commit (not `--amend` — previous commit stands).
- **Never push.** Human decides when to push.

### 7. Report

One or two sentences: commit hash, subject, whether stranded-Checkpoint case handled, whether PR opened. Nothing else.

## References

- [workflow-git.md](../../rules/workflow-git.md) — shared branch/commit hygiene: branching model, two-phase model, Checkpoint convention, land-ready definition
- [`commit_reference.md`](./commit_reference.md) — Conventional Commits v1.0.0 cheat sheet
- `/finalize` skill (`../finalize/SKILL.md`) — rebasing-phase sibling for cleaning `main..HEAD` before landing
- CLAUDE.md — worktree layout, "Landing a feature" section