---
paths:
  - ".claude/agents/**"
  - ".claude/skills/swarm-*/**"
  - ".claude/artifacts/**"
---

# Feature Development Workflow

Two workflows for implementing features, from planning through quality gates. Use the `plan.template.md` template from `.claude/templates/artifacts/` when creating plan artifacts. Referenced from [workflow-intent.md](./workflow-intent.md) when work is classified as a feature.

## Swarm Workflow (Primary)

The proven approach using subagent orchestration with **contract-first TDD**.

### Planning Phase
1. **Plan** — Human describes feature. Invoke `/architect` or `/swarm-plan`. `/swarm-plan` accepts a tier argument (`low | auto | high | max`) that scales worker count, research depth, and review adversariness to feature scope; `auto` (default) classifies from prompt signals. See `workflow-swarm.md` "Tier & Overlay Vocabulary" for details.
2. **Research** — Launch `worker-researcher` to scout the technology landscape. Persist findings as `.claude/artifacts/research_[topic].md`.
3. **Design** — Architect reads subsystem context rules + code + research artifacts, produces plan in `.claude/artifacts/`. Plan must include testable component contracts and user experience scenarios. At `max` tier (or with `--codex` overlay), `/swarm-plan` runs an optional Codex plan-artifact review as a cross-model final gate on the plan before handoff — see `workflow-swarm.md` "Codex Plan Review".
4. **Review** — Human reviews and approves plan.

### Execution Phase (Contract-First TDD)

Run `/swarm-execute` (optionally with tier `low | high | max`; `auto` default reads the plan header). Tier scales stub/impl builder model, Review-Fix Loop rounds, Stage 2 perspective breadth, and whether Codex code-diff review fires. See `workflow-swarm.md` "Tier & Overlay Vocabulary" for details.

5. **Stub** — `worker-builder` (focus: `stubbing`) creates type signatures, traits, function shells with `unimplemented!()` / `raise NotImplementedError`. Gate: `cargo check` passes.
6. **Verify Architecture** — `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) validates stubs match the design record. Gate: reviewer passes. *Optional for features touching ≤3 files.*
7. **Specify** — `worker-tester` (focus: `specification`) writes unit + acceptance tests from the design record. Tests fail against stubs. Gate: tests compile and fail with `unimplemented`.
8. **Implement** — `worker-builder` (focus: `implementation`) fills in stub bodies until all tests pass. Gate: subsystem verify succeeds (e.g., `task rust:verify` for Rust changes — see the Quality Gate section in each `subsystem-*.md` rule).
9. **Review-Fix Loop** — Apply the canonical Review-Fix Loop to the feature diff. See [`workflow-swarm.md`](./workflow-swarm.md#review-fix-loop). During the loop, run **subsystem verify** for the changed area, NOT full `task verify`.
10. **Cross-Model Adversarial Pass** — Cross-model adversarial pass is documented inside the canonical Review-Fix Loop (see [`workflow-swarm.md`](./workflow-swarm.md#review-fix-loop)). Flag: `--no-cross-model` on `/swarm-execute` to opt out.
11. **Commit** — All changes committed on feature branch with conventional commit message. Deferred findings printed as summary.
12. **Push** — Human decides when to push (CI has real cost).

## Agent Team Workflow (Experimental)

Enables `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`. Multiple Claude sessions coordinate via shared task lists, following contract-first TDD:

1. **Create team** — Human creates team with architect + builder + tester + reviewer teammates.
2. **Architect plans** — Reads subsystem context, produces plan with testable contracts and user experience scenarios.
3. **Builder stubs** — Creates type signatures, trait impls, function shells with `unimplemented!()`. Marks for reviewer.
4. **Reviewer verifies** — Validates stubs against design record (spec-compliance, post-stub). Reports issues or approves.
5. **Tester specifies** — Writes tests from the design record (not from stubs). Tests must fail against stubs. Marks for builder.
6. **Builder implements** — Fills in stub bodies until all tests pass. Marks for reviewer.
7. **Reviewer checks** — Diff-scoped review: spec-compliance, quality, security. Classifies findings as actionable or deferred. Reports actionable findings back to builder.
8. **Iterate** — Builder fixes actionable findings, reviewer re-checks only perspectives that had findings. Loop until no actionable findings remain (max 3 rounds). Deferred findings reported to human.
9. **Complete** — All teammates commit on feature branch. Human reviews deferred findings, decides to push.

### Team Sizing

- 3-5 teammates optimal (coordination overhead grows with size)
- ~5-6 tasks per teammate
- Avoid two teammates editing the same file

## Worker Assignment Guide

See `.claude/rules/workflow-swarm.md` for worker types, models, tools, and focus modes.

## Quality Gates

Run `task verify` (fmt check + clippy + build + unit tests + acceptance tests). See `.claude/rules/quality-core.md` for the canonical gate list.
