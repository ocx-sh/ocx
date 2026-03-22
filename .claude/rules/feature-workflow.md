# Feature Development Workflow

Two workflows for implementing features, from planning through quality gates.

## Swarm Workflow (Primary)

The proven approach using subagent orchestration with **contract-first TDD**.

### Planning Phase
1. **Plan** — Human describes feature. Invoke `/architect` or `/swarm-plan`.
2. **Research** — Launch `worker-researcher` to scout the technology landscape. Persist findings as `.claude/artifacts/research_[topic].md`.
3. **Design** — Architect reads subsystem context rules + code + research artifacts, produces plan in `.claude/artifacts/`. Plan must include testable component contracts and user experience scenarios.
4. **Review** — Human reviews and approves plan.

### Execution Phase (Contract-First TDD)
5. **Stub** — `worker-builder` (focus: `stubbing`) creates type signatures, traits, function shells with `unimplemented!()` / `raise NotImplementedError`. Gate: `cargo check` passes.
6. **Verify Architecture** — `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) validates stubs match the design record. Gate: reviewer passes. *Optional for features touching ≤3 files.*
7. **Specify** — `worker-tester` (focus: `specification`) writes unit + acceptance tests from the design record. Tests fail against stubs. Gate: tests compile and fail with `unimplemented`.
8. **Implement** — `worker-builder` (focus: `implementation`) fills in stub bodies until all tests pass. Gate: `task verify` succeeds.
9. **Review** — `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`) verifies design↔tests↔code consistency, then (focus: `quality`) for code quality. *Spec-compliance review optional for features touching ≤3 files.*
10. **Commit** — All changes committed on feature branch with conventional commit message.
11. **Push** — Human decides when to push (CI has real cost).

## Agent Team Workflow (Experimental)

Enables `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`. Multiple Claude sessions coordinate via shared task lists, following contract-first TDD:

1. **Create team** — Human creates team with architect + builder + tester + reviewer teammates.
2. **Architect plans** — Reads subsystem context, produces plan with testable contracts and user experience scenarios.
3. **Builder stubs** — Creates type signatures, trait impls, function shells with `unimplemented!()`. Marks for reviewer.
4. **Reviewer verifies** — Validates stubs against design record (spec-compliance, post-stub). Reports issues or approves.
5. **Tester specifies** — Writes tests from the design record (not from stubs). Tests must fail against stubs. Marks for builder.
6. **Builder implements** — Fills in stub bodies until all tests pass. Marks for reviewer.
7. **Reviewer checks** — Verifies spec-compliance (post-implementation), then quality, security. Reports issues back.
8. **Iterate** — Teammates communicate via mailbox to resolve issues.
9. **Complete** — All teammates commit on feature branch. Human reviews, decides to push.

### Team Sizing

- 3-5 teammates optimal (coordination overhead grows with size)
- ~5-6 tasks per teammate
- Avoid two teammates editing the same file

## Worker Assignment Guide

See `.claude/rules/swarm-workers.md` for worker types, models, tools, and focus modes.

## Quality Gates

Run `task verify` (fmt check + clippy + build + unit tests + acceptance tests). See `.claude/rules/code-quality.md` for the canonical gate list.
