# ADR: Deduplicate Review-Fix Loop protocol to a single canonical reference

## Metadata

**Status:** Proposed (revised Round 2 — dedup model changed from single-canonical to three-carrier parity)
**Date:** 2026-04-19
**Deciders:** AI config overhaul (automated planning session)
**Related Plan:** `plan_ai_config_overhaul.md` (Phase 5)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path: single source of truth per Core Principle 5 (DRY) in `CLAUDE.md`
**Domain Tags:** devops (AI config), api (workflow contract)
**Supersedes:** Current implicit practice (verbatim copies in 7+ locations)
**Superseded By:** N/A

## Context

Per `audit_ai_config_ocx.md` §Duplication Scan, the Review-Fix Loop protocol is the top duplication cluster in the entire `.claude/` directory. It appears verbatim or near-verbatim in 7+ locations:

- `workflow-bugfix.md` (Phase 6)
- `workflow-refactor.md` (Phase 5)
- `workflow-feature.md` (step 9)
- `workflow-swarm.md`
- `swarm-execute/SKILL.md`
- `swarm-execute/tier-low.md`, `tier-high.md`, `tier-max.md`
- `swarm-review/SKILL.md`

This is real duplication, not intentional subagent redundancy: per the audit note, items 1, 2, 6, 7 in the duplication table (Review-Fix Loop among them) are duplication, not intended parallel copies for subagent context. When a protocol has 7 canonical copies, drift is inevitable and auditing becomes untractable.

Per `research_ai_config_synthesis.md` §Convergent Patterns and the OCX Core Principle 5 ("Don't Repeat Yourself"), extracting the protocol to a canonical location with pointer-only references from the rest is the standard remediation.

The constraint: worker agents (`.claude/agents/worker-*.md`) need access to the Review-Fix Loop protocol during `/swarm-execute`. If the canonical location is not auto-loaded for workers, pointer-only references turn into broken pointers — the worker follows the link conceptually but never loads the file.

Per `audit_ai_config_ocx.md` §Scoped rules, `workflow-swarm.md` has `paths: [".claude/agents/**", ".claude/skills/swarm-*/**"]`. This means it *already* auto-loads for every worker agent file and every swarm skill tier file. It is the natural canonical home.

This is a one-way door at the medium level: once workers depend on the canonical-via-auto-load pattern, the pattern must stay stable, or worker context breaks silently.

## Revision Note (Round 2 adversarial review)

The original decision — canonical prose in `workflow-swarm.md` only + pointer-only from the other 6–7 locations — was found to have a visibility gap (architect Block B2): `workflow-swarm.md` auto-loads only on `.claude/agents/**` and `.claude/skills/swarm-*/**`. After Phase 1's path-scope correction (source-work-surface for `workflow-bugfix.md` + `workflow-refactor.md`), a user running a bug fix on `.rs` would load `workflow-bugfix.md` but NOT `workflow-swarm.md` (wrong scope), so the canonical Review-Fix Loop text would be absent from context during the most common real-world use case.

**Revised dedup model — three canonical carriers + pointer-only elsewhere:**

Canonical carriers (each contains byte-identical Review-Fix Loop prose, delimited by HTML comment markers so a structural parity test can enforce byte equivalence):
1. `workflow-swarm.md` — auto-loads for worker agents and swarm skills
2. `workflow-bugfix.md` — auto-loads for source-work surfaces (post-Phase-1)
3. `workflow-refactor.md` — auto-loads for source-work surfaces (post-Phase-1)

Pointer-only files (reference one of the three carriers, no verbatim prose):
- `workflow-feature.md`
- `swarm-execute/SKILL.md`
- `swarm-execute/tier-low.md`
- `swarm-execute/tier-high.md`
- `swarm-execute/tier-max.md`
(`swarm-review/SKILL.md` already uses pointer-style, no change.)

Trade-off: three copies instead of one, which seems to contradict DRY. But DRY's purpose is preventing drift — a byte-identity structural test catches drift at commit time, achieving DRY's intent (single source-of-truth content) without DRY's literal form (single source-of-truth file). The three-carrier model is the minimum set guaranteeing the protocol is loaded during every practical work scenario: swarm orchestration (workflow-swarm.md), ad-hoc bug fixing (workflow-bugfix.md), and ad-hoc refactoring (workflow-refactor.md).

## Decision Drivers

