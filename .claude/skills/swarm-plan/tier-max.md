# Tier: max

Full kitchen sink for Large-scope features (One-Way Door High: new
crate, breaking API, cross-subsystem refactor, protocol change,
content-addressed storage layout change). Preserves contract-first TDD.
Adds mandatory Opus architect, mandatory 3-axis research, and mandatory
Codex plan review as the final gate before handoff.

Load this file via `Read` from `SKILL.md` after the config is
announced.

**Auto meta-plan preview**: when tier resolves to `max`, `SKILL.md`
step 5 auto-fires the meta-plan gate (users can opt out with
`--no-dry-run`). The preview is a Cost Transparency measure — max-tier
runs are expensive and this catches misclassifications before tokens
burn.

## Phase 1: Discover (parallel, full)

Same as tier-high:
- **1** `worker-architecture-explorer` (sonnet) — maps current
  architecture, traces dependencies, finds reusable code and patterns
- **2–4** `worker-explorer` agents (haiku) — each scoped to a relevant
  subsystem

**In parallel, read directly:**
- `.claude/rules/product-context.md` — product positioning and vision
- `.claude/artifacts/` — related ADRs, plans, prior research
- Relevant `subsystem-*.md` rules for every subsystem touched

GitHub discovery: same as tier-high. PR file list feeds Discover scope
directly.

**Gate**: Full architecture map produced; all prior ADRs / plans / PRDs
in the domain enumerated.

## Phase 2: Research (parallel, 3 axes — mandatory)

Launch **3** `worker-researcher` agents **in a single message with multiple Agent tool calls** so they run concurrently, one per axis:
- **Technology / tools** — trending libraries, competing tools
- **Design patterns** — emerging approaches, industry best practices,
  known pitfalls
- **Domain knowledge** — OCI spec evolution, security considerations,
  algorithm choices

Each researcher produces an opinionated recommendation with trend
analysis, adoption signals, and citations. Persist findings as
`.claude/artifacts/research_[topic].md` — mandatory at this tier,
substantial findings are the norm.

**Gate**: Three research artifacts persisted. SOTA and adoption signals
checked across all axes.

## Phase 3: Classify (sequential)

Confirm **One-Way Door High** in the plan header. If the classification
doesn't hold up (the feature is actually Medium), downgrade the tier
and re-run: `/swarm-plan high "…"`. Don't silently underspec a Large
feature, and don't silently overspec a Medium one.

Required artifacts at this tier:
- `pr_faq_[feature].md` (customer narrative)
- `prd_[feature].md` (product requirements)
- `adr_[decision].md` (architecture decision record — mandatory)
- `plan_[feature].md` (executable phases)

Templates at `.claude/templates/artifacts/`.

**Gate**: Scope, reversibility, and all four artifacts listed in plan
header.

## Phase 4: Design (worker-architect opus, mandatory ADR)

Launch **1** `worker-architect` with **model=opus** (mandatory at this
tier — overrides any `--architect=` flag to a weaker model). Produces
both an ADR and (when the scope warrants it) a system-design document.

**Design must include:**
- **Component contracts**: public API (types, traits, function
  signatures) and expected behavior per component
- **User experience scenarios**: action → expected outcome → error
  cases for each user-facing behavior
- **Error taxonomy**: all documented failure modes with remediation
- **Edge cases**: boundary conditions and corner cases enumerated
- **Trade-off analysis**: min 3 options at this tier (not 2), weighted
  criteria, risks, reversibility, recommendation with rationale
- **Migration / rollout plan**: how existing code / data / users move
  to the new shape without breakage (or with explicit breakage
  communicated)

**Gate**: ADR committed, design artifacts exist, contracts are
testable.

## Phase 5: Decompose (sequential)

Break the design into right-sized tasks that support the contract-first
TDD cycle: **Stub → Specify → Implement → Review**. Each task must map
to that cycle so `/swarm-execute` can run unchanged. Dependencies
between tasks form a graph. Critical path identified. Parallelizable
tasks flagged for `/swarm-execute`.

**Gate**: `plan_[feature].md` contains executable phases (Stub → Verify
→ Specify → Implement → Review) that `/swarm-execute` can run without
further decomposition.

## Phase 6: Review (parallel Claude panel + mandatory Codex)

**Round 1 — full Claude panel (launched in a single message with multiple Agent tool calls so they run concurrently):**
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) —
  Are contracts testable? Do they match the user experience section?
- `worker-architect` — Are trade-offs honest? Alternatives considered?
  Any subsystem boundary violations?
- `worker-researcher` — Does the plan miss trending patterns, known
  pitfalls, or SOTA approaches in the domain?

**Round 2 (selective):** Re-run only perspectives that had actionable
findings. Stop when no actionable findings remain or after 2 rounds.

**Codex plan review — mandatory final gate:**

After the Claude panel converges, invoke `codex-adversary` with scope
`plan-artifact` on the plan file. One-shot, no loop. Triage per
`overlays.md`:

- **Actionable** → orchestrator edits the plan, re-runs one
  `worker-reviewer` (spec-compliance) pass to validate
- **Deferred** → added to handoff Deferred Findings
- **Stated-convention** → dropped, count mentioned
- **Trivia** → dropped, count mentioned

Unavailable path: if `CLAUDE_PLUGIN_ROOT` unset or companion returns
non-zero, log `Cross-model plan review skipped: <reason>` and continue.
At `max` tier this is a gate, not a blocker — but surface the skip
prominently in the handoff so the user knows one layer of review was
missed.

**Gate**: Plan ready for `/swarm-execute`. Deferred findings (both
Claude and Codex) documented in handoff.
