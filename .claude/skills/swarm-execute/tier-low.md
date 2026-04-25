# Tier: low — /swarm-execute

Minimal-effort execute for Two-Way Door features (flag additions, fixture updates, small doc edits, single-subsystem tweaks ≤3 files). Preserves contract-first TDD (Stub → Specify → Implement → Review) so handoff contract with `/swarm-plan` stay intact — scale worker count and review breadth down.

Load this file via `Read` from `SKILL.md` after config announced.

## Phase 1: Discover

Read plan artifact from `.claude/artifacts/` (or extract scope from free-text target). Parse Stub / Specify / Implement / Review steps. Identify single subsystem touched; read its `subsystem-*.md` rule inline.

**Gate**: Plan steps parsed; single subsystem identified.

## Phase 2: Stub

Launch **1** `worker-builder` (focus: `stubbing`, model: sonnet) to create type signatures, traits, function shells with `unimplemented!()` / `raise NotImplementedError`. No business logic.

**Gate**: `cargo check` passes (types compile). Python-only changes: `uv run ruff check` / `uv run pyright` passes.

## Phase 3: Verify Architecture — skipped

Two-Way Door with ≤3 files. Skip `worker-reviewer` architecture pass. If discover phase revealed scope actually larger, stop and re-run `/swarm-execute high <plan>` instead of silent upgrade.

**Gate**: Skip logged in announcement; proceed to Specify.

## Phase 4: Specify

Launch **1** `worker-tester` (focus: `specification`) to write **unit tests** from plan's component contracts. Acceptance tests optional this tier — only add when change user-visible. Tests fail against stubs.

**Gate**: Tests compile/parse and fail with `unimplemented` / `NotImplementedError`.

## Phase 5: Implement

Launch **1** `worker-builder` (focus: `implementation`, model: sonnet) to fill stub bodies until specification tests pass.

**Gate**: Subsystem verify succeeds (e.g., `task rust:verify`).

## Phase 6: Review-Fix Loop (1 round, minimal breadth)

Protocol: see canonical in [`workflow-swarm.md`](../../rules/workflow-swarm.md#review-fix-loop). Tier-low overrides: `loop-rounds=1`; Stage 2 minimal (quality only); no Codex.

> **Reviewer model**: every `worker-reviewer` launch this tier uses resolved `--reviewer` overlay value (tier=low default `haiku`; escalated to `sonnet` when structural markers from `swarm-review/classify.md:48-61` present). See `overlays.md` reviewer axis.

Stage 1 launches **1** `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`); if actionable, one builder fix pass + subsystem verify. Stage 2 launches **1** `worker-reviewer` (focus: `quality`); if actionable, one builder fix pass + subsystem verify. No Round 2 — `--loop-rounds=1` means one pass.

**Gate**: No actionable findings remain OR one fix pass done. `task verify` passes on final state.

## Phase 7: Cross-Model Adversarial Pass — skipped

Two-Way Door — skip. If user explicit pass `--codex`, run pass anyway (user override). Else log `Cross-model gate skipped: tier=low default` and continue.

## Phase 8: Commit

Commit all changes on feature branch with conventional commit message. Never push. Print Deferred Findings summary even when empty (confirms pipeline ran to completion).

## Artifacts

- Plan artifact itself (updated in place if Living Design Records protocol fires)
- Commit on feature branch

No ADR, no new research artifacts this tier. If pipeline reveals need for either, stop and re-route through `/swarm-plan high`.