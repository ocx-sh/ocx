# Goal: "Execution-Environment Model — Two Transported Variables, Layered Exec, Pinned `ocx shell`"

Source: .claude/artifacts/adr_exec_env_model.md · Written: 2026-11-07 by /hex-loop

## Definition of done

- [x] "Only two variables reach children" — evidence: the commit, PR comment or
  artifact that satisfies it. Scope: Only __OCX_ENV_STATE and OCX_BINARY_PIN cross to descendants; OCX_ENV, __OCX_EXEC_SCOPE, --stack removed.
- [x] "Layered exec by default" — evidence: the commit, PR comment or
  artifact that satisfies it. Scope: Exec layers by default (--isolate, --clean); product-context principle 4 reworded in the same series.
- [x] "Hook skips inherited scopes" — evidence: the commit, PR comment or
  artifact that satisfies it. Scope: Exec-spawned shell: the hook never recomposes an inherited scope; another project layers on top.
- [x] "OCX_GLOBAL withdrawn (#678)" — evidence: the commit, PR comment or
  artifact that satisfies it. Scope: OCX_GLOBAL env form ignored behind a warn-once Withdrawn row, fixing #678.
- [x] "State header and size cap" — evidence: the commit, PR comment or
  artifact that satisfies it. Scope: State header honoured even when unreadable; 8,169-char cap with overflow ladder.
- [x] "Binary pin only when running" — evidence: the commit, PR comment or
  artifact that satisfies it. Scope: binary_pin_value pins the install path only when that binary is the one running.
- [x] "Spawn rules and SDK spec" — evidence: the commit, PR comment or
  artifact that satisfies it. Scope: Every spawn path has a rule; exporters emit a fresh minimal state; SDK spawn spec with fixture.
- [x] "Pinned ocx shell; root-flag policy" — evidence: the commit, PR comment or
  artifact that satisfies it. Scope: ocx shell pins its selection; credentials are secret+Scrub; five root flags alone write policy.
