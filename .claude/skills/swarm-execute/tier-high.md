# Tier: high — /swarm-execute

Default tier for Medium-scope features (One-Way Door Medium: new subcommand, new index format, new storage layout, 1–2 subsystems). Matches today's `/swarm-execute` baseline — what existing callers get when no explicit tier passed. Preserves contract-first TDD (Stub → Specify → Implement → Review) with full 3-round Review-Fix Loop.

Load via `Read` from `SKILL.md` after config announced.

## Phase 1: Discover

Read plan artifact from `.claude/artifacts/`. Parse classification (Scope, Reversibility, Tier, Overlays), Stub/Specify/Implement/Review phases, Subsystems Touched. Read relevant `subsystem-*.md` rules for all touched subsystems.

**Gate**: Plan steps parsed; all touched subsystems' rules read.

## Phase 2: Stub

Launch **1** `worker-builder` (focus: `stubbing`, model: sonnet) to create type signatures, traits, function shells with `unimplemented!()` / `raise NotImplementedError`. No business logic.

**Gate**: `cargo check` passes (types compile).

## Phase 3: Verify Architecture

Launch **1** `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) to validate stubs against design record: API surface matches, module boundaries align, error types cover all failure modes.

*Optional for features touching ≤3 files — classifier usually picks tier=low for those.*

**Gate**: Reviewer reports pass.

## Phase 4: Specify

Launch **1** `worker-tester` (focus: `specification`) to write **unit tests + acceptance tests** from plan's component contracts and user experience sections — NOT from stubs. Tests must fail against stubs.

**Gate**: Tests compile/parse and fail with `unimplemented` / `NotImplementedError`.

## Phase 5: Implement

Launch **1** `worker-builder` (focus: `implementation`, model: sonnet default; **opus** when `--builder=opus` fires from classifier for cross-subsystem work) to fill stub bodies. All specification tests must pass.

**Gate**: Subsystem verify succeeds (e.g., `task rust:verify` for Rust changes). Run subsystem verify during loop — NOT full `task verify`.

## Phase 6: Review-Fix Loop (up to 3 rounds, full breadth)

Protocol: see canonical in [`workflow-swarm.md`](../../rules/workflow-swarm.md#review-fix-loop). Tier-high overrides: `loop-rounds=3`; Stage 2 full (quality + security + perf + docs); Codex auto-on for One-Way Door plan signals.

> **Reviewer model**: every `worker-reviewer` launch this tier uses resolved `--reviewer` overlay value (tier=high default `sonnet`). See `overlays.md` reviewer axis.

Stage 1 runs `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`) and `worker-reviewer` (focus: `quality`, lens: test-coverage) **in single message with multiple Agent tool calls** so concurrent; if actionable, one builder fix pass + subsystem verify before Stage 2. Stage 2 runs `quality`, `security` (if auth/input/crypto/signing touched), `performance` (if hot-path / async touched), `worker-doc-reviewer` (if doc triggers match) **in single message with multiple Agent tool calls** so concurrent.

Rounds 2–3: fresh `worker-builder` fixes actionable findings, subsystem verify, re-launch only perspectives with prior actionable findings. Codex code-diff fires when `--codex` resolved on (user flag or classifier-inferred from plan `Reversibility: One-Way Door` / `Overlays: codex=on`); triage per `overlays.md`.

Print deferred findings summary at loop exit:

```
## Deferred Findings

### Auto-fixed (N rounds)
- [Finding]: [What was changed]

### Deferred: Requires human judgment
- [Finding]: [Why human judgment is needed]

### Cross-Model Adversarial (Codex)
- Auto-fixed (N): [finding → what was changed]
- Deferred (M): [finding → why human judgment is needed]
- Dropped (K trivia, L stated-convention)

### Suggestions (not actioned)
- [Finding]: [Optional improvement]
```

**Gate**: `task verify` passes on final state. Deferred findings documented.

## Phase 7: Commit

Commit all changes on feature branch with conventional commit message. Never push. Deferred findings printed with summary.

## Artifacts

- Plan artifact (updated in place if Living Design Records protocol fires)
- Commit on feature branch
- Optional: `research_[topic].md` if Implement uncovered surprise worth persisting (rare this tier)