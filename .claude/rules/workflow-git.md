---
paths:
  - "CHANGELOG.md"
  - "cliff.toml"
  - "dist-workspace.toml"
---

# Git & Commit Workflow

Shared branch-and-commit hygiene for OCX. Consumed by the `/commit` skill (working phase) and the `/finalize` skill (rebasing phase). Catalog-only: referenced on demand, not auto-loaded via a path glob — nothing in the repo is "a git file".

## Branching Model

| Worktree | Branch |
|---|---|
| `ocx` | `goat` |
| `ocx-evelynn` | `evelynn` |
| `ocx-sion` | `sion` |
| `ocx-soraka` | `soraka` |

- **Never commit on `main`.** If you're on `main`, stop and create or switch to a feature/worktree branch first.
- **Never push.** Pushing triggers CI, which has real cost. The human decides when to push. No skill, agent, or automation pushes on its own.
- **Never `--no-verify`, `--no-gpg-sign`, or any hook-skipping flag.** If a hook fails, fix the root cause and create a new commit — hooks failed means the commit did not happen, so `--amend` would rewrite the *previous* commit.
- **Never `Co-Authored-By`** in commit messages. OCX convention.

## Two-Phase Model

A branch's commit history goes through two distinct phases. Each phase has a different goal, a different skill, and different rules.

| Phase | Skill | Goal | Rule |
|---|---|---|---|
| **Working** (default on worktree branches) | `/commit` | Save progress while iterating. Bundle freely. Amend rolling Checkpoints. | One concern per commit is **relaxed**. An honest bundle message is better than a fake narrative. |
| **Rebasing** (explicit, before landing) | `/finalize` | Produce the exact commits that will appear in the changelog | Strict Conventional Commits v1.0.0. One concern per commit. Reword/squash/split as needed. |

Default posture on the four worktree branches (`goat`, `evelynn`, `sion`, `soraka`): **working phase**. Do not badger the user about splitting during working phase — they will clean it up with `/finalize` before landing.

## Checkpoint Convention

A commit with the subject exactly `Checkpoint` (no type, no body) means "rolling WIP". It gets amended every time new work lands on top of it. It's never a commit that goes to `main`. `/finalize` refuses to land a branch that still contains one.

`task checkpoint` creates or amends the rolling Checkpoint automatically.

## Conventional Commits (Quick Rules)

Full cheat sheet: [`commit_reference.md`](../skills/commit/commit_reference.md) (types, scopes, footers, breaking changes, worked examples).

- Format: `<type>[optional scope]: <description>`
- Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `chore`
- **`chore:`** for AI/tooling files (`.claude/`, `CLAUDE.md`, skills, rules, hooks, taskfiles) — keeps them out of the user-facing changelog
- Imperative mood, lowercase description, no trailing period, subject ≤72 chars
- Body explains **why**, not what. Only when non-obvious.
- Breaking changes: `!` before the colon **and** a `BREAKING CHANGE:` footer

## Land-Ready Definition

A branch is ready to fast-forward onto `main` when **all** of these hold:

- [ ] Rebased on top of current `main` (no merge commits in `main..HEAD`)
- [ ] Every commit in `main..HEAD` has a Conventional Commits subject
- [ ] No `Checkpoint` commits remain
- [ ] No "bundle" commits that mix unrelated concerns (working-phase bundles must be split or squashed)
- [ ] Each commit represents one concern
- [ ] `task verify` passes on the final state

`/finalize` checks each of these and proposes a rebase plan for anything that fails.

## Quality Gate

Every commit on a branch must pass `task verify` before it lands on `main`. The `pre_commit_verification.py` hook enforces this on the tip commit. When the hook blocks:

1. Run `task verify` (never bypass with `--no-verify`).
2. Mark the verification state (separate `Bash` call — combining with the commit in one `&&` chain does not satisfy the hook):
   ```sh
   echo $(date +%s) > .claude/hooks/.state/commit-verified
   ```
3. Retry the commit.

## Phase Boundaries — When to Use Which Skill

| Situation | Use |
|---|---|
| Saving progress mid-task | `/commit` (working phase) |
| "Commit this as a proper conventional commit" | `/commit` (drafts message, stages, commits) |
| "Checkpoint this" / "save WIP" | `/commit` (creates/amends rolling Checkpoint) |
| Branch has messy history, prepare to land on main | `/finalize` |
| "Squash this branch into one commit for the changelog" | `/finalize` (squash-all mode) |
| Reword a stranded Checkpoint deep in history | `/finalize` |

## Submodule Workflow (`external/`)

Code in `external/` (e.g., `rust-oci-client`) is a fork of an upstream repo. Three rules:

1. **Upstream-first**: Make changes upstream-compliant. After the change works locally, plan an upstream PR.
2. **Format only new code**: Do NOT run `rustfmt`/`cargo fmt` on the entire file — only format lines you introduced. The upstream may use different style (e.g., 100-char width vs OCX's 120-char). Reformatting bloats diffs and blocks upstream PRs.
3. **No `Co-Authored-By`**: Submodule commits must not have `Co-Authored-By` trailers (upstream convention).

## References

- [`commit_reference.md`](../skills/commit/commit_reference.md) — Conventional Commits v1.0.0 cheat sheet
- [workflow-feature.md](./workflow-feature.md) — where commits fit in the broader feature flow
- [workflow-release.md](./workflow-release.md) — release-time branch handling
- `CLAUDE.md` — worktree layout, "Landing a feature" section
