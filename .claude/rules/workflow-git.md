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

Every commit on branch must pass `task verify` before landing on `main`. Git enforces it itself: the `commit-msg` hook runs `scripts/commit_gate.py` — a prek hook declared in `.pre-commit-config.yaml`, installed into git's hooks directory by `task git:hooks` (armed by `task` and `task verify`) — so every commit is gated however it was spelled and the index survives a refusal. The push gate is `scripts/pre-push.sh`, copied into the same directory. When it blocks:

1. Run `task verify` or `task verify:scoped --force` (never bypass with `--no-verify`; `--force` is the documented spelling — without it, a `sources:`-cached sub-task can skip while the mark still gets written). Both write the verify mark themselves.
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

Carve-outs, all deliberate: a rebase in progress commits ungated (git rewrites HEAD through states no mark can certify); a cherry-pick or revert handed back to you keeps only the *subject* carve-out — the subject git prepared commits as it stands (`git commit --no-edit`), a hand-written one must be conventional, and the mark is required either way; and the subject `Checkpoint` commits without a mark so `task checkpoint` keeps working. A merge is stricter, not looser — see "Work-Package Merges" below; a scoped mark never satisfies it.

### Work-Package Merges

While `MERGE_HEAD` exists — a work-package merge concluded after `git merge --no-ff --no-commit`, or a plain `git merge` committing itself — the commit gate accepts only a **full** mark whose `tree` equals `git write-tree` of the index being committed (plan_test_speed_tiers.md C-017, ADR D4). A scoped mark, or a full mark from a different tree, refuses the commit; the recipe it prints on refusal is the one below. Checked before the `Checkpoint`-subject carve-out, so a merge cannot conclude unmarked by committing with subject `Checkpoint`.

**The recipe:**

```sh
git merge --no-ff --no-commit <branch>
task verify           # writes the full mark over the merged (pre-commit) tree
git commit             # the merge commit; MERGE_MSG's subject is exempt from Conventional Commits
```

`task verify` itself pre-checks a clean working tree at its start whenever `MERGE_HEAD` exists (a working tree that differs from the index is refused immediately, not after the whole suite) and snapshots the merged tree so `.verify:mark` refuses one restaged mid-run (AM-8). Both checks exist because the mark certifies the tree being committed, not whatever the working tree happens to hold when the mark step runs.

**Named residuals** — outside the clause for two different reasons, so none is mechanically enforced and none is attributed as a C-ESC escape. Three never leave `MERGE_HEAD` for the gate to see at all:

- a **fast-forward merge** (no commit at all — nothing to mark);
- `git commit --amend` of a merge that has **already landed** (judged by `commit_gate.py` rules 3–5, like any other commit, not by the merge clause);
- `git merge --squash` + `git commit` (git never sets `MERGE_HEAD` for a squash merge, so this is an ordinary commit to the gate — the ordinary rules apply, not the merge clause).

Two others run no `commit-msg` hook at all, so `MERGE_HEAD` may exist but no rule ever reads it:

- `git merge --no-verify` / `git pull --no-verify` (skips `commit-msg` outright — never used: see "Never `--no-verify`" above);
- a **clean** `git cherry-pick` that applies without conflict (git writes the picked commit without running `commit-msg` at all — measured on git 2.54 — so no rule sees it; the flag-free form cannot be denied without denying every clean cherry-pick). A cherry-pick or revert that **conflicts** and is resumed with `--continue` is a different case — the hook does run there; see the handback carve-out above (mark still required, only the subject is exempt).

The finalize/main full mark is the backstop for all five: `task verify` on the feature branch tip before `/hex-finalize` and on `main` after landing still proves the merged tree, even where the per-merge clause could not see it.

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