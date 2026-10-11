# Goal: "OCX docs to the next level (Starlight, use-case focus, lean reference)"

Source: .agents/discussions/docs-next-level.md · Written: 2026-10-11 by /hex-loop

## Definition of done

- [x] "AI config: reference rule sharpened and docs-quality gate wired" — evidence: plan P1 merged (test/lint/test_reference_shape.py, test_docs_quality_gate.py, docs-style/subsystem-website rules); P14 task verify green at eb02c44b9.
- [ ] "Theme PR on ocx-sh/website, ocx pinned to its SHA" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [x] "Docs ported to Starlight with @ocx-sh/theme" — evidence: plan P5/P5a/P5b merged; site builds under Bazel, task verify green at eb02c44b9.
- [x] "CLI, env and config reference generated with drift check" — evidence: plan P3/P9 merged (ocx_schema docs generator, website:reference:check, bazel:build:drift; ee6569878 wires it into CI); /hex-review drift proof: baseline exit 0, planted page edit and vars.rs doc edit each exit 201, restored exit 0; task verify --force rc=0 at be75e5f47.
- [x] "Task-first IA via /docs-plan" — evidence: plan P6 (.claude/artifacts/docs_ia.md) and P12 merged, redirects in website/redirects.d/.
- [ ] "Production /docs/ and /schemas/ switched to Bunny" — evidence: the commit, PR comment or
  artifact that satisfies it.
- [x] "Everything moved to ocx-website cleaned up" — evidence: plan P11/P11b merged; C-017 grep 0 hits at eb02c44b9.
- [x] "Every page reviewed against docs-quality" — evidence: P13a-f clusters plus P14; MUST grade 2514 -> 0 by the test:lint gate, ratchet deleted (ef0ea3a66).
- [x] "No regression: grade and content inventory preserved" — evidence: check.py exit 0 with 0 uncovered (48 baseline + 111 new pages); MUST 2514 -> 0; SHOULD 4588 -> 533 measured at B3.
- [ ] "Final parallel haiku completeness sweep dry" — evidence: the commit, PR comment or
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
- Forbidden or narrowed acts: none

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

- Entry point: Run the /hex-plan skill on "OCX docs to the next level (Starlight, use-case focus, lean reference), per .agents/discussions/docs-next-level.md".
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

- Run rule (owner, 2026-10-11): skip verification during development. Docs-only changes run no implementation tests; code-touching WPs defer cargo/bazel/acceptance tests too. Every implementation test, plus the drift-check red/green, runs only at the integration gate and /hex-finalize. Commits use `task verify:mark` with the deferral named in the body.
- Run rule (owner, 2026-10-11): before dispatching the online deep verify, run every check on the local Windows host as well (WSL2 → `cmd.exe`, cross-compiled with xwin, staged under `C:\Users\ecom\ocx-wintest\`): the Rust tests and the acceptance suite, the Windows-only modules included. Fix any red before going online.

## Emphasis

None.

## Context

- Source: `.agents/discussions/docs-next-level.md` (requirements, decisions, open questions, verification)
- Research: `.agents/research/research_reference-vs-explanation.md`, `research_docs-reference-recon.md`,
  `research_starlight-reference-generation.md`, `research_theme-gap-ocx-docs.md`,
  `research_docs-use-case-inventory.md` (all under `.agents/research/`)
- Theme repo handover: `../ocx-website/HANDOVER.md`; pilot plan `../ocx-website/.agents/plans/plan_phase3-pilots.md`
