# ADR: Skill description philosophy — Contextual Signal Only (CSO)

## Metadata

**Status:** Proposed
**Date:** 2026-04-19
**Deciders:** AI config overhaul (automated planning session)
**Related Plan:** `plan_ai_config_overhaul.md` (Phase 2)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path: Anthropic skill format; contradicts Anthropic *description prose guidance* ("what it does + when to use it") where empirical evidence shows the guidance causes skill bypass
**Domain Tags:** devops (AI config), api (skill discovery contract)
**Supersedes:** Implicit current practice (skill descriptions written as workflow summaries)
**Superseded By:** N/A

## Context

Per `research_ai_config_superpowers.md` §Pattern 1, Superpowers maintainers observed empirically that a skill description containing workflow-summary language (verbs like "dispatches", "runs", "iterates", "orchestrates", or prose describing what the skill does step-by-step) causes Claude to read the description as a shortcut and skip loading the SKILL.md body. The workflow then runs against the description alone — a degraded, truncated version of the intended workflow.

Superpowers calls the remediation "Contextual Signal Only" (CSO): descriptions must describe the *trigger conditions* for invocation (what the user says / what the task looks like), never the workflow itself.

Anthropic official guidance (per `research_ai_config_sota.md` §Skills) says: "What it does + when to use it. Front-load keywords." This guidance does not address the skill-bypass failure mode that Superpowers has eval evidence for. The two are not strictly contradictory ("when to use it" can mean "trigger conditions") but the "what it does" half tempts maintainers to summarize the workflow inline.

OCX currently has 17 skill descriptions (audit §Skills). They have not been systematically audited against either policy. Per `audit_ai_config_ocx.md` the total description budget is ~3,335 chars (~0.4% of context) — well under the 1% Anthropic cap but unaudited for content discipline.

This is a one-way door at the medium level because reversing the decision requires re-auditing every description again against the opposite rule. However, an individual description that becomes too narrow can be widened in minutes.

## Decision Drivers

- Superpowers has empirical eval evidence (skill-bypass failure mode) that Anthropic docs do not address
- CSO affects every skill invocation across every session — highest per-invocation ROI intervention available
- Cost of the audit is ~30 minutes of edits across 17 files
- Reversibility is high at the individual-description level

## Industry Context & Research

**Research artifact:** `research_ai_config_superpowers.md` §Pattern 1 (CSO with empirical evidence); `research_ai_config_synthesis.md` §3 Divergent Axis 1; `research_ai_config_sota.md` §Skills (Anthropic position).

**Trending approach:** Path-scoped rules and progressive disclosure are already universal. Description-discipline is an emerging concern; Superpowers is the first public config with a named policy and eval evidence.

**Key insight:** When policy and evidence conflict, pick evidence. The alternative — retaining workflow-summary descriptions — has a known failure mode (skill bypass) that costs quality on every invocation. The CSO alternative has a different failure mode (description too narrow → skill stops auto-triggering) that is detectable and recoverable via a sanity check.

## Considered Options

### Option 1: Adopt CSO (Superpowers policy)

**Description:** Descriptions contain trigger phrasing only. Forbidden verbs: "dispatches", "runs", "iterates", "orchestrates", "performs", "executes", "handles". The SKILL.md body describes the workflow.

| Pros | Cons |
|---|---|
| Empirical evidence of reduced skill-bypass | Narrow descriptions may miss valid prompts |
| Structural test is tractable (regex against forbidden verbs) | One-time audit cost (~30 min × 17 skills) |
| Aligns with progressive disclosure principle | Contradicts Anthropic prose guidance |

### Option 2: Retain Anthropic "what + when" phrasing

**Description:** Descriptions summarize the skill's purpose and its use case. Current implicit policy.

| Pros | Cons |
|---|---|
| Aligns with Anthropic docs | Known failure mode (skill bypass) documented by Superpowers |
| Zero edit cost | Failure is silent — degraded quality without error signal |

### Option 3: Hybrid — purpose summary + trigger section

