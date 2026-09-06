# Research: index claim command — repo archaeology

## Metadata

- Date: 2026-09-04
- Expires: 2027-03-04 (re-verify: GitLab job-token scope and push-option availability move per minor release)
- Author: hex-discuss repo-archaeology lane (researcher, sonnet); persisted by the orchestrator — the worker had no write/bash tool, so history was reconstructed from reflogs, ADR changelog tables, and in-code comments rather than `git log`
- Discussion: `.agents/discussions/index-claim-command.md`
- Repos + HEAD: ocx-sion (`sion`, `487570fb`) · ocx-mirror (`hex/mirror-signing`, `41c4b3fd`) · ocx-indexbot (`main`, `1b1c83e6`) · index (`fix/upstream-renames`, `9eba2cc3`)

## Direct answer

No prior "claim" command or auto-claim mechanism exists anywhere in `ocx`, `ocx-mirror`, `ocx-indexbot`, or the `index` repo — zero commits touching "claim" as a feature across main + 9 other worktree reflogs. `ocx package announce` refuses an unclaimed namespace (`AnnounceError::UnclaimedNamespace`, `require_root` in `announce/pipeline.rs:123`) by design, deferring to a human-filed PR — reference-parity behavior since the initial 2026-07-22 design register, never a reverted auto-claim. `ocx index *` never touches forges (Catalog/List/Update/Sync/Regenerate are local-cache ops); `announce` lives under `package` because it is a per-package publisher write action, originally specced as `ocx package push --announce` sugar (design register D4 in `adr_public_index_registry_indirection.md`). The `Forge` trait was extracted for the GitHub+GitLab work (`adr_announce_gitlab_forge.md`, accepted 2026-08-22); self-hosted forge kind is declared via `--forge`, never probed (D3) — up-front risk analysis, not a reverted probe. ocx#399's diverged-branch fix (`6feb9c02`, 2026-09-04) reclassifies a `Diverged`-branch-with-open-PR as `Stale`, rebuilds on the index base and repoints with `RefUpdate::Reset` — the sanctioned "spent branch" semantics any git-push transport must reproduce. GitLab CI job tokens are documented as incapable of driving announce over REST (`forge/gitlab.rs:148-157`).

## Key findings

### 1. Why `ocx package announce`, not `ocx index *`

- `crates/ocx_cli/src/command/index.rs` has exactly 5 subcommands — `Catalog`, `List`, `Update`, `Sync`, `Regenerate` — all consumer-side local-index-cache operations; no history of an added-then-removed forge-touching variant.
- `adr_public_index_registry_indirection.md:119-123` (D4) specced a transport ladder: v0 = human-edited entry + PR; v1 = `repository_dispatch`/`workflow_dispatch` doorbell; v2 = `ocx package push --announce` sugar — announce was conceived as a `push`-adjacent per-package publisher action.
- `design_spec_announce_initiative.md` §2 C14: "Layering: orchestration in `ocx_lib` (CLI thin wrapper …). New CLI file `package_announce.rs`." No artifact debates `ocx index announce`; `subsystem-cli.md`'s taxonomy fixes the split: `ocx package <verb>` = per-package, identifier-driven, never touches `ocx.toml`.
- `crates/ocx_lib/src/announce.rs:1-35` module doc confirms `adr_announce_publisher_surface.md` D5 — orchestration in `ocx_lib`, CLI thin.

### 2. Prior "claim" attempts — none; `require_root` origin

- `git log --all -i --grep=claim` equivalent across every reachable worktree reflog (`fork`, `wp7`, `wp8`, `integration1`, `p10-record`, `ocx-soraka`, `ocx-evelynn`, `winleg`, `shell-env`, `fx-wpSEC`, bare `ocx`) — zero matches for a claim feature/command.
- `require_root` (`announce/pipeline.rs:116-134`): `None` read at `base_ref` → `UnclaimedNamespace`. Doc comment: "an unclaimed namespace that must go through the human lane (design register C10, reference parity)."
- `announce/error.rs:290-294` dates it to a live E2E failure (publisher E2E run 30133426034): "announcing into a namespace with no committed root is the likeliest first-run outcome for a new publisher, and register R3 makes claiming it a one-time human action."
- Auto-claim was foreclosed by policy (index repo ADR-6; `design_spec_announce_initiative.md` §7: "First claim goes through the real human lane (G-04, self-review formality)"), never built and reverted. `claim-a-namespace.md`: "New-package PRs are never auto-merged, no matter how green the automated checks are."

### 3. The `Forge` trait

- `crates/ocx_lib/src/forge/api.rs` declares 11 async methods; the trait doc still says "ten operations" — `pull_request_mergeability` was added for the #399 fix without updating the count (doc drift, not a design gap).
- Extracted by `adr_announce_gitlab_forge.md` (2026-08-22); before it, `announce.rs` called a concrete `GitHubForge` (v1 per `adr_announce_publisher_surface.md` D2, GitHub-only behind a neutral surface, 2026-07-19).
- D3 (declare, never probe) was a first-cut decision: `ForgeKind::from_host` (`forge/kind.rs:23-42`) recognises `github.com`/`gitlab.com`/no-host only. Rationale: "No unauthenticated request distinguishes the forges reliably… a wrong guess sends the announce credential to the wrong API in the wrong header." No probing implementation was ever built.
- Round-two adversarial review fixed a real regression in the declaration mechanism (host-mismatch guard compared two `Option<String>` hosts without resolving `None` → canonical) via `ForgeKind::same_host`.

