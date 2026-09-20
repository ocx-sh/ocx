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

Every commit on branch must pass `task verify` before landing on `main`. Git enforces it itself: `.githooks/commit-msg` runs `scripts/commit_gate.py`, wired by `core.hooksPath` (armed by `task` and `task verify`), so every commit is gated however it was spelled and the index survives a refusal. When it blocks:

1. Run `task verify` or `task verify:scoped` (never bypass with `--no-verify`). Both write the verify mark themselves.
2. Or mark the tree deliberately — `task verify:mark` is an **escape hatch, and using it is allowed**:
   ```sh
   task verify:mark
   ```
   It exists so a cheaper tier can be run on purpose and the expensive one deferred: unit tests green
   and the acceptance suite left for later, intermediate commits on a feature branch kept fast, the
   full suite run once before the branch lands rather than at every commit. Same hatch when only
   cherry-pick / rebase merge context changed since a passing verify. Say in the commit body what was
   deferred. The manual mark is `scoped` and never satisfies a `release:` commit or a commit on
   `main` — those need `task verify`, which is what makes the deferral end at the branch boundary. A bare `echo $(date +%s) > …/commit-verified` reads as *not verified* and overwrites the JSON mark a verify just wrote. A mark also certifies exactly one HEAD **and one working tree**: a sibling agent worktree at the same HEAD has to earn its own.
3. Retry commit. Staging survives a refusal — the commit was aborted, not the `git add`.

Carve-outs, all deliberate: a rebase in progress commits ungated (git rewrites HEAD through states no mark can certify); a merge, cherry-pick or revert handed back to you keeps only the *subject* carve-out — the subject git prepared commits as it stands (`git commit --no-edit`), a hand-written one must be conventional, and the mark is required either way, so mark before you merge (`task verify:mark` after a passing verify, or a scoped run) rather than after the refusal; and the subject `Checkpoint` commits without a mark so `task checkpoint` keeps working.

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