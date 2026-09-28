# Goal: code-docs cleanup — comment density, ADR references, log-string tests

Source: .agents/discussions/code-docs-cleanup.md · Written: 2026-09-28 by /hex-loop

## Definition of done

- [ ] "code-docs rules applied via the code-docs-cleanup procedure across prod crates/**/*.rs" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "per-crate ratio reported before/after; 0.17–0.25 expected, never gated, no guard cut for it" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "ADR rationale moved out, at most one file-qualified pointer kept; plan/WP/C-/S-/DX-/round IDs deleted" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "log-output-text asserts rewritten to behaviour, or deleted where already covered" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "#N refs kept only for open-issue workarounds, as owner/repo#N" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "dangling artifact pointers deleted or re-pointed" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "checks adopted first: --discover verdicts, baselines committed, census + linkage in the T0 lint tier" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "ratchet also covers Rust test code and the Python suite" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "every guard keeps constraint + consequence; cleanup_check clean per WP" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "clap/schemars doc-comment changes reviewed as CLI-surface changes" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] "work sliced per crate, file-disjoint" — evidence: the commit, PR comment or
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

- Entry point: Run the /hex-plan skill on "code-docs cleanup — comment density, ADR references, log-string tests, per .agents/discussions/code-docs-cleanup.md".
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
  None.

## Emphasis

None.

## Context

- Source discussion: `.agents/discussions/code-docs-cleanup.md` (ratified 2026-09-28 → loop; Out of scope, Open questions and Verification live there)
- `.claude/rules/code-docs.md`, `.claude/rules/code-docs/`, `.claude/skills/code-docs-cleanup/SKILL.md`
- `.claude/artifacts/research_code_comment_density.md`