### 4. ocx#399 diverged-branch fix

- `adr_announce_diverged_branch_rebuild.md` (accepted 2026-09-04) + `6feb9c02`. A `Diverged` branch with an open PR is reclassified `BranchState::Stale`: root shape read from index main, branch's tag delta carried forward (base wins on a shared key — yank governance), ref repointed via `RefUpdate::Reset`. New `Forge::pull_request_mergeability` consulted only on the unchanged-path tripwire; `Conflicting` → `AnnounceError::PullRequestUnmergeable` (exit 65).
- Invariant is "no tag announced into an open pull request is ever lost", not "the branch's commit chain is never rewritten" — force-with-lease / `Reset` rewriting a spent branch is the sanctioned mechanism. The ADR rejects naive `git push --force` (the donor `grim` does that and "silently discards a concurrent announce").
- Accepted ceilings: `Reset` is not a true CAS (last writer wins in the `Spent` window; upgrade path GraphQL `updateRefs` with `beforeOid`); `--tags` replace semantics still drop an omitted branch-only tag on a `Stale` branch; union-not-3-way against base tag removal.

### 5. Token/credential history

- `OCX_ANNOUNCE_TOKEN` was the name from day one (design register S7, 2026-07-19); no rename.
- GitHub App vs PAT: `design_spec_announce_initiative.md` S4 — "PAT-only day one. Machine account `ocx-bot`… GitHub App recorded as future scaling option ONLY, with hard constraint: never any permission on a publisher's source repo (BCR #157 / xz lesson)."
- GitLab job-token rejection: `forge/gitlab.rs:148-157` — "GitLab's job-token access table covers packages, releases, artifacts and environments, and lists none of repository files, commits, branches, merge requests or forking." The ADR changelog (third 2026-08-22 entry) records this as a late research correction before acceptance. This is the gap #411 routes around with git push.

### 6. ocx-mirror's shell-out

- `src/pipeline/ocx_cli/announce.rs`: four commands (`pipeline push|patch|cascade|announce`) call `ocx package announce` through one `build_announce_args`/`invoke_announce` boundary: `--format json package announce --package <pkg>`, then `--tags-file <path>` (additive) or `--tags-from-registry` (backlog), then `--out <dir>` or `--fork <fork> --index-repo <repo>`. Never `--tags` (replace).
- `src/spec/announce_config.rs`: `AnnounceConfig { package, fork, index_repo, schedule }` — **no `forge` field**. GitLab support landed in ocx core 2026-08-22 but was never plumbed through the mirror spec; a mirror publisher cannot announce to a GitLab-hosted index yet.
- ocx-mirror never claims namespaces itself.

### 7. Public index + indexbot — `owners[]`, rename, `format_version`, migration

- `adr_forge_neutral_owners.md` (accepted 2026-08-25, indexbot 0.5.0): D1 rename; D2 dual-emit, refuse disagreement; D3 deletes `indexbot announce` — `ocx package announce` is the only writer.
- `format_version` has never been bumped — `site/src/docs/reference/changelog.md`: "`format_version` 1 — 2026-07-17… No prior `format_version` was ever served to a real client." Owners rename, `source`, `superseded_by`, `variants` all landed additive under 1. `wire-format.md:73-76`: a client MUST treat an unrecognised higher `format_version` as a hard error requiring upgrade — a bump is reserved for a breaking wire change.
- `.github/maintainers.yml` got the same rename.
- `scripts/golden-baseline.sh` verifies (does not perform) the ~1.8k-root migration: hashes the normalised rendered site output and asserts byte-identical dist across the migration.
- Docs drift: `claim-a-namespace.md` still instructs `{github, github_id}`.

## negative:

- No commit, branch, or artifact implements or reverts an auto-claim-on-announce mechanism.
- `Forge` trait doc says "ten operations"; 11 declared.
- `ocx-mirror`'s `AnnounceConfig` has no `forge` field — "GitLab forge support" is not fleet-ready at the mirror layer.
- `claim-a-namespace.md` documents the legacy owner shape post-dating the accepted rename ADR.
- SHAs/dates sourced from reflogs, ADR changelog tables and in-code comments; not cross-verified against `git log`.

## leads:

- git-transport-for-announce (#411): reuse `RefUpdate::Reset`-with-carried-tags semantics from #399 rather than reinventing branch-staleness handling.
- forge-neutral owners docs sweep: `claim-a-namespace.md` and any claim-command help text use `login`/`id`.
- ocx-mirror forge parity: `AnnounceConfig` needs a `forge` field before a claim/announce is mirror-invokable against GitLab.
- `Forge` trait doc-count fix: cheap, unrelated to the design.
