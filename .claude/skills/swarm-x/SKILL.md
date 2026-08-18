---
name: swarm-x
description: Use to split a milestone into dependency-ordered issues, deliver each on a long-living branch via swarm-loop, then a bounded review and merge-ready PR. /swarm-x.
user-invocable: true
disable-model-invocation: false
argument-hint: "[milestone-or-task] [--branch=<long-living>] [--max-final=5]"
triggers:
  - "swarm x milestone"
  - "split into issues"
  - "milestone orchestration"
  - "deliver the whole milestone"
---

# swarm-x — Milestone-scale orchestration

Takes one high-level task (a GitHub milestone or a big feature), turns it into a
dependency-ordered set of issues, and delivers the whole thing on a **single
long-living feature branch** by repeating `swarm-loop` per issue. Ends with a
bounded milestone-completion review and a merge-ready PR.

This skill is the **outer loop**. It stays lean by delegating every issue's
heavy work to a per-issue Opus subagent running `swarm-loop`; this orchestrator
keeps only the master plan, the issue queue, and the current cursor in context.

## Inputs

- **milestone-or-task** — a milestone name/number, or a high-level description.
- `--branch=<name>` — long-living feature branch. Created from the current
  branch if absent.
- `--max-final=N` — cap on the closing milestone-completion loop (default 5).

## Phase A — Master plan (once, in this context)

1. **Ground-truth + split.** Read the milestone, its tracker issue, and any
   existing split plan. Verify claims against code (a `worker-explorer` fan-out
   for what's already shipped vs stubbed). Correct SOTA drift with research.
2. **Dependency DAG + order.** Build `issue → depends-on` edges; produce a
   linear execution order (topological, critical path first). Record which
   issues can start immediately (no unmet deps).
3. **Write the master plan** to `.claude/state/plans/plan_<milestone>.md` with a
   `## Status` block (schema in `meta-ai-config.md`) and set
   `.claude/state/current_plan.md`. The plan lists, per issue: branch name,
   tier, acceptance criteria, dep edges, doc surfaces, and gate.
4. **Create the long-living branch** from the current feature branch. Commit the
   plan + any config as a `chore:` commit. Do **not** push.

## Phase B — Per-issue loop (repeat in dependency order)

For each issue in order, when its dependencies are merged:

1. **Delegate to one Opus subagent** running `swarm-loop #<n> --onto=<branch>
   --max-review=3`. The subagent does design → plan → build → bounded review-fix
   loop → merge-commit → one-shot entirety-consistency pass, then returns a
   compact report. Keep the subagent's tool output out of this context.
2. **Integrate the report.** Update the master plan `## Status`, check off the
   issue in the tracker, record deferred findings. If the subagent reports a
   hard block (unmet dep, unreachable service), pause that issue and continue
   with any other unblocked issue; never fabricate completion.
3. Advance the cursor. Next issue.

> The entirety-consistency pass after each merge is part of `swarm-loop`
> (its Phase 6): one-shot, no loop, deferred to a subagent.

## Phase C — Milestone-completion loop (bounded, ≤ --max-final)

Once every issue is merged, run a bounded loop **focused on milestone
completion** (not per-diff):
```
round r in 1..=max_final:
  spawn subagent: /swarm-review max <branch> --base=main
    → focus: every acceptance criterion met, threat table only claims shipped
      defenses, docs/casts complete, no cross-issue seams left dangling
  if no actionable findings: break
  spawn subagent: /swarm-execute <fixes>    # apply, re-verify
```
Exit on clean review or cap. Oscillating findings auto-defer.

## Phase D — Merge-ready PR

1. Run the full gate on `<branch>`: `task verify` (basic) and the deep
   verification gate (e.g. `task rust:verify` + acceptance suite). Both must be
   green, or every red must be an honestly-documented, dependency-gated skip.
2. Open the PR (`feat/<branch>` → `main`) with a body enumerating the milestone
   goal, closed issues (`Closes #…`), deferred findings, and known
   dependency-gated gaps. **Do not push or merge** unless the human directs it;
   default is to prepare the PR body and stop.

## Guardrails

- One long-living branch; short-lived per-issue branches merged with `--no-ff`.
- Delegate maximally — this orchestrator holds state + cursor, not
  implementation detail. Prefer Opus subagents per issue.
- Bounded loops only (`--max-review=3` inner, `--max-final=5` closing).
- Never push to remote without explicit human go (Principle #6). Honest
  verification only (`quality-core.md`). Stop-and-report on genuine blocks.

## See also

- `.claude/skills/swarm-loop/SKILL.md` — the per-issue primitive.
- `.claude/skills/swarm-plan`, `swarm-execute`, `swarm-review` — tiered stages.
- `.claude/rules/workflow-swarm.md` — worker types, review-fix loop, tiers.
- `.claude/rules/workflow-feature.md` — contract-first TDD contract.
