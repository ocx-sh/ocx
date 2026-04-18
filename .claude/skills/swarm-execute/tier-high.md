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

Bounded review-fix cycle. Diff-scoped (findings must relate to
`git diff main...HEAD --name-only`), severity-gated (Block/Warn drive
the loop; Suggest goes directly to deferred).

> **Reviewer model**: every `worker-reviewer` launch in this tier uses the resolved `--reviewer` overlay value (tier=high default `sonnet`). See `overlays.md` reviewer axis.

### Round 1 — Stage 1 (spec-compliance + test-coverage, scoped to changed files)

Launch in parallel:
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`)
- `worker-reviewer` (focus: `quality`, lens: test-coverage) — checks that
  new code has tests, bug fixes have regression tests, edge cases covered

If Stage 1 has actionable findings, run **one** builder fix pass +
subsystem verify before Stage 2. Rationale: polishing code that doesn't
meet the design record or lacks tests wastes effort.

### Round 1 — Stage 2 (full perspectives, parallel)

Launch in parallel (only applicable perspectives):
- `worker-reviewer` (focus: `quality`)
- `worker-reviewer` (focus: `security`) — if touching auth, input
  handling, crypto, external data, signing
- `worker-reviewer` (focus: `performance`) — if touching hot paths or
  async code
- `worker-doc-reviewer` — if doc triggers match changed files

Each reviewer classifies findings:
- **Actionable** (Block/Warn) — fixed without human input
- **Deferred** — requires human judgment
- **Suggest** — optional improvements → deferred summary directly

### Rounds 2–3 (selective re-review)

1. `worker-builder` (fresh subagent) fixes all actionable findings
2. Run subsystem verify — must pass
3. Re-launch **only** perspectives that had actionable findings
4. Drop perspectives that now report clean

Terminate on: converged (no actionable findings), budget exhausted
(3 rounds), or oscillation (same findings as previous round → defer).

### Codex code-diff review (optional at this tier)

If `--codex` fires (user flag or classifier-inferred from plan
`Reversibility: One-Way Door` or `Overlays: codex=on`), run a single
`codex-adversary` pass in `code-diff` scope against the branch diff
after the Claude loop converges. One-shot, no loop. Triage per
`overlays.md`:

- Actionable → one-shot `worker-builder` fix pass; gate: `task verify`
- Deferred → commit summary
- Stated-convention / trivia → dropped, counts reported

Unavailable path: log `Cross-model gate skipped: <reason>` and continue.

### Loop exit

Run `task verify` once as ground truth. Print deferred findings
summary:

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

**Gate**: `task verify` passes on final state. Deferred findings
documented.

## Phase 7: Commit

Commit all changes on the feature branch with a conventional commit
message. Never push. Deferred findings printed with the summary.

## Artifacts

- The plan artifact (updated in place if the Living Design Records
  protocol fires)
- Commit on the feature branch
- Optional: `research_[topic].md` if Implement uncovered a surprise
  worth persisting (rare at this tier)
