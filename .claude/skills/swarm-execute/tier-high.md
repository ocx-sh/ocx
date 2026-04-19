# Tier: high — /swarm-execute

Default tier for Medium-scope features (One-Way Door Medium: new
subcommand, new index format, new storage layout, 1–2 subsystems). This
is the tier that matches today's `/swarm-execute` behavior — the baseline
all existing callers get when they pass no explicit tier. Preserves
contract-first TDD (Stub → Specify → Implement → Review) with the full
3-round Review-Fix Loop.

Load this file via `Read` from `SKILL.md` after the config is announced.

## Phase 1: Discover

Read the plan artifact from `.claude/artifacts/`. Parse classification
(Scope, Reversibility, Tier, Overlays), Stub/Specify/Implement/Review
phases, and Subsystems Touched. Read the relevant `subsystem-*.md` rules
for all touched subsystems.

**Gate**: Plan steps parsed; all touched subsystems' rules read.

## Phase 2: Stub

Launch **1** `worker-builder` (focus: `stubbing`, model: sonnet) to
create type signatures, traits, and function shells with
`unimplemented!()` / `raise NotImplementedError`. No business logic.

**Gate**: `cargo check` passes (types compile).

## Phase 3: Verify Architecture

Launch **1** `worker-reviewer` (focus: `spec-compliance`, phase:
`post-stub`) to validate stubs against the design record: API surface
matches, module boundaries align, error types cover all failure modes.

*Optional for features touching ≤3 files — the classifier already
usually picks tier=low for those.*

**Gate**: Reviewer reports pass.

## Phase 4: Specify

Launch **1** `worker-tester` (focus: `specification`) to write **unit
tests + acceptance tests** from the plan's component contracts and user
experience sections — NOT from the stubs. Tests must fail against the
stubs.

**Gate**: Tests compile/parse and fail with `unimplemented` /
`NotImplementedError`.

## Phase 5: Implement

Launch **1** `worker-builder` (focus: `implementation`, model: sonnet by
default; **opus** when `--builder=opus` fires from the classifier for
cross-subsystem work) to fill stub bodies. All specification tests must
pass.

**Gate**: Subsystem verify succeeds (e.g., `task rust:verify` for Rust
changes). Run subsystem verify during the loop — NOT full `task verify`.

## Phase 6: Review-Fix Loop (up to 3 rounds, full breadth)

Protocol: see canonical in [`workflow-swarm.md`](../../rules/workflow-swarm.md#review-fix-loop). Tier-high overrides: `loop-rounds=3`; Stage 2 full (quality + security + perf + docs); Codex auto-on for One-Way Door plan signals.

> **Reviewer model**: every `worker-reviewer` launch in this tier uses the resolved `--reviewer` overlay value (tier=high default `sonnet`). See `overlays.md` reviewer axis.

Stage 1 runs `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`) and `worker-reviewer` (focus: `quality`, lens: test-coverage) **in a single message with multiple Agent tool calls** so they run concurrently; if actionable, one builder fix pass + subsystem verify before Stage 2. Stage 2 runs `quality`, `security` (if auth/input/crypto/signing touched), `performance` (if hot-path / async touched), and `worker-doc-reviewer` (if doc triggers match) **in a single message with multiple Agent tool calls** so they run concurrently.

Rounds 2–3: fresh `worker-builder` fixes actionable findings, subsystem verify, re-launch only perspectives with prior actionable findings. Codex code-diff fires when `--codex` is resolved on (user flag or classifier-inferred from plan `Reversibility: One-Way Door` / `Overlays: codex=on`); triage per `overlays.md`.

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

Commit all changes on the feature branch with a conventional commit
message. Never push. Deferred findings printed with the summary.

## Artifacts

- The plan artifact (updated in place if the Living Design Records
  protocol fires)
- Commit on the feature branch
- Optional: `research_[topic].md` if Implement uncovered a surprise
  worth persisting (rare at this tier)
