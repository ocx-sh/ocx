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
- **Never `--no-verify`, `--no-gpg-sign`, or any hook-skipping flag.** Hook fail → fix root cause, new commit — hook fail means commit did not happen, so `--amend` would rewrite *previous* commit. One exception: inside a hex run, step commits and merges on hex-owned branches may use `--no-verify` (CLAUDE.md principle 2).
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
2. Or mark the tree — `task verify:mark` is the **unconditional escape hatch**, always allowed on a branch, merge commits included:
   ```sh
   task verify:mark
   ```
   No justification is checked, no tree is pinned; it writes a scoped mark at HEAD with a 5-minute TTL. Say in the commit body what was deferred. Only a `release:` commit and a commit on `main` need `task verify` — that is where deferral ends. A bare `echo $(date +%s) > …/commit-verified` reads as *not verified*. A mark certifies one HEAD **and one working tree**: a sibling agent worktree at the same HEAD marks its own.
3. Retry commit. Staging survives a refusal — the commit was aborted, not the `git add`.

Carve-outs, all deliberate: a rebase in progress commits ungated (git rewrites HEAD through states no mark can certify); a cherry-pick, revert or merge handed back to you keeps only the *subject* carve-out — the subject git prepared commits as it stands (`git commit --no-edit`), a hand-written one must be conventional, and a mark is required either way; and the subject `Checkpoint` commits without a mark so `task checkpoint` keeps working.

### Verification Levels

Pick the level the change needs; the agent decides, nobody audits the choice. The inner loop is for speed — cleanup and hardening happen at `/hex-finalize`, which always runs the full `task verify`.

| Change | Level before committing |
|---|---|
| Docs, comments, AI config, plan artifacts | none — `task verify:mark` |
| Several small commits in a row | verify once after the batch; `task verify:mark` on the commits before it |
| Code in one or a few crates | the crate's tests, or `task verify:scoped --force`, then commit (it marks itself) |
| `verify:scoped` escalates (taskfile, BUILD/bzl, `scripts/**`, workflows) but the edit is small | run what the edit touches, then `task verify:mark` |
| A work-package merge | `git merge`, then `task verify:mark` or a scoped run — no full verify per merge |
| Finalize, `release:`, `main` | full `task verify` (mechanically required) |

Full `task verify` otherwise only when you want the signal — never as a ritual.

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