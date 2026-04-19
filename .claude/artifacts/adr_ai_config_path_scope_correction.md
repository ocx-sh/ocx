# ADR: Correct path-scope frontmatter on `workflow-bugfix.md` and `workflow-refactor.md`

## Metadata

**Status:** Proposed
**Date:** 2026-04-19
**Deciders:** AI config overhaul (automated planning session)
**Related Plan:** `plan_ai_config_overhaul.md` (Phase 1)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path: Anthropic `paths:` frontmatter convention for path-scoped rules
**Domain Tags:** devops, infrastructure (AI config)
**Supersedes:** N/A
**Superseded By:** N/A

## Context

Per `audit_ai_config_ocx.md` §Staleness Candidates and §Always-Loaded Baseline, `workflow-bugfix.md` (120 ln) and `workflow-refactor.md` (113 ln) each open with the sentence "Catalog-only rule. Referenced from [workflow-intent.md](./workflow-intent.md) when work is classified as a bug fix" (or "refactoring"). Despite the self-label, neither file has a `paths:` frontmatter entry. Per Anthropic memory docs (per `research_ai_config_sota.md` §Memory), rules without `paths:` load unconditionally every session. These two files together contribute 233 lines / ~17 KB to the always-loaded baseline that the catalog (`rules.md`) and `meta-ai-config.md` claim is only 5 global rules / ~454 lines.

The audit observed:

> "mislabeled 'catalog-only' — no `paths:` present" (audit §Staleness Candidates, severity High, for both files)

And:

> "Stated '5 currently — monitor growth'; actual effective count is 7" (audit §Staleness Candidates, for `meta-ai-config.md`)

Per `research_ai_config_synthesis.md` Headline Finding 2, this is the #1 byte-reduction opportunity: "OCX has 233 lines of undocumented always-loaded context ... [the] catalog-only-labeled files have no `paths:` frontmatter and load every session."

This is a one-way door at the medium level because agents currently relying on bugfix/refactor guidance always being in context will experience silent behavior change: the guidance will only load when editing planning-surface files. If a bug-fix conversation begins without any file open, the agent may not receive the workflow discipline.

## Decision Drivers

- Reduce always-loaded baseline per target metric M1 (824 → ≤600 lines)
- Align with the self-label ("catalog-only")
- Align with Anthropic `paths:` convention (SOTA §Memory)
- Preserve availability when agents are actually reasoning about bugs or refactors

## Industry Context & Research

**Research artifact:** `research_ai_config_synthesis.md` §Headline Finding 2; `research_ai_config_sota.md` §Memory.

**Trending approach:** Every leading config converges on path-scoped rules (Anthropic `paths:`, Cursor `globs:`, Continue.dev `globs:`). Per SOTA §Convergent Patterns, "Path-scoped rules" is universal. Always-loaded rules should be reserved for content every session needs.

**Key insight:** The `workflow-intent.md` router is the actual global entry point for work classification. It decides "this is a bug fix" and then points at `workflow-bugfix.md`. When the pointer is followed, the target file must be available — which means its scope has to cover the planning surface, not just `crates/**`.

## Considered Options

### Option 1: Add `paths: [".claude/**"]` (broad catalog scope)

**Description:** Scope both files to the entire `.claude/` tree. Fires whenever the agent edits any AI config file, plan, or artifact.

| Pros | Cons |
|---|---|
| Simple, matches "catalog-only" semantics | Over-triggers during routine AI config edits |
| Covers all planning-surface scenarios | Adds 233 lines whenever a rule, skill, or agent is edited |

### Option 2: Add targeted planning-surface scope

**Description:** `paths: [".claude/artifacts/**", ".claude/rules/workflow-*.md"]`. Fires when the agent is producing a planning artifact or editing a workflow rule.

| Pros | Cons |
|---|---|
| Matches actual use case (planning phases) | Does not fire for ad-hoc bug reports where no file is open |
| Minimal over-trigger | Relies on `workflow-intent.md` (global, always loaded) for first-touch routing |

### Option 3: Promote both files to explicit global and update the stated count

**Description:** Keep no `paths:`. Update `meta-ai-config.md` to say "currently 7 globals" and `rules.md` to document both files as global.

| Pros | Cons |
|---|---|
| Zero behavior change | Contradicts SOTA convergent pattern (minimize globals) |
| Simplest | Fails target metric M1 (baseline reduction) |

