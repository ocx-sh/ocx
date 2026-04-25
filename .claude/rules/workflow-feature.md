---
paths:
  - ".claude/agents/**"
  - ".claude/skills/swarm-*/**"
  - ".claude/artifacts/**"
---

# Feature Development Workflow

Two workflows for feature impl, planning through quality gates. Use `plan.template.md` from `.claude/templates/artifacts/` for plan artifacts. Referenced from [workflow-intent.md](./workflow-intent.md) when work classified as feature.

## Swarm Workflow (Primary)

Proven approach: subagent orchestration + **contract-first TDD**.

### Planning Phase
1. **Plan** — Human describe feature. Invoke `/architect` or `/swarm-plan`. `/swarm-plan` accepts tier arg (`low | auto | high | max`) scaling worker count, research depth, review adversariness to scope; `auto` (default) classifies from prompt signals. See `workflow-swarm.md` "Tier & Overlay Vocabulary".
2. **Research** — Launch `worker-researcher` to scout tech landscape. Persist as `.claude/artifacts/research_[topic].md`.
3. **Design** — Architect reads subsystem context rules + code + research artifacts, writes plan in `.claude/artifacts/`. Plan must include testable component contracts + UX scenarios. At `max` tier (or `--codex` overlay), `/swarm-plan` runs optional Codex plan-artifact review as cross-model final gate before handoff — see `workflow-swarm.md` "Codex Plan Review".
4. **Review** — Human review + approve plan.

### Execution Phase (Contract-First TDD)

Run `/swarm-execute` (optional tier `low | high | max`; `auto` default reads plan header). Tier scales stub/impl builder model, Review-Fix Loop rounds, Stage 2 perspective breadth, Codex code-diff review trigger. See `workflow-swarm.md` "Tier & Overlay Vocabulary".

5. **Stub** — `worker-builder` (focus: `stubbing`) creates type sigs, traits, function shells with `unimplemented!()` / `raise NotImplementedError`. Gate: `cargo check` passes.
6. **Verify Architecture** — `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) validates stubs match design record. Gate: reviewer passes. *Optional for features touching ≤3 files.*
7. **Specify** — `worker-tester` (focus: `specification`) writes unit + acceptance tests from design record. Tests fail against stubs. Gate: tests compile + fail with `unimplemented`.
8. **Implement** — `worker-builder` (focus: `implementation`) fills stub bodies until all tests pass. Gate: subsystem verify succeeds (e.g., `task rust:verify` for Rust — see Quality Gate section in each `subsystem-*.md` rule).
9. **Review-Fix Loop** — Apply canonical Review-Fix Loop to feature diff. See [`workflow-swarm.md`](./workflow-swarm.md#review-fix-loop). Run **subsystem verify** for changed area, NOT full `task verify`.
10. **Cross-Model Adversarial Pass** — Documented inside canonical Review-Fix Loop (see [`workflow-swarm.md`](./workflow-swarm.md#review-fix-loop)). Opt-out flag: `--no-cross-model` on `/swarm-execute`.
11. **Commit** — All changes committed on feature branch, conventional commit message. Deferred findings printed as summary.
12. **Push** — Human decides when to push (CI cost real).

## Agent Team Workflow (Experimental)

Enable `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`. Multiple Claude sessions coordinate via shared task lists, contract-first TDD:

1. **Create team** — Human creates team: architect + builder + tester + reviewer.
2. **Architect plans** — Reads subsystem context, writes plan with testable contracts + UX scenarios.
3. **Builder stubs** — Creates type sigs, trait impls, function shells with `unimplemented!()`. Marks for reviewer.
4. **Reviewer verifies** — Validates stubs against design record (spec-compliance, post-stub). Reports issues or approves.
5. **Tester specifies** — Writes tests from design record (not stubs). Tests must fail against stubs. Marks for builder.
6. **Builder implements** — Fills stub bodies until all tests pass. Marks for reviewer.
7. **Reviewer checks** — Diff-scoped review: spec-compliance, quality, security. Classifies findings as actionable or deferred. Reports actionable findings to builder.
8. **Iterate** — Builder fixes actionable findings, reviewer re-checks only perspectives with findings. Loop until no actionable findings (max 3 rounds). Deferred findings reported to human.
9. **Complete** — All teammates commit on feature branch. Human reviews deferred findings, decides push.

### Team Sizing

- 3-5 teammates optimal (coordination overhead grows w/ size)
- ~5-6 tasks per teammate
- Avoid two teammates editing same file

## Worker Assignment Guide

See `.claude/rules/workflow-swarm.md` for worker types, models, tools, focus modes.

## Plan Status Tracking

Every plan in `.claude/state/plans/plan_*.md` carries a `## Status` block at the top (after H1, before first content section). Block fields: `Plan`, `Active phase`, `Step`, `Last update`. Read+mutated by `/swarm-plan` (init), `/swarm-execute` (phase entry/advance), `/swarm-review` (round/verdict), `/commit` (Last update), `/finalize` (refuse if active, mark `finalized` on done), `/next` (read primary signal, state-fix fallback). Global pointer `.claude/state/current_plan.md` (gitignored) names the active plan. Schema + mutation table → [`meta-ai-config.md`](./meta-ai-config.md) "Plan Status Protocol".

## Quality Gates

Run `task verify` (fmt check + clippy + build + unit tests + acceptance tests). See `.claude/rules/quality-core.md` for canonical gate list.