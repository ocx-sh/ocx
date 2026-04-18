# Tier: low — /swarm-execute

Minimal-effort execute for Two-Way Door features (flag additions, fixture
updates, small doc edits, single-subsystem tweaks ≤3 files). Preserves
contract-first TDD (Stub → Specify → Implement → Review) so the handoff
contract with `/swarm-plan` stays intact — just scales worker count and
review breadth down.

Load this file via `Read` from `SKILL.md` after the config is announced.

## Phase 1: Discover

Read the plan artifact from `.claude/artifacts/` (or extract scope from
the free-text target). Parse the Stub / Specify / Implement / Review
steps. Identify the single subsystem touched; read its `subsystem-*.md`
rule inline.

**Gate**: Plan steps parsed; single subsystem identified.

## Phase 2: Stub

Launch **1** `worker-builder` (focus: `stubbing`, model: sonnet) to
create type signatures, traits, and function shells with
`unimplemented!()` / `raise NotImplementedError`. No business logic.

**Gate**: `cargo check` passes (types compile). For Python-only
changes: `uv run ruff check` / `uv run pyright` passes.

## Phase 3: Verify Architecture — skipped

Two-Way Door with ≤3 files. Skip the `worker-reviewer` architecture
pass. If the discover phase revealed the scope is actually larger,
stop and re-run `/swarm-execute high <plan>` instead of silently
upgrading.

**Gate**: Skip logged in announcement; proceed to Specify.

## Phase 4: Specify

Launch **1** `worker-tester` (focus: `specification`) to write **unit
tests** from the plan's component contracts. Acceptance tests are
optional at this tier — only add one when the change is user-visible.
Tests should fail against stubs.

**Gate**: Tests compile/parse and fail with `unimplemented` /
`NotImplementedError`.

## Phase 5: Implement

Launch **1** `worker-builder` (focus: `implementation`, model: sonnet)
to fill stub bodies until specification tests pass.

**Gate**: Subsystem verify succeeds (e.g., `task rust:verify`).

## Phase 6: Review-Fix Loop (1 round, minimal breadth)

Single-round loop. No iteration.

> **Reviewer model**: every `worker-reviewer` launch in this tier uses the resolved `--reviewer` overlay value (tier=low default `haiku`; escalated to `sonnet` when structural markers from `swarm-review/classify.md:48-61` are present). See `overlays.md` reviewer axis.

**Round 1 — Stage 1 (spec-compliance, scoped to changed files):**
Launch **1** `worker-reviewer` (focus: `spec-compliance`, phase:
`post-implementation`).

If actionable findings, run one builder fix pass + subsystem verify
before Stage 2.

**Round 1 — Stage 2 (quality only):**
Launch **1** `worker-reviewer` (focus: `quality`). No security /
performance / architecture reviewers at this tier.

Findings classified as actionable / deferred / suggest. If actionable
findings exist, run **one** builder fix pass + subsystem verify and
stop. No Round 2 at this tier — `--loop-rounds=1` means one pass.

**Codex code-diff review**: skipped. Announcement confirms `codex: off`.

**Gate**: No actionable findings remain OR one fix pass completed.
`task verify` passes on final state.

## Phase 7: Cross-Model Adversarial Pass — skipped

Two-Way Door — skip. If the user explicitly passed `--codex`, run the
pass anyway (user override). Otherwise log
`Cross-model gate skipped: tier=low default` and continue.

## Phase 8: Commit

Commit all changes on the feature branch with a conventional commit
message. Never push. Print the Deferred Findings summary even when
empty (confirms the pipeline ran to completion).

## Artifacts

- The plan artifact itself (updated in place if the Living Design
  Records protocol fires)
- Commit on the feature branch

No ADR, no new research artifacts at this tier. If the pipeline reveals
the need for either, stop and re-route through `/swarm-plan high`.
