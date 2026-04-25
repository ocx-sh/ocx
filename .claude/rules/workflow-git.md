---
paths:
  - "CHANGELOG.md"
  - "cliff.toml"
  - "dist-workspace.toml"
---

# Git & Commit Workflow

Shared branch-and-commit hygiene for OCX. Used by `/commit` skill (working phase) and `/finalize` skill (rebasing phase). Catalog-only: referenced on demand, not auto-loaded via path glob — nothing in repo "a git file".

## Branching Model

| Worktree | Branch |
|---|---|
| `ocx` | `goat` |
| `ocx-evelynn` | `evelynn` |
| `ocx-sion` | `sion` |
| `ocx-soraka` | `soraka` |

- **Never commit on `main`.** If on `main`, stop and switch to feature/worktree branch first.
- **Never push.** Push triggers CI, real cost. Human decides when push. No skill, agent, automation push on own.
- **Never `--no-verify`, `--no-gpg-sign`, or any hook-skipping flag.** Hook fail → fix root cause, new commit — hook fail means commit did not happen, so `--amend` would rewrite *previous* commit.
- **Never `Co-Authored-By`** in commit messages. OCX convention.

## Two-Phase Model

Branch commit history go through two phases. Each phase: different goal, different skill, different rules.

| Phase | Skill | Goal | Rule |
|---|---|---|---|
| **Working** (default on worktree branches) | `/commit` | Save progress while iterating. Bundle freely. Amend rolling Checkpoints. | One concern per commit **relaxed**. Honest bundle message better than fake narrative. |
| **Rebasing** (explicit, before landing) | `/finalize` | Produce exact commits that appear in changelog | Strict Conventional Commits v1.0.0. One concern per commit. Reword/squash/split as needed. |

Default posture on four worktree branches (`goat`, `evelynn`, `sion`, `soraka`): **working phase**. Do not badger user about splitting during working phase — they clean up with `/finalize` before landing.

## Checkpoint Convention

Commit with subject exactly `Checkpoint` (no type, no body) means "rolling WIP". Amended every time new work lands on top. Never goes to `main`. `/finalize` refuses to land branch that still contains one.

`task checkpoint` creates or amends rolling Checkpoint automatically.

## Conventional Commits (Quick Rules)

Full cheat sheet: [`commit_reference.md`](../skills/commit/commit_reference.md) (types, scopes, footers, breaking changes, worked examples).

- Format: `<type>[optional scope]: <description>`
- Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `chore`
- **`chore:`** for AI/tooling files (`.claude/`, `CLAUDE.md`, skills, rules, hooks, taskfiles) — keeps out of user-facing changelog
- Imperative mood, lowercase description, no trailing period, subject ≤72 chars
- Body explains **why**, not what. Only when non-obvious.
- Breaking changes: `!` before colon **and** `BREAKING CHANGE:` footer

## Land-Ready Definition

Branch ready to fast-forward onto `main` when **all** hold:

- [ ] Rebased on top of current `main` (no merge commits in `main..HEAD`)
- [ ] Every commit in `main..HEAD` has Conventional Commits subject
- [ ] No `Checkpoint` commits remain
- [ ] No "bundle" commits mixing unrelated concerns (working-phase bundles must split or squash)
- [ ] Each commit one concern
- [ ] `task verify` passes on final state

`/finalize` checks each and proposes rebase plan for anything that fails.

## Quality Gate

Every commit on branch must pass `task verify` before landing on `main`. `pre_commit_verification.py` hook enforces on tip commit. When hook blocks:

1. Run `task verify` (never bypass with `--no-verify`).
2. Mark verification state (separate `Bash` call — combining with commit in one `&&` chain does not satisfy hook):
   ```sh
   echo $(date +%s) > .claude/hooks/.state/commit-verified
   ```
3. Retry commit.

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

Code in `external/` (e.g., `rust-oci-client`) is fork of upstream repo. Three rules:

1. **Upstream-first**: Make changes upstream-compliant. After change works locally, plan upstream PR.
2. **Format only new code**: Do NOT run `rustfmt`/`cargo fmt` on entire file — only format lines you introduced. Upstream may use different style (e.g., 100-char width vs OCX 120-char). Reformatting bloats diffs and blocks upstream PRs.
3. **No `Co-Authored-By`**: Submodule commits must not have `Co-Authored-By` trailers (upstream convention).

## References

- [`commit_reference.md`](../skills/commit/commit_reference.md) — Conventional Commits v1.0.0 cheat sheet
- [workflow-feature.md](./workflow-feature.md) — where commits fit in broader feature flow
- [workflow-release.md](./workflow-release.md) — release-time branch handling
- `CLAUDE.md` — worktree layout, "Landing a feature" section