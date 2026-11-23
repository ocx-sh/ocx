# Goal: Package export and import as an index directory plus a blob directory, with blob directories and an opt-in cache

Source: .claude/artifacts/adr_package_export_import.md · Written: 2026-11-23 by /hex-loop

## Definition of done

- [x] ADR and system design reworked to the discussion's Track 1 block — evidence: commits e57e5db6, 86bef70b,
  26b2a638 (`.claude/artifacts/adr_package_export_import.md`, `.claude/artifacts/system_design_package_export_import.md`;
  /hex-architect at tier xhigh, one review round, no `[NEEDS CLARIFICATION]` left).
- [x] Export writes index/ and blobs/ in the home layouts; no --platform means every platform — evidence: rows V1 and V14 of `.claude/artifacts/evidence_package_export_import.md`
  (`test_export_platform_filter`), green in the integration gate at fec09779.
- [x] Blob directories and the opt-in cache directory: config, env and flag, one lookup chain — evidence: evidence rows V12 and V13 and
  `test/tests/test_blob_cache.py`, green in the integration gate at fec09779.
- [x] Import updates the home index and assembles packages; tools then run offline without the export — evidence: evidence rows V3
  and V8–V10, `test_import_roots_nothing`, green in the integration gate at fec09779.
- [x] Registries stay apart, the index never chains, the home keeps no layer archives — evidence: evidence rows V7 and V11, green in the
  integration gate at fec09779.
