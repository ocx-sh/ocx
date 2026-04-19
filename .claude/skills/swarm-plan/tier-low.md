# Tier: low

Minimal-effort planning for Two-Way Door features (flag additions,
fixture updates, small doc edits, single-subsystem tweaks ≤3 files).
Preserves the contract-first TDD skeleton (Stub → Specify → Implement →
Review) so `/swarm-execute` can still run the plan unchanged — just
scales the worker count and research depth down.

Load this file via `Read` from `SKILL.md` after the config is
announced.

## Phase 1: Discover (single worker)

Launch **1** `worker-explorer` (haiku) scoped to the single subsystem
the target touches. No `worker-architecture-explorer` — the scope is
small enough that one focused explorer suffices.

**In parallel, read directly:**
- Relevant `subsystem-*.md` rule for the feature area
- The specific code region named in the target (if obvious from the
  prompt)

GitHub discovery: if `/swarm-plan <N>` resolved to a PR, the file list
becomes explicit scope input. Skip the generic `list_issues` scan.

**Gate**: Current code region mapped; reusable utilities identified.

## Phase 2: Research (skip)

No `worker-researcher` launched. Orchestrator may inline a brief
product-context.md check if the target touches positioning-sensitive
behavior.

**Gate**: Decision to skip logged in the plan header (`Research: skipped
— Two-Way Door`). If inline research surfaced a surprise, upgrade the
tier (announce, ask user to confirm via meta-plan gate).

## Phase 3: Classify (inline)

Confirm the Two-Way Door scope inline in the plan header. If the
discovery phase revealed the feature is *not* Two-Way Door after all
(e.g., it touches a public API surface), **stop and re-run** with
`/swarm-plan high "…"` — do not silently upgrade mid-pipeline.

## Phase 4: Design (inline)

Draft the design inline in the plan artifact. No `worker-architect`.
The design still must include:

- **Component contracts**: the single public function/type signature(s)
  touched, with expected behavior
- **User experience**: at least one action → expected outcome scenario
- **Error taxonomy**: failure modes that change as a result of the edit
- **Edge cases**: boundary conditions for the new behavior

Trade-off analysis is optional at this tier — a single sentence stating
the chosen approach is enough when the change is genuinely small.

## Phase 5: Decompose (inline)

Produce a single Stub → Specify → Implement → Review cycle in the plan.
For ≤3 files this may collapse into one task; that's fine.

## Phase 6: Review (single reviewer, single pass)

Launch **1** `worker-reviewer` (focus: `spec-compliance`, phase:
`post-stub`) on the draft plan. No parallel adversarial panel. Single
pass — no Round 2 loop.

Findings triaged as:
- **Actionable** → orchestrator edits the plan, re-runs the reviewer
  once more (max total: 2 passes)
- **Deferred** → surfaced in handoff

**Codex plan review**: skipped at this tier. Announcement confirms
`codex: off`.

**Gate**: Plan ready for `/swarm-execute`.

## Artifacts

- `.claude/state/plans/plan_[feature].md` — required

No PRD, no ADR, no PR-FAQ at this tier. If the classifier inferred any
of those, it should have picked a higher tier — re-run if needed.

## Handoff

Use the standard handoff format from `SKILL.md`. Classification line:

```
- **Scope**: Small (Two-Way Door)
- **Tier**: low
- **Overlays**: (none)
```
