# Goal: "rules_ocx under remote execution"

Source: .agents/discussions/bazel-rbe-rules-ocx.md · Written: 2026-11-26 by /hex-loop

## Definition of done

- [ ] "First-run criteria hold at final heads" — evidence: on the final head of each pull request, the acceptance jobs
  that run `test/tests/test_vendor.py` and `test/tests/test_package_vendor.py`, the six native legs of
  `.github/workflows/launcher-blobs.yml`, and the rules_ocx jobs `test`, `rig-buildbarn` and `rig-nativelink`, with
  rules_ocx pinned to the dev build of the final ocx head. The five criteria are listed under § Context, "First run".
- [ ] "Short vendored layout (issue 785)" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "One launcher and one record file everywhere" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Atomic launcher publish and refresh (issue 679)" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Runfiles manifest fallback" — evidence: the commit, PR comment or
  artifact that satisfies it, with the Windows CI job that starts a vendored tool without `--enable_runfiles`.
- [ ] "rules_ocx package locking (its issue 37)" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Red and green shown for every Verification check" — evidence: the commit, PR comment or
  artifact that satisfies it. Verification is the source's `## Verification` section plus every check this run adds.
- [ ] "The owner's rules hold in both repositories" — evidence: the commit, PR comment or
  artifact that satisfies it, and the last review reports no open Block or Warn.
- [ ] PR merge-ready (DONE block only) — evidence: the PR URL, and the
  PR is not a draft.
- [ ] CI green per job (DONE block only) — evidence: every check run on
  the PR head SHA with its conclusion; every skipped job states its
  skip reason (its `if:` or path filter).
  A skip with no reason is not green.
- [ ] Deep verify passed (DONE block only) — evidence: the run URL and conclusion of `verify-deep.yml` dispatched on the
  final branch head of this repository, and of the deep run of ocx-sh/rules_ocx on its final head.

## Autonomy

- Prompting: never — no question waits for a human; a doubt runs
  § Issue resolution.
- Granted acts (authority: the pasted prompt): In this repo: push the branch and update PR 784; comment on issues; dispatch release.yml with mode=dev and verify-deep.yml on that branch only; In ocx-sh/rules_ocx: commit, push and force-push with lease to hex/bazel-rbe-rules-ocx only; update PR 36; open and comment on issues; dispatch and rerun its CI; never merge, tag or release; Start and remove only the run's docker compose projects; /hex-finalize may skip its primary-checkout halt here; Delete the run's scratch dirs
- Forbidden or narrowed acts: Never write to the shared Bazel remote cache or the real ~/.ocx.

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

Recorded decisions, first run:

- Question: the entry reads "Run the /hex-architect skill on .agents/discussions/bazel-rbe-rules-ocx.md at tier high.", and `/hex-architect` refuses a discussion at `State: handed-off → loop` as path input (its step 1, with the fix "paste the decision as free text"). Research: `.claude/skills/hex-architect/SKILL.md`, the state table of its fast path. Decision: the run invokes the skill at tier `high` on the free text "rules_ocx under remote execution, per .agents/discussions/bazel-rbe-rules-ocx.md". The source and the tier are unchanged.
- Question: who writes to the git repository of `ocx-sh/rules_ocx` during execution. Research: the pasted prompt's I6 ("other-repo acts are yours alone") and the plan's "rules_ocx branch mechanics" and "External acts". Decision: the meta-orchestrator creates the one branch `hex/bazel-rbe-rules-ocx` and its worktree, and makes every commit and push there. Sub-orchestrators and workers edit files and run tests in that worktree and make no git write in that repository. The plan's detached per-pipeline worktrees are not used: the pipelines of one wave are file-disjoint and share the branch's worktree.
- Question: whether the run dispatches ocx `verify-deep.yml` and `build-windows-shims.yml`, which the plan lists as external acts 2 and 3. Research: the pasted grants name `release.yml` with `mode=dev` only; `.agents/memory/hex.md` records the owner's "No deep dispatch" preference. Decision: neither is dispatched by this run. `task satellite:verify` runs locally before finalize instead, and the signed shim blobs are reported as an owner step in the DONE block unless an existing pull-request check produces them.
- Question: the third `/hex-review` call (tier high, head `6718aacf`) returned Needs Work with 0 Block and 3 Warn, and both caps are spent (3 of 3 review calls, 2 of 2 refinement rounds). May the run apply the findings and finalize? Research: § Loop shape ("Past `2`, the DONE block reports every remaining criterion `not met` and the run stops"), `.claude/skills/hex-review/SKILL.md` (Needs Work hands to `/hex-execute`, only Approve hands to `/hex-finalize`). Decision: no third fix pass and no `/hex-finalize` on a branch that is not approved. The run finishes the verification already in flight, ticks what is evidenced, and prints DONE with the open Warns and the unmet criteria. The three Warns: `crates/ocx_shim/src/direct.rs:755-763` (the batch failure line has no test), `.github/workflows/build-windows-shims.yml:20-55` (`.cargo/config.toml` is hashed and sits in neither `paths:` list), `.claude/artifacts/system_design_vendor_rules_ocx.md:126` (the path rules lag the code).