- Eliminate the top duplication cluster (7+ verbatim copies)
- Preserve worker-agent access to the protocol during `/swarm-execute`
- Prevent future drift via structural test
- Align with DRY / single-source-of-truth principle

## Industry Context & Research

**Research artifact:** `audit_ai_config_ocx.md` §Duplication Scan (items 1, 2); `research_ai_config_synthesis.md` §2 Convergent Patterns.

**Trending approach:** Anthropic recommends `@path` imports for forced inclusion and skill-name references for on-demand composition. Cursor/Continue.dev both use similar patterns. The OCX convention is to keep prose canonical in a rule and let path-scoping deliver it to the right contexts.

**Key insight:** `workflow-swarm.md` is already path-scoped to every file that needs the Review-Fix Loop (agents + swarm skills). This is a fortunate accident that becomes the decision's foundation.

## Considered Options

### Option 1: Canonical in `workflow-swarm.md`; pointer-only everywhere else

**Description:** Keep the full protocol prose in `workflow-swarm.md`. In all 6 other locations, replace the verbatim prose with a pointer block: `> Protocol: see [Review-Fix Loop in workflow-swarm.md](../../rules/workflow-swarm.md#review-fix-loop). Loads automatically for agents and swarm skills.`

| Pros | Cons |
|---|---|
| `workflow-swarm.md` already auto-loads for workers and swarm skills | Non-swarm workflows (`workflow-bugfix.md`, `workflow-refactor.md`, `workflow-feature.md`) get a pointer but not auto-loaded prose |
| Path-scoping already solved | Bug-fix / refactor flows rely on the human to follow the pointer |

### Option 2: Canonical in a new `.claude/rules/workflow-review-loop.md`

**Description:** Create a dedicated file for the protocol; other files reference it.

| Pros | Cons |
|---|---|
| Single-purpose file | Adds a new file for a single protocol; fragments workflow rules |
| Can have its own `paths:` scope | Would need overlapping `paths:` with `workflow-swarm.md`, failing `test_path_overlaps_declared_or_absent` unless declared |

### Option 3: Accept duplication; just add a drift-detection test

**Description:** Keep the 7 copies; add a structural test asserting they are byte-identical.

| Pros | Cons |
|---|---|
| Zero edit cost | Test becomes a change multiplier: any protocol tweak requires 7 edits |
| | Fails the DRY principle |

### Option 4: Inline the protocol as an `@-import` in each caller

**Description:** Extract to a file; each caller uses `@.claude/rules/workflow-review-loop.md` to force-load.

| Pros | Cons |
|---|---|
| Forced inclusion in every caller's context | `@-import` depth limit is 5 hops; increases cache churn |
| | Overrides path-scoping semantics — every edit to the protocol invalidates all caller files' cached content |

## Decision Outcome

**Chosen Option (revised Round 2):** **Option 5 — three-carrier byte-identity parity** (new option introduced in Round 2 after adversarial review).

**Rationale:** The original Option 1 (single canonical in `workflow-swarm.md` + pointer-only elsewhere) was rejected in Round 2 adversarial review (architect Block B2). After Phase 1's path-scope correction, `workflow-swarm.md` auto-loads only on `.claude/agents/**` and `.claude/skills/swarm-*/**`. A user running a bug fix directly on `.rs` code would load `workflow-bugfix.md` but NOT `workflow-swarm.md`, so the canonical Review-Fix Loop text would be absent from context at the exact moment it is needed.

**Option 5 — three-carrier byte-identity parity:** keep full Review-Fix Loop prose in three files, delimited by HTML comment markers `<!-- REVIEW_FIX_LOOP_CANONICAL_BEGIN -->` / `<!-- REVIEW_FIX_LOOP_CANONICAL_END -->`. A structural test enforces byte-identity across the three copies. DRY's intent (single source of truth content) is preserved via the test; DRY's literal form (single file) is sacrificed for correct context loading.

**Canonical carriers (each auto-loads on a different surface):**
1. `workflow-swarm.md` — auto-loads for `.claude/agents/**` and `.claude/skills/swarm-*/**` (worker orchestration)
2. `workflow-bugfix.md` — auto-loads for source-work surfaces per Phase 1 path-scope correction
3. `workflow-refactor.md` — auto-loads for source-work surfaces per Phase 1 path-scope correction

