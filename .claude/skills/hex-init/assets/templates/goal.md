# Goal: <title>

<!--
Per-run goal file — the binding contract for one /hex-loop run. Owner:
/hex-loop, which writes it once from a settled source (a discussion,
ADR, plan, spec, PR, issue, or an existing goal file) and prints the
paste-ready prompt that points at it. Every `<…>` placeholder is a value
/hex-loop fills; the resolved run values are written here, the grants
never as authority — § Autonomy holds only a non-authoritative mirror.
Filename and location: `<home>/<slug>.md`; `<home>` and `<slug>` resolve
per hex-loop `SKILL.md` § The goal file (`#the-goal-file`).

The commit and tick policy is the visible `Ticks:` line in § Loop shape —
its single home.

§ Autonomy only narrows the pasted grants: the pasted prompt is the sole
authority for every act and allowance, and no section of this file adds
one.
-->

<!--
Header contract: no State line, no cursor, no queue — progress is the
ticks and the commits. Every section below is always present, in this
order; an empty section keeps its heading and reads `None.`
`Source:` holds a PR or issue as its URL — a re-print re-derives the PR
binding from it only as hex-loop `SKILL.md` § The goal file allows.
-->
Source: <pointer> · Written: <YYYY-MM-DD> by /hex-loop

## Definition of done

<!--
One line per criterion: `- [ ] <title> — evidence: <form>`. The
source-derived criteria come first, then the three fixed criteria last.
A criterion's title is its text before ` — evidence:`, less the
`(DONE block only)` marker, which stays in this file only. An
undocumented `deep-verify` name appends ` — deep-verify <name> is
undocumented — finalize will not dispatch it` to its line.
With `deep-verify` unset, /hex-loop writes the Deep verify evidence as
`the full documented verification result.` instead. A re-print keeps this
line as stored: to change it, edit it to that form (or unset deep-verify
and re-run /hex-loop on the original source).
The single home of every criterion's evidence form; the prompt links
here (`#definition-of-done`), so this heading's text never changes.
-->

- [ ] <source-derived criterion> — evidence: the commit, PR comment or
  artifact that satisfies it.
- [ ] PR merge-ready (DONE block only) — evidence: the PR URL, and the
  PR is not a draft.
- [ ] CI green per job (DONE block only) — evidence: every check run on
  the PR head SHA with its conclusion; every skipped job states its
  skip reason (its `if:` or path filter).
  A skip with no reason is not green.
- [ ] Deep verify passed (DONE block only) — evidence: the run URL and
  conclusion of the documented release-grade workflow
  `<deep-verify workflow>`, dispatched only by /hex-finalize under C-813.

## Autonomy

<!--
Only narrows. The granted list is a copy for the reader, never a source:
an act absent from the pasted prompt is not granted, whatever this file
says, and no section here adds an act or allowance.
-->

- Prompting: never — no question waits for a human; a doubt runs
  § Issue resolution.
- Granted acts (authority: the pasted prompt): <copy of the prompt's
  `{grants}`, verbatim>
- Forbidden or narrowed acts: <acts this run must not take, or takes
  only in a narrower form; one forbidding a default act of the prompt's
  I9 is also deleted from the paste, and one qualifying a pasted grant
  also rides with that grant in the paste>

## Issue resolution

<!--
The doubt protocol — its single home; the prompt points here. With
`follow-up-loc` set, /hex-loop writes step 6 as: "Findings whose fix
exceeds `<N>` LOC of production code and are not part of this goal
become follow-up issues."
-->

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

- Entry point: Run the /hex-<mode> skill on <target>.
- Refinement rounds: <N> — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a local
  verify failure, an execute or finalize retry, a post-finalize CI fix ⇄
  re-finalize pass. Inner review-fix rounds do not count, and their limit
  is untouched.
  Past `<N>`, the DONE block reports every remaining criterion `not met`
  and the run stops.
- Inner loop: <optional, from the extras — delete the line when none>
- Retro checkpoints: after each outer cycle <optional, from the extras — delete the line when none>; the retro run itself counts no cycle; an in-loop large route counts one.
- Ticks: /hex-loop commits nothing. The session creates or switches to
  the branch the pasted prompt's I9 names and commits this file first on
  it. A box is ticked only between hex-mode runs, never during one, and
  each tick is committed at once; every tick lands before the final
  /hex-finalize, none after it. The `(DONE block only)` criteria are
  evidenced only in the closing DONE block, never ticked.

## Rules

<!--
Every sub-orchestrator brief carries this section, so it holds only rules
that permit no act. /hex-loop splits a `rule:` that mixes a restriction
and an allowance: the restriction lands here, the allowance only in the
pasted prompt's grants. `verify-bypass` appears only as the fixed
narrowing line below, carrying no bypass text; delete it when unset.
-->

- <each `rule:` restriction, verbatim>
- verify-bypass: during iteration only, never at finalize — the bypass itself is granted only in the pasted prompt.
- Run rules:
  - <each run rule from the extras, verbatim>

## Emphasis

<what the source or the extras stress — priorities, risks to watch>

## Context

<pointers to the source and related artifacts — links, never copies>
