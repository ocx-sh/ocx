# Goal: "ocx → rules_rs + hermetic-llvm, cross-build from Linux"

Source: /home/mherwig/.claude/plans/humble-meandering-fox.md · Written: 2026-10-20 by /hex-loop

## Definition of done

- [x] "every pipeline landed" — evidence: the commit, PR comment or
  artifact that satisfies it. Evidence: branch head 493461f01 (all pipelines merged; integration `task verify` run 4 EXIT 0).
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

- Entry point: Run the /hex-execute skill on /home/mherwig/.claude/plans/humble-meandering-fox.md.
- Refinement rounds: 2 — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a local
  verify failure, an execute or finalize retry, a post-finalize CI fix ⇄
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
  - Use our ocx project-zot/zot to run integration tests on windows, linux and macos.
  - The new CI should only run the test executable and acceptance tests on non-linux runners.

## Emphasis

- The verify-deep and verify-basic verification should be fixed; it looks like the acceptance tests run twice there.
- Our acceptance test execution should allow sharding (maybe 4 runners) to parallelize, then merge the junit results.

## Context

- Source plan: /home/mherwig/.claude/plans/humble-meandering-fox.md
- Migrate to rules_rs and hermetic-llvm, building on the go spike on branch spike/rules-rs (HEAD 47f2a2bb5, evidence in .claude/artifacts/spike_rules_rs_evidence.md on that branch).

## Decisions

- Double-run question: the goal says acceptance "runs twice" in verify-deep and verify-basic. Research on run [39456163958](https://github.com/ocx-sh/ocx/actions/runs/39456163958): `Acceptance (Linux)` executed 179/179 targets once, with no in-job double run. The duplication was (a) the acceptance job rebuilding `ocx` and the schema through Bazel (385 s) after waiting on the 3-OS build matrix, and (b) Linux unit tests running in both verify-basic `smoke` and verify-deep `build` on every push to `main`.
- Decision: the acceptance shards consume the Linux jobs' artifacts; deep's `build` skips `bazel:test:unit` on `push` events.
- Native-acceptance scope: macOS and Windows run the smoke subset on native zot (`OCX_TEST_NATIVE_SERVICES=1`); full-suite parity there is a follow-up; the `registry:2` mirror has no native build, so its fixtures skip there.
- Issue resolution (cycle 1, lead decision): the serial "Build (Linux, all targets)" step took 2h+ cold on run [39518223260](https://github.com/ocx-sh/ocx/actions/runs/39518223260), the regression this migration was meant to remove. verify-deep now has a `cross` matrix (8 parallel ubuntu legs, `fail-fast: false`); each leg uploads its own `ocx`, plus the unit bundle and acceptance binaries for the three triples that have a native leg. `build` keeps the Linux unit tests and the schema once. `unit-native` and `acceptance-native` need `cross`; the shards need `build` and `cross`. GitHub's `needs:` names a whole matrix, so a consumer waits for the slowest leg. Cache writes stay read-only on this lane: [ocx-sh/ocx#638](https://github.com/ocx-sh/ocx/issues/638).
