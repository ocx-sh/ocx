# Goal: "Plan: index closure — ocx-sh/ocx#639"

Source: .claude/artifacts/plan_issue_639_index_closure.md · Written: 2026-10-24 by /hex-loop

## Definition of done

- [x] every pipeline landed — evidence: plan State: review; ocx
  hex/plan-issue-639-index-closure @ 0b5e562c1 (7 finding commits), indexbot @ 1674eff (12),
  catalog @ 0b397f0 (8); integration gates green per the plan Status block.
- [ ] PR merge-ready (DONE block only) — evidence: the PR URL, and the
  PR is not a draft.
- [ ] CI green per job (DONE block only) — evidence: every check run on
  the PR head SHA with its conclusion; every skipped job states its
  skip reason (its `if:` or path filter).
  A skip with no reason is not green.
- [ ] Deep verify passed (DONE block only) — evidence: the full
  documented verification result.

## Autonomy

- Prompting: never — no question waits for a human; a doubt runs
  § Issue resolution.
- Granted acts (authority: the pasted prompt): satellites indexbot (ocx-sh/indexbot) and catalog (ocx-sh/catalog): run /hex-finalize's steps in this session on each satellite's hex/ branch — recompose, signed, linear — force-push with lease to that branch, create or update its one PR, flip it ready; finalize halt (5) is waived for these two; comment once on ocx-sh/ocx#639 linking the three PRs and listing deferred and dropped items with reasons; rerun an infra-flaked CI job once per job per SHA
- Forbidden or narrowed acts: never merge, tag, release or publish in
  any repo; never push to any `main`; never commit on, rewrite or merge
  `goat` (it carries an armed `backup/goat-pre-finalize` ref); never
  touch the owner's uncommitted changes in any primary checkout; never
  edit the plan's other-repo sections from a pipeline in another repo; open no issues — deferrals, follow-ups and
  out-of-scope findings go into the one #639 closing comment (owner,
  2026-10-24).

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

- Entry point: Run the /hex-plan skill on .claude/artifacts/plan_issue_639_index_closure.md.
- Refinement rounds: 2 — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a red
  gate's fix pass, an execute or finalize retry, a post-finalize CI fix ⇄
  re-finalize pass. Inner review-fix rounds do not count, and their limit
  is untouched.
  Past `2`, the DONE block reports every remaining criterion `not met`
  and the run stops.
- Inner loop: per PR at most 2 rounds of /hex-review ⇄ /hex-execute, then /hex-finalize; /hex-plan turns the rough plan into a federated plan (Repo column `.`, `indexbot`, `catalog`) and /hex-execute runs the three repos' pipelines in parallel.
- Ticks: /hex-loop commits nothing. The session creates or switches to
  the branch the pasted prompt's I9 names and commits this file first on
  it. A box is ticked only between hex-mode runs, never during one, and
  each tick is committed at once; every tick lands before the final
  /hex-finalize, none after it. The `(DONE block only)` criteria are
  evidenced only in the closing DONE block, never ticked.

## Rules

- Run rules:
  - Stick to the plan's decided scope and minimal fixes; deferred and dropped items stay deferred and dropped.
  - Yanked tags keep their objects; installability is company policy (owner decision 2026-10-24).
  - One signed conventional commit per fix (A1+A9+X4 and C11+X2 share one), linear history; this overrides hex.md's "squash a PR branch to one commit" preference for this run.
  - Each repo's full local test suite is run by hand before its PR leaves draft, and its result is quoted in the PR body.
  - Never edit `CHANGELOG.md`; the commit subject is the changelog line.

## Emphasis

- Bugfix workflow: red regression test first, then the fix, per finding.
- Parallel execution with subagents; the three repos proceed concurrently.
- Three PRs, one per repo (ocx-sh/ocx, ocx-sh/indexbot, ocx-sh/catalog), each merge-ready; landing order catalog → ocx → indexbot.
- ocx: integration and release gates add what `task verify` cannot see (hex.md Preferences: local Windows `cargo test --workspace` on `C:`, `verify-deep.yml` dispatched on first push).
- indexbot: 100% branch coverage and named G-01..G-20 tests are binding (its `.claude/rules/quality-indexbot-security.md`).
- catalog: main is v0.6.0 (Astro port); re-anchor and re-verify every C-item there before its red test.

## Context

- Rough plan: `.claude/artifacts/plan_issue_639_index_closure.md`
- Issue: https://github.com/ocx-sh/ocx/issues/639
- Verified findings (2026-10-24, per finding: verdict, evidence, minimal fix): `.agents/research/issue639_verified_findings.json`; issue body copy: `.agents/research/issue639_body.md`
- Federation keys: `.agents/memory/hex.md › Pointers` (`indexbot`, `catalog`)
- Related: https://github.com/ocx-sh/indexbot/issues/9 (touches `root_glob`; do not conflict)