Recorded decisions, second run:

- Question: the first run printed DONE with five criteria not met. How does the work continue? Research: the owner's instructions of 2026-11-26: "build our previously discussed locking and address https://github.com/ocx-sh/ocx/issues/785, then use up to two more cycle and then dispatch deep and then finalze"; "i would move the linux and macos launcher as well and solve #785 and #679"; "continue with 2 more review/execute loops after everything we now discussed has additonally be implemented ... reusing the existing prs". Decision: this file is updated for a second run on the same branch and the same two pull requests. The Definition of done is restated for the final heads. The five criteria the first run ticked are one open criterion again, because their evidence names heads that this run replaces; that evidence is kept under § Context. The refinement cap and the review-call limit count from zero.
- Question: the entry names this file, and `/hex-architect` takes a discussion or free text. Research: the first decision above. Decision: the run invokes the skill at tier `high` on the free text "launcher model, vendored layout and package locking, per .agents/goals/bazel-rbe-rules-ocx.md, section Context". It records or amends the decision records named there. `/hex-plan` then extends `.claude/artifacts/plan_vendor_rules_ocx.md`, and every later mode runs at tier `high`.
- Question: the third decision of the first run says no deep dispatch. Research: the owner's instruction above asks for one after the cycles. Decision: where the pasted prompt grants it, `verify-deep.yml` is dispatched on the branch after the last cycle and before `/hex-finalize`. A red run is fixed and dispatched again; a fix pass counts as one outer cycle.
- Question: `/hex-finalize` halt 2 refuses a linked worktree and halt 3 refuses a dirty tree. Research: the primary checkout stays on `main`; this session's checkout is a linked worktree that created the branch; agent worktrees are the `.agents/worktrees/` trees (`.agents/memory/hex.md`, Pointers); `.agents/goals/adr-package-export-import.md` and `.agents/goals/otel-telemetry.md` record the same decision; the one dirty path is `.serena/project.yml`, which a local tool rewrites. Decision: where the pasted prompt grants it, halt 2 does not apply to this checkout, and the finalize gate discloses that. `.serena/project.yml` is hidden with `git update-index --skip-worktree` for the finalize run, is never staged, and the flag is removed afterwards.
- Question: who finalizes the rules_ocx branch. Research: `/hex-finalize` halt 5 says a satellite's feature branch is finalized by hand. Decision: after the ocx finalize, the meta-orchestrator recomposes the rules_ocx series into Conventional Commits and force-pushes it with lease, where the pasted prompt grants that. Its pull request stays a draft, because it cannot merge before an `ocx` release with `ocx vendor`.
- Question: the architect phase covers two repositories. Research: the two parts share only the vendor manifest. Decision: two sub-orchestrators ran the tier-high phases in parallel, one per repository. The ocx record is `.claude/artifacts/adr_unified_launcher.md`; the rules_ocx record is `.agents/adr/adr_0003_package_locking.md` there. The rules_ocx record lists what it needs from the manifest (MR-1 to MR-14), and section 16 of the ocx record answers each item. The ocx record wins on what `ocx vendor` does.
- Question: the ocx record asked whether a newer `ocx` also writes `entrypoints/` for an older `ocx` that shares the home. Research: the record's section on mixed-version homes. Decision: no, as the record recommends. That launcher would need the old binary and its directory would carry no generation. An older `ocx` keeps every launcher it published.
- Question: the rules_ocx record asked how far a pin in `MODULE.bazel.lock` is trusted. Research: the record's security review. Decision: like a pin in `ocx.lock`, as the record recommends.
- Question: both records end at `Proposed`, and a plan needs a decided design. Research: the first run accepted its record under goal-loop authority (`.agents/memory/hex.md`, Memory). Decision: both are set to `Accepted` under goal-loop authority; the owner may revisit. Owner-revisitable choices to name in the DONE block: launchers in the home are copies, not hardlinks; a render deletes no old directory and only `ocx clean` retires them; `ocx vendor` keeps refusing a package whose declared binary is missing from its content.
- Question: `ocx vendor` refuses node 26.13.1. Research: a run in a scratch home by the architect sub-orchestrator; the package declares the binary `corepack` and its content lacks it. Decision: the refusal stays with a clearer message, and the data defect is deferred to https://github.com/ocx-sh/ocx/issues/787, because the package is published from another repository.
- Question: does the toolchain render switch `active` on Windows by retargeting the junction in place, or by today's remove-then-rename? Research: the job `junction-proof` of https://github.com/ocx-sh/ocx/actions/runs/45914869684 at `31cdcbd6`. Both tests (`junction_proof_retarget_under_lookup`, `junction_proof_retire_running_exe`) ran once and passed, as an administrator and as a standard local user. Decision: the in-place retarget ships (variant A of the plan's DP-41).
- Question: the rebuilt launcher for x86_64 Windows is 534016 bytes and the size budget is 512 KiB (524288 bytes). Shrink the launcher or raise the budget? Research: `.claude/artifacts/adr_shim_hermetic_zigbuild.md` records the earlier raise from 256 to 512 KiB as a decision and names a size lever (`build-std` with `panic_immediate_abort`); the other five binaries are 402096 to 496256 bytes; a shrink needs a toolchain change and a second rebuild round, and the plan keeps the one spare round for review fixes. Decision: the budget is raised to 640 KiB, recorded in that record. The owner may revisit; the DONE block names it.
- Question: the test that scans the Windows launcher binaries for the `ocx` argument words looked for the words with spaces around them. Research: `crates/ocx_shim/src/core.rs:110-115` builds the arguments as separate elements now. Decision: the test scans for `--project`, `--global` and `.launch`, and keeps its purpose: it fails when the committed binaries predate the launcher source.
- Security finding by reference, from the third review of the first run: `crates/ocx_shim/src/core.rs` and `crates/ocx_shim/src/main.rs`, class: unescaped control characters in diagnostic text. It predates the branch. The DONE block reports it in full.
- Question: on 2026-11-27 the model that runs the pipeline sub-orchestrators reached its weekly limit (reset 2026-11-28 15:00 Europe/Berlin) and both running sub-orchestrators (pipelines TR and IR) ended without a report. Wait for the reset or go on? Research: their work is saved in the two worktrees; `/hex-execute` § 4 lets the orchestrator spawn every step itself; the model policy allows the default worker model for implementation and reserves the larger model for sub-orchestrators. Decision: no sub-orchestrator runs until the reset. The meta-orchestrator dispatches the remaining pipeline steps directly to workers on the default model, one worker per pipeline, with capped reports. The integration gate and the review run as planned.

