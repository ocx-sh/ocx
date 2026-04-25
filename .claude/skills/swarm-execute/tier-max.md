# Tier: max — /swarm-execute

Full kitchen sink for Large-scope features (One-Way Door High: new crate, breaking API, cross-subsystem refactor, protocol change, content-addressed storage layout change). Preserves contract-first TDD. Adds mandatory opus builders, broader architecture + SOTA + CLI-UX review perspectives, mandatory Codex code-diff review as final gate before commit.

Load via `Read` from `SKILL.md` after config announced.

**Requires plan artifact**: tier=max expects target = plan from `/swarm-plan max`. Free-text `max` targets stop, route through `/swarm-plan max` first.

**Auto meta-plan preview**: when tier resolves to `max`, `SKILL.md` auto-fires meta-plan gate (opt out with `--no-dry-run`). Cost transparency — max-tier runs expensive, catch misclassifications before tokens burn.

## Phase 1: Discover

Read plan artifact. Parse classification, all phases, Subsystems Touched. Read `subsystem-*.md` rules for **all** touched subsystems + any adjacent ones possibly affected.

In parallel, re-read:
- ADR (`.claude/artifacts/adr_*.md`) if exists for this feature
- Related research artifacts (`.claude/artifacts/research_*.md`)
- `.claude/rules/product-context.md` (plan may imply positioning shifts implementation must not violate)

**Gate**: Plan + ADR + research + subsystem rules all read.

## Phase 2: Stub

Launch **1** `worker-builder` (focus: `stubbing`, model: **opus** — mandatory at this tier for sound architectural scaffolding on new crates / cross-subsystem boundaries).

**Gate**: `cargo check` passes across full workspace (not just touched crate — cross-subsystem implications must surface immediately).

## Phase 3: Verify Architecture (reviewer + architect)

Launch **in single message with multiple Agent tool calls** so run concurrently:
- **1** `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`)
- **1** `worker-architect` — reviews stubs against ADR: boundaries honored? Trade-offs implemented as ADR specified? Any subsystem boundary violations?

Architect findings here treated as first-class — if stubs diverge from ADR, **stop and re-stub** before writing any tests.

**Gate**: Both reviewer and architect report pass. ADR compliance confirmed.

## Phase 4: Specify

Launch **1** `worker-tester` (focus: `specification`, model: **opus** — mandatory at tier=max, overrides any explicit `--tester=sonnet`) with instruction to cover **edge cases exhaustively**: boundary conditions, concurrent access, failure modes, protocol-level corner cases, cross-subsystem interactions. Unit + acceptance tests both required.

**Gate**: Tests compile/parse, fail with `unimplemented` / `NotImplementedError`. Coverage matches design record's edge-case list verbatim.

## Phase 5: Implement

Launch **1** `worker-builder` (focus: `implementation`, model: **opus** — mandatory, overrides `--builder=sonnet` if user passed it) to fill stub bodies. All specification tests must pass.

**Gate**: `task verify` succeeds (full verify at this tier, not just subsystem — max-tier changes often have cross-subsystem implications).

## Phase 6: Review-Fix Loop (up to 3 rounds, adversarial breadth)

Protocol: see canonical in [`workflow-swarm.md`](../../rules/workflow-swarm.md#review-fix-loop). Tier-max overrides: `loop-rounds=3`; Stage 2 adversarial (+ architect + SOTA + cli-ux); Codex mandatory.

> **Reviewer model**: every `worker-reviewer` launch in this tier uses resolved `--reviewer` overlay value (tier=max default `sonnet`; escalated to `opus` when `--breadth=adversarial` fires). See `overlays.md` reviewer axis.

Stage 1 matches tier-high: `worker-reviewer` (spec-compliance, post-implementation) + `worker-reviewer` (quality, lens: test-coverage). Stage 2 adds to `full` set:
- `worker-reviewer` (focus: `quality`) — CLI-UX lens when touching command surface
- `worker-reviewer` (focus: `security`)
- `worker-reviewer` (focus: `performance`)
- `worker-doc-reviewer` — model per resolved `--doc-reviewer` overlay (`sonnet` default; `haiku` when narrow-scope doc trigger fires — see `overlays.md` doc-reviewer axis)
- **`worker-architect`** — ADR-compliance perspective
- **`worker-researcher`** — SOTA gap check

Rounds 2–3 follow canonical selective re-review with oscillation-auto-defer (architect ↔ reviewer oscillation = known max-tier pattern — both perspectives captured in deferred entry).

**Codex code-diff review — mandatory final gate.** After Claude loop converges, invoke `codex-adversary` with scope `code-diff` on branch diff. One-shot. Triage per `overlays.md`: Actionable → one-shot `worker-builder` (focus: `implementation`, model: opus) fix pass, gate `task verify`; Deferred → Deferred Findings; Stated-convention / trivia → dropped with counts. If one-shot fix pass fails `task verify`, revert and promote all Codex findings to deferred.

Unavailable path: at max-tier this = **gate, not blocker** — surface skip prominently in commit summary.

**Gate**: `task verify` passes on final state. All deferred findings (Claude + Codex) documented.

## Phase 7: Commit

Commit all changes on feature branch with conventional commit message. Never push. Deferred findings printed with summary. At max-tier, explicitly surface:

- Whether Codex gate ran or skipped (with reason)
- Any ADR-compliance concerns architect flagged as deferred
- Any SOTA gaps researcher flagged as deferred

## Artifacts

- Plan artifact (updated in place if Living Design Records fires)
- Possibly follow-up ADR if implementation revealed decision original ADR didn't cover — escalate to `/swarm-plan max` rather than inlining
- Commit on feature branch