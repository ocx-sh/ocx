# Tier: max

Full kitchen sink for Large-scope features (One-Way Door High: new
crate, breaking API, cross-subsystem refactor, protocol change,
content-addressed storage layout change). Preserve contract-first TDD.
Add mandatory Opus architect, mandatory 3-axis research, mandatory
Codex plan review as final gate before handoff.

Load via `Read` from `SKILL.md` after config announced.

**Auto meta-plan preview**: tier resolve `max`, `SKILL.md`
step 5 auto-fire meta-plan gate (opt out `--no-dry-run`). Preview =
Cost Transparency — max-tier expensive, catch misclassification before
tokens burn.

## Phase 1: Discover (parallel, full)

Same as tier-high:
- **1** `worker-architecture-explorer` (sonnet) — map current
  architecture, trace deps, find reusable code/patterns
- **2–4** `worker-explorer` agents (haiku) — each scoped to relevant
  subsystem

**Parallel, read direct:**
- `.claude/rules/product-context.md` — product positioning, vision
- `.claude/artifacts/` — related ADRs, plans, prior research
- Relevant `subsystem-*.md` rules for every subsystem touched

GitHub discovery: same as tier-high. PR file list feeds Discover scope
direct.

**Gate**: Full architecture map produced; all prior ADRs / plans / PRDs
in domain enumerated.

## Phase 2: Research (parallel, 3 axes — mandatory)

Launch **3** `worker-researcher` agents **in a single message with multiple Agent tool calls** so concurrent, one per axis:
- **Technology / tools** — trending libraries, competing tools
- **Design patterns** — emerging approaches, industry best practices,
  known pitfalls
- **Domain knowledge** — OCI spec evolution, security, algorithm
  choices

Each researcher produce opinionated recommendation with trend
analysis, adoption signals, citations. Persist as
`.claude/artifacts/research_[topic].md` — mandatory this tier,
substantial findings normal.

**Gate**: Three research artifacts persisted. SOTA + adoption signals
checked all axes.

## Phase 3: Classify (sequential)

Confirm **One-Way Door High** in plan header. If classification
fail (feature actually Medium), downgrade tier
and re-run: `/swarm-plan high "…"`. No silent underspec Large
feature, no silent overspec Medium one.

Required artifacts this tier:
- `pr_faq_[feature].md` (customer narrative)
- `prd_[feature].md` (product requirements)
- `adr_[decision].md` (architecture decision record — mandatory)
- `plan_[feature].md` (executable phases)

Templates at `.claude/templates/artifacts/`.

**Gate**: Scope, reversibility, all four artifacts listed in plan
header.

## Phase 4: Design (worker-architect opus, mandatory ADR)

Launch **1** `worker-architect` with **model=opus** (mandatory this
tier — override any `--architect=` flag to weaker model). Produce
both ADR and (when scope warrants) system-design doc.

**Design must include:**
- **Component contracts**: public API (types, traits, function
  signatures) + expected behavior per component
- **User experience scenarios**: action → expected outcome → error
  cases per user-facing behavior
- **Error taxonomy**: all documented failure modes with remediation
- **Edge cases**: boundary conditions, corner cases enumerated
- **Trade-off analysis**: min 3 options this tier (not 2), weighted
  criteria, risks, reversibility, recommendation with rationale
- **Migration / rollout plan**: how existing code / data / users move
  to new shape without breakage (or with explicit breakage
  communicated)

**Gate**: ADR committed, design artifacts exist, contracts testable.

## Phase 5: Decompose (sequential)

Break design into right-sized tasks supporting contract-first
TDD cycle: **Stub → Specify → Implement → Review**. Each task map
to cycle so `/swarm-execute` run unchanged. Dependencies
between tasks form graph. Critical path identified. Parallelizable
tasks flagged for `/swarm-execute`.

**Gate**: `plan_[feature].md` contain executable phases (Stub → Verify
→ Specify → Implement → Review) that `/swarm-execute` run without
further decomposition.

## Phase 6: Review (parallel Claude panel + mandatory Codex)

**Round 1 — full Claude panel (launched in single message with multiple Agent tool calls, concurrent):**
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) —
  Contracts testable? Match user experience section?
- `worker-architect` — Trade-offs honest? Alternatives considered?
  Subsystem boundary violations?
- `worker-researcher` — Plan miss trending patterns, known
  pitfalls, SOTA approaches in domain?

**Round 2 (selective):** Re-run only perspectives with actionable
findings. Stop when no actionable findings remain or after 2 rounds.

**Codex plan review — mandatory final gate:**

After Claude panel converges, invoke `codex-adversary` scope
`plan-artifact` on plan file. One-shot, no loop. Triage per
`overlays.md`:

- **Actionable** → orchestrator edit plan, re-run one
  `worker-reviewer` (spec-compliance) pass to validate
- **Deferred** → add to handoff Deferred Findings
- **Stated-convention** → drop, count mentioned
- **Trivia** → drop, count mentioned

Unavailable path: if `CLAUDE_PLUGIN_ROOT` unset or companion return
non-zero, log `Cross-model plan review skipped: <reason>` and continue.
At `max` tier this gate, not blocker — but surface skip
prominently in handoff so user know one layer of review missed.

**Gate**: Plan ready for `/swarm-execute`. Deferred findings (both
Claude and Codex) documented in handoff.