## Loop shape

- Entry point: Run the /hex-architect skill on .agents/goals/bazel-rbe-rules-ocx.md.
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
  - Reuse the two existing pull requests, https://github.com/ocx-sh/ocx/pull/784 and https://github.com/ocx-sh/rules_ocx/pull/36. Open no second pull request in either repository.
  - Launcher files are built complete in a staging directory and published with one rename or one link switch. A published launcher directory is never edited. No design relies on old and new files working side by side. Owner: "we solve this with atomic operations", "not yield intermediate broken state by design".
  - Nothing writes into a launcher directory that an older `ocx` published. Owner: "we need to be very sure to not break existing windows installations".
  - One launcher program and one record file with an inner `kind` key, the same on Linux, macOS and Windows. Owner: "one unified launcher with one sidecar that has an inner variant type that is the same on linux, windows and mac".
  - `manifest.json` is the only interface between `ocx vendor` and rules_ocx. rules_ocx derives no file name itself.
  - A lock holds one platform manifest digest per platform and never the digest of an image index. Owner: "we cannot lock an index but only manifests".
  - Owner: "do not re-implement any env/package content or other construction methods"; adjust the current interfaces.
  - Never edit `CHANGELOG.md`. Never write a rollout or user count anywhere. `.serena/project.yml` stays out of every commit.
  - Every Bazel call uses `--ignore_all_rc_files` and an output root in scratch. Every `ocx`, `task` and `grim` call uses a scratch `OCX_HOME` and a scratch `HOME`, or is reported as not run.