- [x] Every Track 1 acceptance test in the discussion passes, each shown red once — evidence: `.claude/artifacts/evidence_package_export_import.md`,
  rows V1–V18 each with a red line and a green run (V5 withdrawn with the owner's reversal), commits 83a287dc and 9248beb3;
  `task verify:scoped --force` exit 0 at fec09779 (scoped acceptance 2254 passed, 44 skipped, 1 xfailed).
- [x] User docs and the interface contract cover the new commands, flags and variables — evidence: guide page 53772194, generated
  references and goldens a0bedba1; `task website:build`, `task website:check` and `task website:reference:check` exit 0 at
  fec09779; report roots additive against the v0.6.8 baseline (no ledger entry).
- [ ] PR merge-ready (DONE block only) — evidence: the PR URL, and the
  PR is not a draft.
- [ ] CI green per job (DONE block only) — evidence: every check run on
  the PR head SHA with its conclusion; every skipped job states its
  skip reason (its `if:` or path filter).
  A skip with no reason is not green.
- [ ] Deep verify passed (DONE block only) — evidence: the full documented verification result.

## Autonomy

- Prompting: never — no question waits for a human; a doubt runs
  § Issue resolution.
- Granted acts (authority: the pasted prompt): none
- Forbidden or narrowed acts: None.

## Issue resolution

Every doubt — an ambiguous requirement, a design question, a failure
with an unclear cause — runs this protocol:

1. Delegate the research to a sub-orchestrator.
2. Record question → research → decision in this file or in the PR.
3. Defer to a GitHub issue only in hard cases — the research ends with no
   decision, or the decision needs an act outside the grants; the issue
   link is the recorded decision.
4. Every doubt not deferred ends in an action.
5. A pre-existing failure that blocks done is in scope and runs this
   protocol.
6. Findings outside this goal's scope become follow-up issues.
7. Secrets and credential-bearing logs are never written to any committed
   file, commit message, PR text, PR comment or review comment, or issue.
   Security findings are never filed as issues:
   this file records them by reference only — location and class, no
   secret value or exploit detail — and only the DONE block reports them
   in full.

## Loop shape

- Entry point: Run the /hex-architect skill on .agents/discussions/bazel-hermetic-package-bundles.md, then the /hex-plan skill on its design.
- Refinement rounds: 2 — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a red
  gate's fix pass, an execute or finalize retry, a post-finalize CI fix ⇄
  re-finalize pass. Inner review-fix rounds do not count, and their limit
  is untouched.
  Past `2`, the DONE block reports every remaining criterion `not met`
  and the run stops.
- Ticks: /hex-loop commits nothing. The session creates or switches to
  the branch the pasted prompt's I9 names and commits this file first on
  it. A box is ticked only between hex-mode runs, never during one, and
  each tick is committed at once; every tick lands before the final
  /hex-finalize, none after it. The `(DONE block only)` criteria are
  evidenced only in the closing DONE block, never ticked.

## Rules

- Run rules:
  - The loop starts with /hex-architect reworking the ADR and system design from the discussion notes, then plans, builds, reviews and finalizes export/import only.
  - Done criteria come from the notes' current-state block and acceptance tests, not from the stale ADR decisions.
  - The run picks the flag, config and environment names.
  - /hex-architect runs at tier high or above (ratified at the restate on 2026-11-23).
  - Out of scope: Track 2 of the discussion (rules_ocx under remote execution); it follows after this goal lands.

## Emphasis

- The source ADR is stale where the discussion reversed it. Its format decision (the OCI image layout, `sh.ocx.bundle.version`, `LayoutTransport`) and the `fetch_plan` report are withdrawn. The block "Track 1 as it stands on 2026-11-22" in the discussion is the current design; where the two disagree, the discussion wins.
- The export format and the settings are interfaces: the directory layout is the home's own `index/` and `blobs/` layouts, and the new flags, config keys and environment variables are CLI contract. The word "bundle" is ruled out for this feature.
- Five checks are carried under the discussion's Open questions and belong to the ADR rework: the index source for import, a valid catalog for a partial export, `ocx package copy` from a blob directory, `PathList` splitting on Windows, and the names.
- One acceptance test (project, `ocx clean`, then `ocx exec` offline) depends on ocx-sh/ocx#759. That issue is separate from this goal; if it blocks done, § Issue resolution step 5 applies.
- Cache eviction and the shared-volume guarantee are ocx-sh/ocx#760 and not part of this goal. Concurrent use of one cache directory must still be safe in this release.

## Context

- Discussion (ratified 2026-11-23, `State: handed-off → architect`): `.agents/discussions/bazel-hermetic-package-bundles.md` — read "Track 1 as it stands on 2026-11-22", `## Files and interfaces`, `## Open questions` and `## Verification`.
- ADR and system design to rework: `.claude/artifacts/adr_package_export_import.md`, `.claude/artifacts/system_design_package_export_import.md`.
- Related ADR: `.claude/artifacts/adr_servable_index_snapshot.md` (a local index directory as a `file://` source).
- Research: `.agents/research/research_local-sources-cache-config-prior-art.md`, `.agents/research/research_ocx-config-and-blob-seams.md`, `.agents/research/research_bundle-rules-ocx-current-state.md`.
- Issues: https://github.com/ocx-sh/ocx/issues/759, https://github.com/ocx-sh/ocx/issues/760, https://github.com/ocx-sh/ocx/issues/23.

## Decision log

- 2026-11-23 · /hex-architect ran at tier xhigh (the classifier called the decision large); the meta-plan and plan gates
  passed under § Autonomy. Design calls beyond the discussion, each reasoned in the ADR: the root marker
  `ocx-export.json` (format version and registry map, no contents list); an import with no names only adds pins and
  reports a differing pin as `conflict`; names `--blob-dir` / `OCX_BLOB_DIR` / `[blobs] dirs` and `--blob-cache-dir` /
  `OCX_BLOB_CACHE_DIR` / `[blobs] cache_dir`; root `ocx export` / `ocx import` beside `ocx package export` / `import`.
- 2026-11-23 · /hex-plan: `.claude/artifacts/plan_package_export_import.md` at `State: plan-approved` (commit 12d10e42),
  25 pipelines in 8 waves, 17 recorded deviations from the system design.
- 2026-11-23 · Security finding by reference (plan finding F17): location `crates/ocx_package` environment validation and
  the compose path in `crates/ocx_package_manager`; class: reserved tool-control variables accepted from untrusted
  package metadata on the read path. Pre-existing, and the new cache variable widens its effect. Decision: in scope
  (§ Issue resolution step 5 reasoning, I4 proportionate security) — fixed in this run as its own regression-tested
  `fix:` commit; no issue is filed (step 7). Full report in the DONE block only.
- 2026-11-23 · /hex-execute pass 1: every pipeline, the red and green proofs and review round 1 (verdict in the plan,
  reviewed at d23fd6e1) landed; the round-1 fix pass merged through 60509659. The session then restarted during the
  integration-gate reruns and the sub-orchestrator was lost with a clean tree and no worktrees left. Decision: a fresh
  sub-orchestrator resumes the same pass from the plan's Status block. Counting (§ Loop shape): review round 1 and its
  fix pass are outer cycle 1; the resume itself is not a failed repair or retry and spends no cycle; a delta review with
  a fix pass after it would be outer cycle 2, the last.
- 2026-11-23 · /hex-execute reached `State: review` at 9248beb3: integration gate green at fec09779
  (`task verify:scoped --force` exit 0 — the scoped arm, not the full `task verify`; cross-build
  `x86_64-pc-windows-gnullvm`, satellite check, website build and checks exit 0). Review calls spent: 2 (round 1 at
  d23fd6e1, delta round 2 at 1a10802f: 0 Block, 14 Warn, all fixed). Outer cycles spent: 2 of 2. Fixes inside the run:
  ocx-sh/ocx#759 (bcd1df96) and the reserved-variable read-path finding (5ac62f44).
- 2026-11-23 · Security finding by reference (plan § Deferred findings, round 2): location
  `crates/ocx_oci` blob-directory stream reads and the file index transport; class: a symbolic link at the final path
  component is followed on operator-configured inputs. The untrusted path (an imported export) refuses links. Decision:
  accepted for this release, not filed (§ Issue resolution step 7); full report in the DONE block only.
- 2026-11-23 · Out-of-scope findings become follow-up issues after /hex-finalize opens the PR: an export whose marker
  registry slug is `locks` is refused by import; the Bazel website render omits `dist/schemas/index.html` (pre-existing).
- 2026-11-23 · Human act needed, no grant covers it: `.agents/worktrees/ctx` holds root-owned directories left by a
  Docker run; removing them needs sudo.
- 2026-11-23 · /hex-finalize pre-flight decisions. (1) Target `main`, from the discovered trunk: no pull request
  exists and the invocation's argument was the plan path, which is not a branch and is ignored. (2) Halt 2 ("not the
  primary checkout"): this checkout is a linked worktree, but it is the session's own working checkout and the branch
  was opened here, as I9 instructs; agent worktrees are the `.agents/worktrees/` trees. Precedent: the inert refs
  `backup/uno-*` show earlier finalize runs in this same checkout. Decision: the halt does not apply. (3) Halt 3: the
  only dirty path is `.serena/project.yml`, rewritten by a local tool and not part of this work. Decision: it is
  hidden with `git update-index --skip-worktree` for the run and never staged; the recompose stages by tree, not by
  working directory. (4) Series shape: the documented convention (`workflow-git.md`: one concern per commit, strict
  Conventional Commits) outranks the squash-to-one hint, so the series is two `fix:` commits (ocx-sh/ocx#759 and the
  reserved-variable read path), one `feat:` commit and one `chore:` commit for planning artifacts and rules.
  (5) Remote gate: `hex.md` documents no dispatchable release-grade workflow (`verify-deep.yml` is never dispatched
  by a hex skill); the pull request's own CI is the remote gate and the evidence for "Deep verify passed".
