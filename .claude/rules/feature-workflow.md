# Feature Development Workflow

Two workflows for implementing features, from planning through quality gates.

## Swarm Workflow (Primary)

The proven approach using subagent orchestration:

1. **Plan** — Human describes feature. Invoke `/architect` or `/swarm-plan`.
2. **Research** — Launch `worker-researcher` to scout the technology landscape: trending tools, design patterns, adoption signals, competing approaches. Persist findings as `.claude/artifacts/research_[topic].md`. Check existing research artifacts for prior findings.
3. **Design** — Architect reads subsystem context rules + code + research artifacts, produces plan in `.claude/artifacts/`.
4. **Review** — Human reviews and approves plan.
5. **Execute** — Invoke `/swarm-execute` with the plan artifact. Orchestrator spawns:
   - `worker-builder` for implementation (parallel if independent tasks)
   - `worker-tester` for tests (after implementation)
   - `worker-reviewer` for quality check (after tests)
6. **Gate** — Quality gates: `task verify` (fmt + clippy + build + unit tests + acceptance tests).
7. **Commit** — All changes committed on feature branch with conventional commit message.
8. **Push** — Human decides when to push (CI has real cost).

## Agent Team Workflow (Experimental)

Enables `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`. Multiple Claude sessions coordinate via shared task lists:

1. **Create team** — Human creates team with architect + builder + tester + reviewer teammates.
2. **Architect plans** — Reads subsystem context, creates tasks for builder teammate.
3. **Builder implements** — Picks up tasks, writes code, marks for tester.
4. **Tester validates** — Writes tests, runs them, marks for reviewer.
5. **Reviewer checks** — Verifies OCX patterns, quality, security. Reports issues back.
6. **Iterate** — Teammates communicate via mailbox to resolve issues.
7. **Complete** — All teammates commit on feature branch. Human reviews, decides to push.

### Team Sizing

- 3-5 teammates optimal (coordination overhead grows with size)
- ~5-6 tasks per teammate
- Avoid two teammates editing the same file

## Worker Assignment Guide

See `.claude/rules/swarm-workers.md` for worker types, models, tools, and focus modes.

## Quality Gates

Run `task verify` (fmt check + clippy + build + unit tests + acceptance tests). See `.claude/rules/code-quality.md` for the canonical gate list.