## Emphasis

From the source, as pointers; the source's own text governs.

- Read § Context, "Second run", first. It lists what is added and what is decided. Then read the source's block "As handed off on 2026-11-26 (read this first)"; its `## Requirements` still bind, and the blocks "Ruled by the owner" hold the owner's own words.
- The source's `## Verification` names the checks and the test setup. Each check is shown red once and green once. Every check the second run adds follows the same rule.
- Order: land the vendored format first (layout, record file, manifest), because the dev build and rules_ocx depend on it. The home launcher work follows as its own pipelines with file-disjoint packages; it touches four decision records and about 15 test modules.
- The launcher binaries are committed files. Each change of the launcher source needs the rebuild round: the workflow run, six binaries, the recorded hashes, a dev build, the pin in rules_ocx. Batch launcher source changes so that the round runs once or twice.
- Platform claims for macOS and Windows are proven in CI only; this host has neither. The Windows junction retarget and the runfiles manifest lookup each need a Windows CI job. If the junction proof fails, today's remove-then-rename stays and the pull request says so.
- `ocx-sh/ocx-mirror` has no grant in the pasted prompt. If `task satellite:verify` goes red, an act there follows § Issue resolution.

## Context

- Source discussion: `.agents/discussions/bazel-rbe-rules-ocx.md`. It is committed on this run's branch, as are the research files.
- Research: the files under `.agents/research/` that the source lists in its `## Research` section, and for the second run `.agents/research/research_launcher-atomic-publish-and-runfiles.md`.
- Earlier discussion, the baseline for decisions the source does not change: `.agents/discussions/bazel-hermetic-package-bundles.md`.
- Related records: `.claude/artifacts/adr_package_export_import.md`, `.claude/artifacts/adr_bazel_build_adoption.md`, `.claude/artifacts/adr_crate_split_workspace.md`, `.claude/artifacts/adr_ocx_interface_contract.md`. The second run amends `.claude/artifacts/adr_vendor_rules_ocx_remote_execution.md`, `.claude/artifacts/system_design_vendor_rules_ocx.md`, `.claude/artifacts/adr_windows_exe_shim.md`, `.claude/artifacts/adr_package_entry_points.md`, `.claude/artifacts/adr_toolchain_activation.md` with its system design, and `.claude/artifacts/adr_exec_env_model.md`.
- Related work: https://github.com/ocx-sh/ocx/pull/771 (export and import, merged), https://github.com/ocx-sh/ocx/issues/785 (vendored layout), https://github.com/ocx-sh/ocx/issues/679 (refresh of generated launchers, part of the second run), https://github.com/ocx-sh/rules_ocx/issues/37 (package locking).
- Second repository: `ocx-sh/rules_ocx`, a sibling checkout of this repository. Its branch worktree is `.agents/worktrees/hex-bazel-rbe-rules-ocx` there.

Requirements the owner added during the first run (2026-11-26), with where each landed:

- Continuous checks that the committed launcher binaries are current and work: `.github/workflows/launcher-blobs.yml`, `scripts/launcher_blob_check.sh`, `scripts/shim_inputs.py` and `crates/ocx_store/src/shim_inputs.sha256` in this repository.
- The Windows path-length limits and their workarounds documented in rules_ocx with links: `docs/guides/windows-path-length.md` there; the layout question is https://github.com/ocx-sh/ocx/issues/785.
- rules_ocx supports only an `ocx` with `ocx vendor` and reads as designed for it: floor `0.6.8` as one constant (`MIN_OCX_VERSION`), the older integrations, their tests and their documentation removed.

First run, ended 2026-11-26. Heads: ocx `9c9e32d4`, rules_ocx `401c9c4`, both pushed. Its evidence for the five criteria it ticked:

