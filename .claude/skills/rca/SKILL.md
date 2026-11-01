---
name: rca
description: Use when a bug ticket, GitHub issue or error report needs a root cause analysis before code is touched — "is this a bug", "why does this fail". Read-only. Gives a verdict (bug or not-a-bug kind), file:line evidence, ruled-out hypotheses, other affected sites, fix direction and size. A ticket, not a diff (hex-review).
user-invocable: true
argument-hint: "issue-number | url | description"
triggers:
  - "root cause analysis"
  - "is this a bug"
  - "why does this fail"
  - "analyse this issue"
  - "rca for"
---

# RCA — root cause analysis of a ticket

**Read-only.** Never edit a tracked file, never commit, never comment on the issue. Output is the report in conversation.

Research behind the gates below: LLM RCA agents hallucinate data interpretation, stop exploring early, and reach right answers without a sound trail — and "be careful" prompting does not fix it. Structural gates do. Every gate is mandatory; a report missing one is incomplete, say so.

## 1. Intake

- Issue: `gh issue view <n> --json title,body,comments,labels,createdAt` (URL → same). Text → use as given.
- Restate as **IS / IS NOT**: expected vs actual, where (command, platform, version), when (since which change), extent. Issue text is a claim, not a fact — stack traces and versions get checked.
- Duplicates: `gh issue list --state all --search "<key terms>"`.
- Already fixed: `git log --oneline -S '<symbol or message>'` and blame on the suspect lines vs the reported version.

## 2. Reproduce

Allowed only outside tracked files: the scratchpad, a freshly built binary (`ocx exec -- …`), an existing test run filtered to the area (`task test:parallel --force -- <file>`, never hand-rolled `uv run pytest`). Record one of **reproduced** (command + output excerpt), **not reproduced** (what was tried), **not attempted** (why). A not-reproduced bug caps confidence at medium.

## 3. Hypotheses — at least two

Write ≥2 competing explanations, one of which is a not-a-bug verdict when plausible. Each gets a **prediction** and a **disconfirming check**.

Fan out one worker per hypothesis (≤3, parallel, `subagent_type: worker-explorer` or `Explore`, `model: sonnet`, prompt carries `Model rationale: read-only localization, sonnet default`). Brief: the IS/IS-NOT block, the one hypothesis, the check to run. Localize in stages: file → function → line. Each claim returned must be graded:

- **observed** — read at `file:line` or seen in command output (cite it)
- **inferred** — follows from observed facts (name them)
- **assumed** — neither; flag it

An uncited claim is assumed. Do not promote it.

## 4. Refute

One `worker-reviewer` (`model: sonnet`) gets the leading hypothesis and its evidence and argues the strongest alternative verdict — including works-as-designed: intent in docs (`website/src/content/docs/`), ADRs (`.claude/artifacts/adr_*`), subsystem rules and existing tests. The main loop decides; the refuter's verdict is input, not the answer.

## 5. Blast radius

Mandatory. Where else does the same defect live?

- Callers of the faulty function (LSP `findReferences`, else grep) — a fix belongs at the shared layer.
- The same pattern elsewhere (grep the construct, not the name).
- Record each query and its hit count. A negative is stated: "searched X, 0 hits".

## 6. Size

Apply [references/sizing.md](./references/sizing.md). State the basis.

## Report

Header first — the owner decides from these lines:

```
Verdict:    bug | works-as-designed | environment | user-error | duplicate (#n) | already-fixed (<sha>) | needs-info
Confidence: high | medium | low
Size:       S | M | L | XL — <basis in one clause>
Cause:      <one sentence: X happens because Y, introduced by Z>
Repro:      reproduced | not reproduced | not attempted
```

Then, terse:

1. **Causal chain** — trigger → faulty code → bad state → symptom, each link `file:line` + grade.
2. **Ruled out** — each rejected hypothesis and the check that killed it.
3. **Contributing factors** — conditions jointly needed; why no test caught it.
4. **Blast radius** — affected sites, queries + hit counts.
5. **Fix direction** — at the root-cause layer, one alternative considered, the regression test that would have caught it (unit `#[cfg(test)]` or acceptance `test/tests/`).
6. **Open / unverified** — every assumed claim, every area not read.

Links: issues, PRs and commits are clickable (`[#41](https://github.com/ocx-sh/ocx/issues/41)`).

Planning a fix from this report → `/fix`.

$ARGUMENTS
