# Tier: low

Min-effort plan for Two-Way Door features (flag adds, fixture updates, small doc edits, single-subsystem tweaks ≤3 files). Keep contract-first TDD skeleton (Stub → Specify → Implement → Review) so `/swarm-execute` run plan unchanged — scale worker count + research depth down.

Load file via `Read` from `SKILL.md` after config announced.

## Phase 1: Discover (single worker)

Launch **1** `worker-explorer` (haiku) scoped to single subsystem target touches. No `worker-architecture-explorer` — scope small, one focused explorer enough.

**In parallel, read directly:**
- Relevant `subsystem-*.md` rule for feature area
- Specific code region named in target (if obvious from prompt)

GitHub discovery: if `/swarm-plan <N>` resolved to PR, file list = explicit scope input. Skip generic `list_issues` scan.

**Gate**: Current code region mapped; reusable utilities identified.

## Phase 2: Research (skip)

No `worker-researcher` launched. Orchestrator may inline brief product-context.md check if target touches positioning-sensitive behavior.

**Gate**: Skip decision logged in plan header (`Research: skipped — Two-Way Door`). If inline research surfaced surprise, upgrade tier (announce, ask user confirm via meta-plan gate).

## Phase 3: Classify (inline)

Confirm Two-Way Door scope inline in plan header. If discovery phase revealed feature *not* Two-Way Door (e.g., touches public API surface), **stop and re-run** with `/swarm-plan high "…"` — do not silently upgrade mid-pipeline.

## Phase 4: Design (inline)

Draft design inline in plan artifact. No `worker-architect`. Design still must include:

- **Component contracts**: single public function/type signature(s) touched, with expected behavior
- **User experience**: ≥1 action → expected outcome scenario
- **Error taxonomy**: failure modes that change from edit
- **Edge cases**: boundary conditions for new behavior

Trade-off analysis optional at this tier — single sentence stating chosen approach enough when change genuinely small.

## Phase 5: Decompose (inline)

Produce single Stub → Specify → Implement → Review cycle in plan. For ≤3 files may collapse to one task; fine.

## Phase 6: Review (single reviewer, single pass)

Launch **1** `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) on draft plan. No parallel adversarial panel. Single pass — no Round 2 loop.

Findings triaged:
- **Actionable** → orchestrator edits plan, re-runs reviewer once more (max total: 2 passes)
- **Deferred** → surfaced in handoff

**Codex plan review**: skipped this tier. Announcement confirms `codex: off`.

**Gate**: Plan ready for `/swarm-execute`.

## Artifacts

- `.claude/state/plans/plan_[feature].md` — required

No PRD, ADR, PR-FAQ this tier. If classifier inferred any, should have picked higher tier — re-run if needed.

## Handoff

Use standard handoff format from `SKILL.md`. Classification line:

```
- **Scope**: Small (Two-Way Door)
- **Tier**: low
- **Overlays**: (none)
```