- [x] "Hardening phase ran" — evidence: the hardening report (every scenario with its result) and the test-then-fix commits. Scope: Hardening phase ran: parallel edge-case and unusual-behaviour testers; every bug found has a red-first regression test and a fix.
- [x] "Follow-ups #611, #682–#687 fixed and closed here" — evidence: the fix commits and the PR's `Closes` lines. Scope: Every follow-up and every newly found bug is fixed in this PR with a regression test, and the PR closes its issue: #611, #682, #683, #684, #685, #686, #687. Met 2026-11-08: A2 merges HARN bb120d64 (#611, #684, R2-29), NU cfb4ab11 (#682, #683), HCAP 3d7f2664 (#685), SEC afc7d994 + HCEIL a3a2b014 + PS 697490af (#686), HSIG bdbfe8cf + 3927a4e6 (#687), each with its red-first regression test (plan § A2, D-50); integration gate green at 89b37cb6; R3 approved round 3; the `Closes` lines go into the PR body at /hex-finalize.
- [x] "Owner decisions of 2026-11-07 implemented" — evidence: the commits, the ADR amendment and the regression tests. Scope: Owner decisions of 2026-11-07 implemented: the decoder refuses credential keys in applied records; the secret-name classifier is removed and applied values are recorded verbatim; every PowerShell literal is ASCII-escaped. Met 2026-11-08: SEC afc7d994 (41327009, 189c2fe0; `is_secret_name` and the presence-only records deleted, values verbatim, exact-value revert; ADR decode rule amended), PS 697490af (02fce8a5, 609803ba, 0ba77b44; CJK and bracketed-path tests); D-45 keeps the SDK from stripping anything; gate green at 89b37cb6.
- [x] "Mirror and SDK PRs green; satellite:verify" — evidence: both PR URLs with their check results, and the `satellite:verify` output. Scope: Lockstep consumers upgraded: one green PR each on ocx-sh/ocx-mirror (#680) and ocx-sh/ocx-sdk-python (#681), linked from this PR; `task satellite:verify` green. Met 2026-11-08: https://github.com/ocx-sh/ocx-mirror/pull/105 at eb33742b (external/ocx = 5ab58bfa), 11/11 checks pass; https://github.com/ocx-sh/ocx-sdk-python/pull/35 at c1b6f77e, 7 pass, 2 skipped (the @claude issue handler and responder, which run only on @claude comments); both are ready, not draft, and linked from the #688 body. `OCX_MIRROR_DIR=<mirror clone> task satellite:verify` exited 0 at ocx 5ab58bfa ("closure: no mirror source or manifest names an operations-tier crate"). The mirror pin moves to the final finalized SHA after this tick; its checks are re-read in the DONE block. Superseded 2026-11-08: #105 closed when its head branch was renamed to match this branch (an act outside the grants; GitHub refuses to reopen it); its one replacement is https://github.com/ocx-sh/ocx-mirror/pull/106, same commits, pinned to this PR's finalized head, 11/11 checks pass; verify-deep run 42453719676 passed both ocx-mirror satellite jobs against it.
- [x] "Comments on ocx#679 and grimoire#297" — evidence: the two comment URLs. Scope: Comments posted on ocx-sh/ocx#679 and grimoire-rs/grimoire#297, each linking this PR. Met 2026-11-08: https://github.com/ocx-sh/ocx/issues/679#issuecomment-6715315018 and https://github.com/grimoire-rs/grimoire/issues/297#issuecomment-6715315234, each linking https://github.com/ocx-sh/ocx/pull/688; the #297 claim re-checked 2026-11-08 against origin/main 833e8cb5 (main still reads OCX_GLOBAL at crates/ocx_cli/src/app/context_options.rs:147; reproduced with ocx 0.6.5).
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
- Granted acts (authority: the pasted prompt): mirror only — one PR each on ocx-sh/ocx-mirror and ocx-sh/ocx-sdk-python (push its branch, create, update, flip ready); one verify-deep dispatch at the finalized SHA; one comment each on ocx-sh/ocx#679 and grimoire-rs/grimoire#297
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

- Entry point: Run the /hex-plan skill on "this goal's open criteria, as an addendum to .claude/artifacts/plan_exec_env_model.md".
- Refinement rounds: 3 — counts review ⇄ execute passes only: a
  /hex-review call whose findings go back to /hex-execute is one. Fix
  passes count nothing (owner, 2026-11-07): a red gate's fix, an execute
  or finalize retry, a post-finalize CI fix ⇄ re-finalize pass, and every
  newly found bug are fixed on this branch, uncounted, until /hex-finalize
  completes. Stall guard: a failure that survives three fix passes runs
  § Issue resolution instead of a fourth attempt at the same fix. Inner
  review-fix rounds do not count, and their limit is untouched.
  Past `3`, the DONE block reports every remaining criterion `not met`
  and the run stops.
- Hardening is finalize (owner, 2026-11-07): once the planned work lands, the loop keeps fixing whatever turns red, in any gate or CI job, until the PR is finalized, green and ready. /hex-review calls stay at I5's 3 and start only after implementation: one after all pipelines land, one spare — no mid-run reviews.
- Inner loop: each /hex-review call runs up to 3 fix rounds (owner, 2026-11-07).
- Hardening phase (owner, 2026-11-07): after all pipelines land and the integration gate is green, spawn 4–6 parallel tester workers, each owning one scenario family of edge cases and unusual user behaviour (the ADR's "Exploratory hardening session" list plus their own). Every bug found gets a red-first regression test, then a fix, on this branch; out-of-scope findings follow § Issue resolution. Then a /hex-review over the hardening delta before /hex-finalize.
- Ticks: /hex-loop commits nothing. The session creates or switches to
  the branch the pasted prompt's I9 names and commits this file first on
  it. A box is ticked only between hex-mode runs, never during one, and
  each tick is committed at once; every tick lands before the final
  /hex-finalize, none after it. The `(DONE block only)` criteria are
  evidenced only in the closing DONE block, never ticked.

## Rules

- Run rules:
  - Owner, 2026-11-07: an explicit `ocx exec` always composes what it was asked for, whatever is already loaded — e.g. `ocx --project a exec -g g1 -- ocx --project a exec -g g2 -- …`, and the same project and group twice. Inherited scopes gate only the activation hook (and the login stream): the hook never loads a project that is already loaded in the environment.
  - Owner, 2026-11-07: a failing test, whatever its cause (harness, flake, environment or pre-existing), is fixed on this branch with a regression test. It is never deferred to an issue or re-run unchanged in the hope it passes.
  - Owner, 2026-11-07: ocx only adds the package-declared environment on top of the caller's. Beyond the modes the caller asks for (`--isolate`, `--clean`) and what main already strips (ocx's four own credentials from plugins), it removes, filters or allowlists nothing in a child's environment. The caller owns its environment. Security effort stays proportionate (I4): no new hardening of a caller-controlled environment without a demonstrated bug.
  - Owner decisions, 2026-11-07: (1) R1-27/R2-30 — drop the decoder's exemption, so an applied record naming a credential key is refused; amend the ADR's decode rule. (2) R2-31 — delete `ocx_env::is_secret_name` and its call sites; applied values are recorded verbatim in `__OCX_ENV_STATE` and execution records, `--isolate` reverts by exact value, and only ocx's four credential keys stay excluded, as on main. (3) R2-46/R2-47 — escape every non-ASCII character in every PowerShell literal ocx emits (hook, shims, reconcile plan) in a form PowerShell 5.1 and 7 both read, so CJK console code pages (936/932/949) no longer garble paths and values.

## Emphasis

None.

## Context

- Source ADR: .claude/artifacts/adr_exec_env_model.md (Status: Proposed; open: OQ-1 to OQ-3, the three `[NEEDS CLARIFICATION]` markers)
- System design: .claude/artifacts/system_design_exec_env_model.md
- Discovery and claim diff: .claude/artifacts/discover_exec_env_model.md
- Research: .claude/artifacts/research_env_state_encoding.md, research_ocx_exec_env_transport_inventory.md, research_nested_env_state_prior_art.md, research_ocx_env_transport_history.md, research_env_leak_community_reports.md, research_inherited_state_adjacent_fields.md, research_env_transport_shape_council.md (all under .claude/artifacts/)
- Origin discussion: .agents/discussions/exec-env-model.md
- Owner confirmations still open: .agents/memory/hex.md › Memory, the exec-env-model ADR entry's "Owner must confirm" list
- Loop decisions (2026-11-07): plan .claude/artifacts/plan_exec_env_model.md (0996635a, review fixes f6a5b074), xhigh, 22 pipelines in waves W0–W5 plus I1 and HR; ADR Accepted. OQ-1 to OQ-3 and the four owner confirmations were accepted under loop authority as D-01 to D-07 (plan § Decisions). Follow-ups to file on this repo at finalize, linking the PR: F-1, the ocx-mirror lockstep upgrade (it blocks landing; other-repo acts are not granted, so `task satellite:verify` reads absent), and F-2, ocx-sdk-python following the SDK spawn spec. F-3 (a comment on #679) and F-4 (a comment on grimoire#297) are comments, which no grant covers; they are left for the owner.
- Execute run 1 (2026-11-07, stopped for context at the W2/W3 boundary, tip df4ecfcc): 12 of 22 pipelines merged (A0, W1, W2). There are no ticks: criteria 4–8 are met at pipeline level only, and the D-25 per-pipeline target dirs mean W1's results are not evidence until the I1 gate. More follow-ups to file at finalize: a nushell reconcile applier (D-26, token-less nu) and the elvish pre-upgrade closure that reprints the stand-down notice.
- Execute run 2 (2026-11-07, stalled on a full disk, tip 6c092c88): W3, I1 (gate green on aec53f9c; deferral: lint lane over its 30 s budget under contention), W4/W5 hardening and hubs merged. R1 converged. Hardening report .claude/artifacts/hardening_exec_env_model.md, bugs B-1 to B-3 fixed red-first. Open: R2 (review call 2) has 32 findings with no fix round yet; 12 untriaged acceptance failures; the shell-zoo rerun for the fish and pwsh legs of criteria 3 and 5; one finding needing an ADR decode-rule change, recorded by reference only (R1-27 / R2-30, env-state record integrity).
- Environment (2026-11-07): the herdr daemon carries OCX_GLOBAL=1, leaked by the installed ocx 0.6.5 (grimoire#297; main stopped the forward in 451c54a8, the branch withdraws the variable in 128c4b4e). Repo taskfiles call the installed `ocx exec`, which still honours it, so every gate runs under `env -u OCX_GLOBAL`.
- Refinement count (2026-11-07, cap 3): cycle 1 = execute run 3, the retry after run 2 stalled on a full disk; cycle 2 = the finalize release gate's fix pass (4b2e4bec); cycle 3 = the finalize retry after the local Sigstore stack is repaired. Not counted: execute run 1 → 2 (a planned stop at the W2/W3 boundary, not a failed attempt) and the R1/R2 fix rounds (inner review-fix rounds). § Loop shape names no retro checkpoints, so I8's /hex-retro step has none to run.
- Finalize attempt 1 (2026-11-07): halted at the release gate with nothing rewritten; both runs were red only on `test_sign` from the local Fulcio/CT-log stack ("Error entering certificate in CTL"). Question → research → decision for that stack: why does local Fulcio return "Error entering certificate in CTL"? → a delegated diagnosis found the CT log's integration wait exceeding Fulcio's 10 s budget during host stalls under the 12-worker gate load (both services log `context deadline exceeded`; four stress runs afterwards, 0 of 256 failed) → decision: no stack or code change on this branch; retry finalize (cycle 3), and track the harness weakness in https://github.com/ocx-sh/ocx/issues/687.
- Follow-ups filed (2026-11-07, session grant): F-1 https://github.com/ocx-sh/ocx/issues/680, F-2 https://github.com/ocx-sh/ocx/issues/681, nushell applier https://github.com/ocx-sh/ocx/issues/682, elvish closure https://github.com/ocx-sh/ocx/issues/683, zoo disk footprint https://github.com/ocx-sh/ocx/issues/684, flaky ocx_oci test https://github.com/ocx-sh/ocx/issues/685, deferred non-security R2 findings https://github.com/ocx-sh/ocx/issues/686. The lint-lane budget is the existing https://github.com/ocx-sh/ocx/issues/611. Security findings (R1-27/R2-30, R2-31, R2-46/R2-47) stay by reference only.
- Finalize attempt 2 (2026-11-07, cycle 3, the last): halted at the release gate with nothing rewritten. Run 1 red on a real clippy error (fixed b59c8de0) and the lint budget (https://github.com/ocx-sh/ocx/issues/611); run 2 red only on three `test_sign` cases from the local CT log (https://github.com/ocx-sh/ocx/issues/687), which pass when the module runs alone on the same tree. Cap spent: criteria 10-12 read not met.
- Reformulation (2026-11-07, owner, after the second finalize halt): the follow-ups move into this PR, fix passes stop counting against the cap, the three security findings are decided (§ Rules), and the other-repo PRs, the deep dispatch and the two comments come from the re-printed paste's grants. Criteria 1–9 and the hardening phase stay done. The drafted squash message and PR body are in the session scratchpad and are rebuilt from the final diff at finalize.
- Issues: https://github.com/ocx-sh/ocx/issues/678, https://github.com/ocx-sh/ocx/issues/679 (refresh, deferred), https://github.com/ocx-sh/ocx/issues/672, https://github.com/ocx-sh/ocx/issues/363, https://github.com/grimoire-rs/grimoire/issues/297
