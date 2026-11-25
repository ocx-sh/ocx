# Goal: "rules_ocx under remote execution"

Source: .agents/discussions/bazel-rbe-rules-ocx.md · Written: 2026-11-26 by /hex-loop

## Definition of done

- [ ] "ocx has the vendor command in both tiers" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "ocx has a native launcher that starts a tool with no `ocx`" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "rules_ocx builds every repository from the manifest alone" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "rules_ocx has a remote-execution rig that runs in its CI" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "The rules_ocx pull request is green against the build of the ocx pull request" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "Every check under Verification is shown red once and green once" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "The owner's rules hold in both repositories" — evidence: the commit, PR comment or
  artifact that satisfies it.
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
- Granted acts (authority: the pasted prompt): In ocx-sh/rules_ocx: create one branch, commit, push it, open one PR and issues, dispatch and rerun its CI; never merge, tag or release there; In this repo: push the feature branch and open its draft PR early; dispatch release.yml with mode=dev on that branch only; Start and remove docker compose projects the run creates; never touch other containers
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

## Loop shape

- Entry point: Run the /hex-architect skill on .agents/discussions/bazel-rbe-rules-ocx.md at tier high.
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

None.

## Emphasis

From the source, as pointers; the source's own text governs.

- Read the source's block "As handed off on 2026-11-26 (read this first)" before anything else. Its `## Requirements` bind the run, and the blocks "Ruled by the owner" hold the owner's own words. Older text in the source is the record of how the design was reached.
- The source's `## Intent`, "Hand-off to a goal loop", names the two repositories, the test rig, and what stays with the owner: merges, tags, releases and the publish to the Bazel Central Registry.
- The source's `## Open questions` are this run's docket for the architecture step.
- The source's `## Verification` names the checks and the test setup. Each check is shown red once and green once.
- Platform claims for macOS and Windows are proven in CI only; this host has neither.
- `ocx-sh/ocx-mirror` has no grant in the pasted prompt. If `task satellite:verify` goes red, an act there follows § Issue resolution.

## Context

- Source discussion: `.agents/discussions/bazel-rbe-rules-ocx.md`. It is ignored by git and exists only in the worktree where this file was written, as do the research files.
- Research: the files under `.agents/research/` that the source lists in its `## Research` section.
- Earlier discussion, the baseline for decisions the source does not change: `.agents/discussions/bazel-hermetic-package-bundles.md`.
- Related records: `.claude/artifacts/adr_package_export_import.md`, `.claude/artifacts/adr_bazel_build_adoption.md`, `.claude/artifacts/adr_crate_split_workspace.md`, `.claude/artifacts/adr_ocx_interface_contract.md`.
- Related work: https://github.com/ocx-sh/ocx/pull/771 (export and import, merged) and https://github.com/ocx-sh/ocx/issues/679 (launchers of installed homes, not part of this run).
- Second repository: `ocx-sh/rules_ocx`, a sibling checkout of this repository.