**Description:** Description includes a one-line purpose and a `when_to_use` field with trigger phrasing. Both fields contribute to the description budget.

| Pros | Cons |
|---|---|
| Captures both signals | Doubles description length → pressure on the 1% budget |
| Some alignment with each position | Superpowers evidence suggests the purpose summary is the failure trigger — mixing may still cause bypass |

## Decision Outcome

**Chosen Option:** Option 1 — adopt CSO.

**Rationale:** Superpowers has the only public empirical evidence on this axis; Anthropic docs do not address the failure mode. The mitigation cost (audit + one-time structural test) is low, and the reversibility is high at the individual-description level. Per synthesis §3 Divergent Axis 1, "empirical evidence beats documentation when they conflict."

### Quantified Impact

| Metric | Before | After | Notes |
|---|---|---|---|
| CSO-compliant skill descriptions | Unknown (unaudited) | 17/17 | Verified by `test_skill_descriptions_are_cso_compliant` |
| Skill description budget | ~3,335 chars (~0.4% of 200K) | ≤3,500 chars (buffer for `when_to_use` trigger bullets) | Still well under 1% Anthropic cap |
| Auto-invocation coverage for the 3 representative skills (`/commit`, `/architect`, `/swarm-plan`) | Currently triggers | Must continue to trigger | Verified by post-edit sanity check |

### Consequences

**Positive:**
- Eliminates skill-bypass failure mode across every invocation
- Structural test prevents regression on future skills
- Front-loads keywords (aligns with Anthropic budget truncation rules)

**Negative:**
- One-time edit cost across 17 files
- Risk of descriptions too narrow to trigger for valid prompts — mitigated by sanity check

**Risks:**
- **Description too narrow breaks auto-invocation.** Mitigation: pre-edit snapshot of which skills auto-trigger on which prompts; post-edit re-test against the same prompts; widen trigger phrasing if any skill stops firing.
- **Future contributor writes a workflow-summary description.** Mitigation: structural test fails on forbidden verbs.

## Description Template (post-decision)

```
description: [trigger phrase 1]. [trigger phrase 2]. [trigger phrase 3].
```

Each trigger phrase is an observable signal — user intent, task shape, file type, or keyword pattern. Never a verb describing the workflow. Example rewrite for `/architect`:

- **Before:** "Dispatches the architect worker to produce a design spec or ADR for complex architectural decisions in the OCX project."
- **After:** "Use when the task involves a design spec, an ADR, a one-way-door decision, or evaluating trade-offs between architectural approaches. Invoke before implementation begins when requirements span multiple subsystems."

## Implementation Plan

1. [ ] Snapshot pre-audit auto-invocation behavior on 3 prompts per skill (9 total — `/commit`, `/architect`, `/swarm-plan`)
2. [ ] Audit each of 17 skill descriptions; rewrite any containing forbidden verbs or workflow-summary prose
3. [ ] Add structural test `test_skill_descriptions_are_cso_compliant` (regex forbidden-verb check; max-length check; first-sentence-not-duplicated-in-body check)
4. [ ] Add structural test `test_skill_description_budget_under_cap` (sum < 50% of Anthropic 1% cap)
5. [ ] Post-edit re-test: same 9 prompts should still auto-trigger the same skills. Widen trigger phrasing for any regression.
6. [ ] Update `meta-ai-config.md` §Skills section to document the CSO policy explicitly, citing this ADR

## Validation

- [ ] 17/17 skills pass `test_skill_descriptions_are_cso_compliant`
- [ ] Total skill description budget remains < 50% of Anthropic 1% cap
- [ ] All 9 sanity-check prompts continue to auto-trigger the expected skill
- [ ] `meta-ai-config.md` cites this ADR under the Skills section

## Links

- `plan_ai_config_overhaul.md` Phase 2
- `research_ai_config_superpowers.md` §Pattern 1
- `research_ai_config_synthesis.md` §3 Divergent Axis 1
- `research_ai_config_sota.md` §Skills + §Skill Description Budget

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-04-19 | AI config overhaul planning | Initial draft |
