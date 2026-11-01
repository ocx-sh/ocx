---
name: assess
description: Use when a feature request or enhancement issue needs assessment before planning — "assess this feature", "is this feature worth it". Read-only, timeboxed. Gives a verdict (already-supported, accept, needs-design, out-of-scope, …), the need behind the ask, existing capability, interface surfaces touched, directions and size. Bug counterpart: rca.
user-invocable: true
argument-hint: "issue-number | url | description"
triggers:
  - "assess this feature"
  - "feature assessment"
  - "evaluate feature request"
  - "is this feature worth"
---

# Assess — feature request assessment

**Read-only, timeboxed.** Never edit a tracked file, never commit, never comment on the issue. Light feasibility only — deep design is `/hex-plan` or `/hex-architect`; when the answer needs it, the verdict is `needs-spike` or `needs-design`, not a design.

Same frame as [`/rca`](../rca/SKILL.md): intake, evidence graded **observed / inferred / assumed**, uncited = assumed, fan-out workers `model: sonnet` with a `Model rationale:` line.

## 1. Intake

- `gh issue view <n> --json title,body,comments,labels` or the given text.
- **Need** — the job the requester is trying to get done, in one sentence, *without* their proposed flag or syntax. The **ask** is recorded separately.
- Duplicates: `gh issue list --state all --search "<key terms>"`.

## 2. Existing capability — first, mandatory

The cheapest answer is "it exists". Search, in parallel (≤3 `worker-explorer`, `model: sonnet`):

- code (commands, options, config keys), `ocx <cmd> --help`
- docs (`website/src/content/docs/`), ADRs (`.claude/artifacts/adr_*`)
- open/closed issues and PRs

Result: exists / partial (name the gap) / workaround (show it) / absent — each with citation.

## 3. Fit

Against [`product-context.md`](../../rules/product-context.md): OCX is a backend tool for other tools, not an end-user UX. Is this a setting, a command, or someone else's job? Prior art: how the competitors listed there solve it.

## 4. Surfaces touched

Table over the interface surfaces in [../rca/references/sizing.md](../rca/references/sizing.md). Every row: **touched / untouched / unknown**, and door type (one-way unless additive). Also list internal crates touched (`scripts/crate_map.toml` for allowed edges).

## 5. Directions

≥2 directions (one may be "no feature, document the workaround"). For the preferred one: no-gos (explicitly out of scope), rabbit holes (unknowns that could blow the size), drawbacks, **cost to keep** (maintenance, contract surface, docs).

## 6. Size

Apply [../rca/references/sizing.md](../rca/references/sizing.md), plus a confidence tag. Low confidence → `needs-spike`.

## Report

```
Verdict:    already-supported | partially-supported | accept | needs-spike | needs-design | needs-info | duplicate (#n) | out-of-scope | decline-workaround
Confidence: high | medium | low
Size:       S | M | L | XL — <basis>
Need:       <one sentence, no requested syntax>
Decision:   <the one decision the owner must make>
```

Then terse: ask as stated · existing capability (cited) · fit · surfaces table · directions + no-gos · risks / rabbit holes · cost to keep · open questions and the next step (`/hex-plan`, a spike, or a question back to the requester).

Links: issues, PRs and commits are clickable.

$ARGUMENTS
