# Tier: high

Default tier for Medium-scope features (One-Way Door Medium: new
subcommand, new index format, new storage layout, 1–2 subsystems). This
is the tier that matches today's `/swarm-plan` behavior — the baseline
all existing callers get when they pass no explicit tier. Preserves
contract-first TDD (Stub → Specify → Implement → Review).

Load this file via `Read` from `SKILL.md` after the config is
announced.

## Phase 1: Discover (parallel)

Launch in parallel:
- **1** `worker-architecture-explorer` (sonnet) — maps current
  architecture, traces dependencies, finds reusable code and patterns
- **2–4** `worker-explorer` agents (haiku) — each scoped to a relevant
  subsystem; first identify scope from `.claude/rules/subsystem-*.md`
  table in `SKILL.md`

**In parallel, read directly:**
- `.claude/rules/product-context.md` — product positioning and vision
- `.claude/artifacts/` — related ADRs, plans, and prior research
- Relevant `subsystem-*.md` rules for the feature area

GitHub discovery: when the target resolves to a PR/issue, use the
fetched context in place of a generic `list_issues` scan. PR file list
becomes explicit scope input to `worker-architecture-explorer`.
Fallback to `mcp__github__list_issues` /
`mcp__github__list_pull_requests` when the target is free text.

**Gate**: Worker reports returned. Current architecture mapped,
reusable components identified, prior artifacts checked for overlap.

## Phase 2: Research (parallel, 1 axis)

Launch **1** `worker-researcher` (model per resolved `--researcher` overlay: `sonnet` default; `haiku` when narrow-scope trigger fires — see `overlays.md` researcher axis) on the single most relevant
axis — pick from tech / patterns / domain. Pair with at least one
explorer's output so external findings are grounded in local code.

Findings >1 paragraph MUST be persisted as
`.claude/artifacts/research_[topic].md` for reuse.

Override: `--research=3` launches all three axes in parallel (tech /
patterns / domain) when the classifier or user requests it.

**Gate**: Research findings persisted (or explicitly marked as "no new
signals"). Adoption trends / recent publications checked.

## Phase 3: Classify (sequential)

Determine reversibility and scope. Record in the plan header:

| Scope | Reversibility | Artifacts Required |
|-------|---------------|--------------------|
| Small (1-3 days) | Two-Way Door | `plan_[feature].md` |
| Medium (1-2 weeks) | One-Way Door (Medium) | `prd_[feature].md` + `plan_[feature].md` |
| Large (2+ weeks) | One-Way Door (High) | `pr_faq_[feature].md` + `prd_[feature].md` + `adr_[decision].md` + `plan_[feature].md` |

Templates at `.claude/templates/artifacts/`. If classification resolves
to Large, **stop and re-run** with `/swarm-plan max "…"` — do not
silently upgrade mid-pipeline.

**Gate**: Scope and reversibility documented in the plan header.

## Phase 4: Design (delegated for One-Way Door, inline otherwise)

For **One-Way Door Medium** or cross-subsystem features, launch
`worker-architect` (sonnet by default, opus if `--architect=opus`
overlay fires) to produce an ADR or system design. For Two-Way Door
features design inline in the plan artifact.

**Design must include:**
- **Component contracts**: public API (types, traits, function
  signatures) and expected behavior per component
- **User experience scenarios**: action → expected outcome → error
  cases for each user-facing behavior
- **Error taxonomy**: all documented failure modes with remediation
  guidance
- **Edge cases**: boundary conditions and corner cases enumerated
- **Trade-off analysis**: min 2 options, weighted criteria, risks,
  reversibility, recommendation with rationale

**Gate**: Design artifacts exist in `.claude/artifacts/`, contracts are
testable (a tester could write failing tests from them without reading
any code).

## Phase 5: Decompose (sequential)

Break the design into right-sized tasks that support contract-first TDD
execution:

- Each task maps to a Stub → Specify → Implement → Review cycle
- Dependencies between tasks form a graph
- Critical path identified
- Parallelizable tasks flagged for `/swarm-execute`

**Gate**: `plan_[feature].md` contains executable phases (Stub → Verify
→ Specify → Implement → Review) that `/swarm-execute` can run without
further decomposition.

## Phase 6: Review (parallel Claude panel, bounded loop)

Launch parallel adversarial reviewers on the draft plan. The loop is
**scope-gated** (plan artifact only) and **severity-gated** (only
actionable findings drive iterations).

**Round 1 — full review (parallel):**
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) —
  Are contracts testable? Do they match the user experience section?
- `worker-architect` — Are trade-offs honest? Alternatives considered?
  Any subsystem boundary violations? *— required for One-Way Door
  decisions*
- `worker-researcher` — Does the plan miss trending patterns, known
  pitfalls, or SOTA approaches in the domain?

Each reviewer classifies findings as:
- **Actionable** — Plan author fixes and re-runs affected reviewers in
  Round 2
- **Deferred** — Requires human decision; surfaced in the handoff
  summary

**Round 2 (selective):** Re-run only perspectives that had actionable
findings. Stop when no actionable findings remain or after 2 rounds
total (reviews of plans should converge fast).

**Codex plan review (optional at this tier):** if the `--codex`
overlay fires (user flag or classifier-inferred for One-Way Door
signals), run a single `codex-adversary` pass in `plan-artifact` scope
mode against the plan file *after* the Claude panel converges.
One-shot, no loop. Triage findings per `overlays.md`:

- Actionable → orchestrator edits the plan, re-runs one
  `worker-reviewer` (spec-compliance) pass to validate
- Deferred → added to handoff Deferred Findings
- Stated-convention / trivia → dropped, counts reported

Unavailable path: log `Cross-model plan review skipped: <reason>` and
continue. Gate, not blocker.

**Gate**: Plan ready for `/swarm-execute`. Deferred findings documented
in handoff.