**Pointer-only files (no verbatim prose; short reference block):**
- `workflow-feature.md`
- `swarm-execute/SKILL.md`
- `swarm-execute/tier-low.md`
- `swarm-execute/tier-high.md`
- `swarm-execute/tier-max.md`
- (`swarm-review/SKILL.md` already uses pointer-style, no edit)

### Canonical Prose Location

`workflow-swarm.md` §Review-Fix Loop (new section anchor added if not already present). Contains:

1. Loop semantics (Round 1 runs all perspectives; subsequent rounds re-run only affected ones)
2. Termination rules (no actionable findings, or max rounds reached)
3. Classification (actionable vs deferred)
4. Cross-model adversarial pass
5. Tier scaling (1 round at low, 3 at high/max)

### Pointer Block Template

Each of the 6 other files replaces its Review-Fix Loop prose with:

```
## Review-Fix Loop

Protocol: see the full Review-Fix Loop specification in `workflow-swarm.md`. The protocol auto-loads for agent and swarm-skill contexts; for other contexts, read the section directly before invoking the loop.
```

### Consequences

**Positive:**
- Single source of truth for the protocol
- Future protocol tweaks need one edit, not seven
- Structural test prevents drift

**Negative:**
- Three non-swarm files rely on prose pointers rather than auto-loaded content
- Readers of `workflow-bugfix.md` must follow the pointer to see full protocol

**Risks:**
- **Agents trained on the old verbatim copies may drift when protocol evolves in one place.** Mitigation: structural test asserts only one file contains the full protocol.
- **Pointer goes stale (section anchor renamed).** Mitigation: structural test verifies the pointed-at anchor exists in `workflow-swarm.md`.
- **Worker context misses the protocol in an edge case.** Mitigation: verify post-edit by spawning a `worker-reviewer` and inspecting its loaded context; the `workflow-swarm.md` body must be present.

## Structural Test

New test `test_review_fix_loop_parity` in `.claude/tests/test_ai_config.py`:

1. Extract the byte range between `<!-- REVIEW_FIX_LOOP_CANONICAL_BEGIN -->` and `<!-- REVIEW_FIX_LOOP_CANONICAL_END -->` markers from each of the three canonical carriers (`workflow-swarm.md`, `workflow-bugfix.md`, `workflow-refactor.md`).
2. Assert all three byte ranges are exactly equal (byte-identity parity).
3. Assert the pointer-only files (`workflow-feature.md`, `swarm-execute/SKILL.md`, `swarm-execute/tier-low.md`, `swarm-execute/tier-high.md`, `swarm-execute/tier-max.md`) contain a link to one of the three carriers' `#review-fix-loop` anchor.
4. Assert no file outside the three canonical carriers contains a `<!-- REVIEW_FIX_LOOP_CANONICAL_BEGIN -->` marker (prevents accidental fourth carrier).

## Implementation Plan

1. [ ] Ensure `workflow-swarm.md` has a canonical `## Review-Fix Loop` section with the full protocol, delimited by `<!-- REVIEW_FIX_LOOP_CANONICAL_BEGIN -->` / `<!-- REVIEW_FIX_LOOP_CANONICAL_END -->` markers
2. [ ] Sync byte-identical prose into `workflow-bugfix.md` Phase 6 between the same markers
3. [ ] Sync byte-identical prose into `workflow-refactor.md` Phase 5 between the same markers
4. [ ] Replace verbatim prose in `workflow-feature.md` step 9 with the pointer block
5. [ ] Replace verbatim prose in `swarm-execute/SKILL.md` with the pointer block
6. [ ] Replace verbatim prose in `swarm-execute/tier-low.md`, `tier-high.md`, `tier-max.md` with the pointer block
7. [ ] Confirm `swarm-review/SKILL.md` already uses a pointer (no change needed) or adjust
8. [ ] Add structural test `test_review_fix_loop_parity`
9. [ ] Spawn-test a `worker-reviewer` with a trivial task; confirm at least one Review-Fix Loop carrier is in its loaded context

## Validation

- [ ] `test_review_fix_loop_parity` passes (all three carriers byte-identical between markers)
- [ ] Worker spawn-test confirms protocol is in agent's context
- [ ] `task claude:tests` passes
- [ ] `lychee --offline .claude/` confirms no broken internal references

## Links

- `plan_ai_config_overhaul.md` Phase 5
- `audit_ai_config_ocx.md` §Duplication Scan
- `research_ai_config_synthesis.md` §2 Convergent Patterns

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-04-19 | AI config overhaul planning | Initial draft |