- "ocx has the vendor command in both tiers": https://github.com/ocx-sh/ocx/pull/784, `test/tests/test_vendor.py` and `test/tests/test_package_vendor.py` in the acceptance jobs on head `95759e78`.
- "ocx has a native launcher that starts a tool with no `ocx`": the six native legs of `.github/workflows/launcher-blobs.yml` on head `95759e78`, and `vendor_launcher::launcher_starts_on_image_without_a_shell`.
- "rules_ocx builds every repository from the manifest alone": https://github.com/ocx-sh/rules_ocx/pull/36 at `401c9c4`, job `test` of https://github.com/ocx-sh/rules_ocx/actions/runs/45828193685.
- "rules_ocx has a remote-execution rig that runs in its CI": jobs `rig-buildbarn` and `rig-nativelink` of https://github.com/ocx-sh/rules_ocx/actions/runs/45828193723.
- "The rules_ocx pull request is green against the build of the ocx pull request": `e2e/rbe/ocx-under-test.txt` pins dev build `0.6.8-pr784_20261126125712` of ocx commit `6718aacf`; deep run https://github.com/ocx-sh/rules_ocx/actions/runs/45828177769.

Second run: what is added on top of the current changes.

In ocx, pull request 784:

1. The vendored layout of issue 785: `manifest.json`, `l/NAME` for launchers, `p/HEX16/c/` for content, `p/HEX16/e/NAME` for a package's entry-point launchers. HEX16 is the first 16 hex characters of the digest; `ocx vendor` compares full digests and refuses a clash. One type owns the layout names, and the composer's vendor lane takes them from it.
2. One record file per launcher, `NAME.launch`, JSON with a `kind` key. It replaces `.direct`, `.shim`, `.shimref` and `.exec` for every newly written launcher. The manifest lists the record path of each tool. The command lines the launcher builds for `ocx` stay as they are.
3. The native launcher replaces the generated shell scripts in the Linux and macOS home. Trampoline detection follows.
4. Atomic construction (§ Rules). Packages, deferred tools and vendored trees already publish with one rename. The toolchain directory is rendered into a new directory under `shells/`, then `active` is switched. On Windows the junction is retargeted in place if CI proves it.
5. Issue 679: the directory name carries the launcher generation, and `ocx` creates the directory of its own generation on first use, where it creates deferred-tool launchers today. Old directories stay untouched. The update hand-off refreshes the global home.
6. The launcher reads Bazel's runfiles manifest when no runfiles tree exists, so a Windows build needs no `--enable_runfiles`. The runfiles tree lookup stays.
7. Fixes: `ocx vendor --omit-links` refuses node 26.13.1 (cause unconfirmed, see the research file); the three Warns of the third review; two missing tests (a private dependency behind an entry point, and an entry point reached by bare name through `PATH`); the deferred-tool staging directory that `ocx clean` can delete while it is built; `generate_is_idempotent`, which asserts nothing; the stale doc comment at `crates/ocx_package_manager/src/tasks/pull.rs:466-472`.
8. The decision records and user pages follow in the same series. The launcher binaries are rebuilt and a new dev build is cut.

In rules_ocx, pull request 36:

1. Package locking of issue 37, as its comment with the four-row table says. A package tag without a digest and without an index gets one platform manifest digest per listed platform as a fact in `MODULE.bazel.lock`, and each set vendors that digest. A project stays locked by `ocx.lock`, a package with an index by its index snapshot, a package with a digest by its identifier.
2. `update` targets: one per hub, and one for all. A project hub runs `ocx update`, a snapshot package runs `ocx index update`, a digest-pinned package reports that it is pinned.
3. Launcher and record paths come from the manifest. Fixtures and the Windows path-length page follow the short layout.
4. The pin moves to the dev build of the final ocx head. A tag without a digest resolves; today the extension runs `ocx vendor --manifest-only` without `--remote` and fails on a fresh home.

Owner steps to name in the DONE block, carried from the first run: the merges; the `ocx` release; the rules_ocx floor number with `DEFAULT_OCX_VERSION` and `dist/dist.json`; removal of the pre-release pin files in rules_ocx; the committed `MODULE.bazel` move in ocx after a rules_ocx release; the `buildifier` package that the first run pulled into the real `~/.ocx` and the Chromium download in `~/.cache/ms-playwright`; rig data directories in scratch that need `sudo`.
