# ADR: Rebuild a Diverged Announce Branch on the Index Base

<!--
Architecture Decision Record — design only. Owner: Architect. Handoff: Builder.
Scope: the `Diverged` + open-pull-request arm of `resolve_branch_state`
(`crates/ocx_lib/src/announce.rs`). Reverses half of the #228 decision.
-->

## Metadata

**Status:** Accepted (owner approved the plan 2026-09-04)
**Date:** 2026-09-04
**Deciders:** Michael Herwig (owner) + Claude design swarm
**Related PRD:** [#399](https://github.com/ocx-sh/ocx/issues/399)
**Tech Strategy Alignment:**
- [x] Golden Path — Rust/Tokio, existing forge REST plumbing, no new dependency, no new ref primitive
**Domain Tags:** integration | data | api
**Supersedes:** N/A — amends C4/C6 of [`design_spec_announce_initiative.md`](./design_spec_announce_initiative.md)

## Context

Two mirror-image failure modes meet on one per-package announce branch.

[#228](https://github.com/ocx-sh/ocx/issues/228): the branch name derives from the package
alone, so it outlives every pull request opened from it. Accumulating onto a squash-merged
branch re-proposed commits the index already had, and the pull request conflicted on the
root file every announce edits. Fixed by
[`60c8b391`](https://github.com/ocx-sh/ocx/commit/60c8b391) — `resolve_branch_state` asks
ancestry first, and `Diverged` *without* an open pull request classifies `Spent`: rebuild
on the base, repoint the ref with `RefUpdate::Reset`.

[#399](https://github.com/ocx-sh/ocx/issues/399): the same classifier reads `Diverged`
*with* an open pull request as `Live`. `read_committed_root` then takes the root from the
branch head instead of index main; `pipeline::regenerate` clones that root and rewrites
only `tags`, `desc` and `variants`, carrying every other key verbatim; and the C6 check
byte-compares the result against the branch's own bytes. So when the base migrates the
root's shape ([ocx-sh/index#740](https://github.com/ocx-sh/index/pull/740), an `owners[]`
respelling), every later run reproduces the branch's pre-migration bytes, reports
`unchanged`, commits nothing, and the pull request stays CONFLICTING
([ocx-sh/index#648](https://github.com/ocx-sh/index/pull/648) is one of them). 34 packages
froze for up to 21 days, reported as success, and it recurs on any index-wide root change.

#228 says *branch content already merged — must not re-propose*. #399 says *branch content
unmerged but the base moved — must not read the branch as truth*. One classifier holds both.

## Decision Drivers

- No tag announced into an open pull request may be lost, in any of the four tag modes.
- `--tags-file` and `--refresh` start from the *committed* tags, so the obvious fix deletes.
- The freeze is self-reinforcing and reports as a benign `unchanged` — it must become loud.
- No new ref primitive, no new dependency, no polling.

## Considered Options

| # | Option | Verdict |
|---|---|---|
| a | Naive `Diverged → Spent` | Rejected — drops tags under `--tags-file`/`--refresh`, regressing the #228 guard |
| b | Gate the reset on tag mode (reset only under `--tags`/`--tags-from-registry`) | Rejected — freezes exactly the publishers on the documented `push --tags-file` → `announce --tags-file` workflow |
| c | Mergeability-gated classification: accumulate when mergeable, rebase when conflicting | Rejected — a GET on every `Diverged` run, `Unknown` ambiguity in the hot path, and a mergeable-but-CI-red branch stays stale |
| d | **Always rebase `Diverged` + open PR, carrying the branch's tags, plus a mergeability tripwire** | **Chosen** |

## Decision Outcome

### D1 — `BranchState::Stale` for `Diverged` + open pull request

A new variant replacing `Live` on that arm. `is_live()` is false, so the root's *shape* is
read from index main and the commit bases there; `ref_update()` returns the existing
`RefUpdate::Reset` — `Spent` already uses it, and GitLab's reset path is byte-identical.

The branch head's root is read once more and its **tag delta carried**: base order first,
branch-only keys appended in branch order, base entry wins on a shared key. That tie-break
is **yank governance**, not refresh — `regenerate` overwrites `content` and `observed` on
any digest move, so it only ever decides `yanked`. The base's yank is merged, CI-validated
state a stale branch must not revert; an unmerged branch-side yank costs one `--yank` re-run.

C6 still compares against the **base's raw bytes** while the regeneration input is the
merged root. Comparing against the merged root would make `--refresh` on a stale branch
read `unchanged` again — #399 in a new place.

The `NonFastForward` retry re-reads the **base**, not the branch head, whenever the first
attempt used `Reset`, re-applies the carried tags, and retries with `Reset`. Re-reading the
branch head there turns the rebuild back into accumulate-on-stale. The result is one commit
on the current base carrying old plus new tags: the pull request is mergeable by
construction, and no tag is lost in any of the four tag modes.

### D2 — mergeability is a detector, never the fix

`Forge::pull_request_mergeability(index_repo, number) -> Mergeability {Mergeable,
Conflicting, Unknown}`, consulted at exactly one call site: the unchanged path under
`Stale`. Every other outcome commits with `Reset` and fixes the conflict by construction.
`Conflicting` raises `AnnounceError::PullRequestUnmergeable`, exit `DataError` (65) per C13
with `DescDisappeared` as the precedent — nothing is malformed, the two sides disagree, and
only a human clears it; `TempFail` would invite a retry that can never succeed. `Unknown` is
benign: no polling, the next run re-checks.

### The invariant, restated

**No tag announced into an open pull request is ever lost.** That is what #228 protected.
The wording it acquired in code — *the branch's commit chain is never rewritten* — was a
proxy for it, and #399 is the case where proxy and invariant point in opposite directions.

So `test_announce_keeps_accumulating_while_its_pull_request_is_open` flips its parent
assertion from the branch's first head to the index main head **on purpose**, keeps its tag
assertion unchanged, and is renamed to say what it now guards. That is not a weakened
regression test: the assertion carrying the invariant — both tags present in one pull
request — is untouched; only the one encoding the old mechanism moves to the new one.

### Consequences and accepted ceilings

- **Union, not 3-way.** A tag removed on the base by a `--tags` replace while a stale branch
  still carries it is re-proposed. Upgrade: 3-way against the compare API's merge base.
- **`--tags` keeps replace semantics on a `Stale` branch.** A branch-only tag the flag
  omits drops, exactly as on a `Live` branch today (pinned by
  `test_announce_tags_replace_drops_omitted_committed_tag`); when such a run is
  otherwise unchanged, the open pull request still proposes the omitted tag and only
  the D2 tripwire guards that corner. Upgrade: union the carried tags into the explicit
  selection under `Stale`, or refuse a replace while a stale pull request is open.
- **`Reset` is not CAS** — the window `Spent` already has. Two simultaneous announces on one
  stale branch: last writer wins. Upgrade: GraphQL `updateRefs` with `beforeOid`.
- **`Unknown` mergeability is benign** — a fetch during GitHub's computation may report
  unchanged once. No polling by design.
- **`Stale` + unchanged + `Conflicting` is a hard error the tool cannot clear** — it has no
  close-pull-request or delete-ref primitive. Nothing is lost there (every tag is already on
  the base), only the pull request is stuck. Upgrade: those two trait methods.
- **Every `Diverged` + open-PR run rewrites the branch into one commit.** Acceptable: the
  index squash-merges anyway, and C4's "sequential announces accumulate into one pull
  request" still holds at the tag level, which is the level the invariant names.

## Links

- [#399](https://github.com/ocx-sh/ocx/issues/399) (this defect) · [#228](https://github.com/ocx-sh/ocx/issues/228) (its mirror image) · [`60c8b391`](https://github.com/ocx-sh/ocx/commit/60c8b391) (the #228 fix, half-reversed here)
- [ocx-sh/index#740](https://github.com/ocx-sh/index/pull/740) — the base migration that triggered the freeze · [ocx-sh/index#648](https://github.com/ocx-sh/index/pull/648) — one pull request frozen by it
- [`design_spec_announce_initiative.md`](./design_spec_announce_initiative.md) (C4/C6, amended here) · [`adr_announce_publisher_surface.md`](./adr_announce_publisher_surface.md) (the surface this sits inside)

**Changelog:** 2026-09-04, Claude (architect) — initial record: D1 `Stale` rebuild with tag
carry, D2 mergeability tripwire.