### Option 4: Delete the files; inline their content into `workflow-intent.md`

**Description:** The router (`workflow-intent.md`) absorbs the phases inline.

| Pros | Cons |
|---|---|
| Simplifies structure | Bloats `workflow-intent.md` (currently 39 ln → ~250 ln global) |
| | Violates `meta-ai-config.md` anti-pattern "Global rule >200 lines" |

## Decision Outcome

**Chosen Option:** Option 2 (revised) — **source-work-surface scope** (not planning-surface scope).

**Revision note (Round 2 adversarial review):** The original recommendation was Option 2 with narrow planning-surface scope `paths: [".claude/artifacts/**", ".claude/rules/workflow-*.md"]`. The architect adversarial review (Block B1, file `reviewer-architect` Round 1) found this is a regression masquerading as a fix: bug-fixing happens on source files (`.rs`, `.py`, `.ts`), not in `.claude/` or `workflow-*.md`. Under the narrow scope, workflow-bugfix.md would almost never fire during actual bug-fixing work. The fallback argument that `quality-{lang}.md` covers test-first discipline was specifically rebutted — `quality-rust.md` contains general quality standards but not the Reproduce→RCA→Regression Test→Fix→Verify phase sequence.

**Revised scope:** `paths: ["crates/**", "test/**", "website/**", "mirrors/**", ".claude/**", "Cargo.toml", "Cargo.lock"]` — covers every source-work surface where bug-fixing or refactoring actually happens. workflow-intent.md (still global) handles first-touch triage in conversations where no file is yet open.

**Rationale:** Option 1 (`.claude/**` only) over-triggers during unrelated AI config edits AND under-triggers during source work. Option 3 (promote to global) fails the baseline-reduction target and contradicts SOTA convergent patterns. Option 4 (inline) creates a bloated global. Revised Option 2 — source-work-surface — fires during the actual work (bug-fixing and refactoring on source code), loads the phase discipline where it matters, and still removes the 233 lines from the always-loaded baseline. The path overlap with `quality-{lang}.md` files is acceptable: both fire together on source edits, which is the intended coupling.

### Quantified Impact

| Metric | Before | After | Notes |
|---|---|---|---|
| Always-loaded baseline (lines) | 824 | 591 | 233-line reduction |
| Always-loaded baseline (bytes) | ~54 KB | ~37 KB | ~17 KB reduction |
| Stated global rule count | 5 (inaccurate) | 5 (accurate) | Drift eliminated |

### Consequences

**Positive:**
- Meets target metric M1 in a single edit
- Aligns stated with actual
- No new test infrastructure required beyond `test_workflow_rules_have_paths`

**Negative:**
- Agents reasoning about bug fixes without opening a file may miss phase discipline. Mitigated by `workflow-intent.md` still being global.

**Risks:**
- Silent regression: an agent expected to reach Phase 5 "Verify" may skip it. Mitigation: `workflow-intent.md` (still global) explicitly names the phases sequence, so the router always sets the correct expectation even if the full rule is not loaded.

## Implementation Plan

1. [ ] Add frontmatter block to `workflow-bugfix.md`:
   ```yaml
   ---
   paths:
     - "crates/**"
     - "test/**"
     - "website/**"
     - "mirrors/**"
     - ".claude/**"
     - "Cargo.toml"
     - "Cargo.lock"
   ---
   ```
2. [ ] Same for `workflow-refactor.md`.
3. [ ] Add structural test `test_workflow_rules_have_paths` in `.claude/tests/test_ai_config.py`.
4. [ ] Update `rules.md` "By auto-load path" table to list the two rules under their new scope.
5. [ ] Update `meta-ai-config.md` consistency checklist global count reference.
6. [ ] Verify `task claude:tests` passes.

## Validation

- [ ] `wc -l` of always-loaded files shows ≤600 lines
- [ ] `task claude:tests` passes including the new structural test
- [ ] `workflow-intent.md` continues to route bug / refactor / feature classifications correctly

## Links

- `plan_ai_config_overhaul.md` Phase 1
- `audit_ai_config_ocx.md` §Staleness Candidates
- `research_ai_config_synthesis.md` Headline Finding 2

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-04-19 | AI config overhaul planning | Initial draft |
| 2026-04-19 | Round 2 adversarial review | Scope widened from planning-surface to source-work-surface per architect Block B1 finding (narrow scope never fired during actual bug/refactor work) |
