# Tier: high

Default tier for Medium-scope features (One-Way Door Medium: new subcommand, new index format, new storage layout, 1–2 subsystems). Tier match today `/swarm-plan` behavior — baseline all existing callers get when pass no explicit tier. Preserve contract-first TDD (Stub → Specify → Implement → Review).

Load file via `Read` from `SKILL.md` after config announced.

## Phase 1: Discover (parallel)

Launch **in single message with multiple Agent tool calls** so run concurrent:
- **1** `worker-architecture-explorer` (sonnet) — map current architecture, trace dependencies, find reusable code + patterns
- **2–4** `worker-explorer` agents (haiku) — each scope to relevant subsystem; first identify scope from `.claude/rules/subsystem-*.md` table in `SKILL.md`

**In parallel, read direct:**
- `.claude/rules/product-context.md` — product positioning + vision
- `.claude/artifacts/` — related ADRs, plans, prior research
- Relevant `subsystem-*.md` rules for feature area

GitHub discovery: when target resolve to PR/issue, use fetched context in place of generic `list_issues` scan. PR file list become explicit scope input to `worker-architecture-explorer`. Fallback to `mcp__github__list_issues` / `mcp__github__list_pull_requests` when target free text.

**Gate**: Worker reports returned. Current architecture mapped, reusable components identified, prior artifacts checked for overlap.

## Phase 2: Research (parallel, 1 axis)

Launch **1** `worker-researcher` (model per resolved `--researcher` overlay: `sonnet` default; `haiku` when narrow-scope trigger fires — see `overlays.md` researcher axis) on single most relevant axis — pick from tech / patterns / domain. Pair with at least one explorer output so external findings grounded in local code.

Findings >1 paragraph MUST persist as `.claude/artifacts/research_[topic].md` for reuse.

Override: `--research=3` launch all three axes **in single message with multiple Agent tool calls** so run concurrent (tech / patterns / domain) when classifier or user request.

**Gate**: Research findings persisted (or explicit mark "no new signals"). Adoption trends / recent publications checked.

## Phase 3: Classify (sequential)

Determine reversibility + scope. Record in plan header:

| Scope | Reversibility | Artifacts Required |
|-------|---------------|--------------------|
| Small (1-3 days) | Two-Way Door | `plan_[feature].md` |
| Medium (1-2 weeks) | One-Way Door (Medium) | `prd_[feature].md` + `plan_[feature].md` |
| Large (2+ weeks) | One-Way Door (High) | `pr_faq_[feature].md` + `prd_[feature].md` + `adr_[decision].md` + `plan_[feature].md` |

Templates at `.claude/templates/artifacts/`. If classify resolve to Large, **stop and re-run** with `/swarm-plan max "…"` — no silent upgrade mid-pipeline.

**Gate**: Scope + reversibility documented in plan header.

## Phase 4: Design (delegated for One-Way Door, inline otherwise)

For **One-Way Door Medium** or cross-subsystem features, launch `worker-architect` (sonnet default, opus if `--architect=opus` overlay fires) to produce ADR or system design. For Two-Way Door features design inline in plan artifact.

**Design must include:**
- **Component contracts**: public API (types, traits, function signatures) + expected behavior per component
- **User experience scenarios**: action → expected outcome → error cases for each user-facing behavior
- **Error taxonomy**: all documented failure modes with remediation guidance
- **Edge cases**: boundary conditions + corner cases enumerated
- **Trade-off analysis**: min 2 options, weighted criteria, risks, reversibility, recommendation with rationale

**Gate**: Design artifacts exist in `.claude/artifacts/`, contracts testable (tester could write failing tests from them without reading any code).

## Phase 5: Decompose (sequential)

Break design into right-sized tasks that support contract-first TDD execution:

- Each task map to Stub → Specify → Implement → Review cycle
- Dependencies between tasks form graph
- Critical path identified
- Parallelizable tasks flagged for `/swarm-execute`

**Gate**: `plan_[feature].md` contain executable phases (Stub → Verify → Specify → Implement → Review) that `/swarm-execute` can run without further decomposition.

## Phase 6: Review (parallel Claude panel, bounded loop)

Launch parallel adversarial reviewers on draft plan. Loop **scope-gated** (plan artifact only) + **severity-gated** (only actionable findings drive iterations).

**Round 1 — full review (parallel):**
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) — Contracts testable? Match user experience section?
- `worker-architect` — Trade-offs honest? Alternatives considered? Any subsystem boundary violations? *— required for One-Way Door decisions*
- `worker-researcher` — Plan miss trending patterns, known pitfalls, SOTA approaches in domain?

Each reviewer classify findings as:
- **Actionable** — Plan author fix + re-run affected reviewers in Round 2
- **Deferred** — Need human decision; surface in handoff summary

**Round 2 (selective):** Re-run only perspectives with actionable findings. Stop when no actionable findings remain or after 2 rounds total (plan reviews converge fast).

**Codex plan review (optional at this tier):** if `--codex` overlay fires (user flag or classifier-inferred for One-Way Door signals), run single `codex-adversary` pass in `plan-artifact` scope mode against plan file *after* Claude panel converges. One-shot, no loop. Triage findings per `overlays.md`:

- Actionable → orchestrator edit plan, re-run one `worker-reviewer` (spec-compliance) pass to validate
- Deferred → add to handoff Deferred Findings
- Stated-convention / trivia → drop, counts reported

Unavailable path: log `Cross-model plan review skipped: <reason>` + continue. Gate, not blocker.

**Gate**: Plan ready for `/swarm-execute`. Deferred findings documented in handoff.