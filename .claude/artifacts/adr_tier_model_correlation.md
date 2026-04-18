# ADR: Tier ↔ Worker Model Correlation — Cost-Aware Routing Across the Swarm

## Metadata

**Status:** Proposed
**Date:** 2026-04-18
**Deciders:** mherwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Paths in `.claude/rules/product-tech-strategy.md` (no new external tech introduced; tooling-layer change only)
**Domain Tags:** ai-config, swarm, cost-optimization, meta
**Supersedes:** N/A
**Superseded By:** N/A

## Context

OCX runs three swarm skills (`/swarm-plan`, `/swarm-execute`, `/swarm-review`) over nine worker types. Worker model selection today is a mixed regime:

- Seven of nine workers have a **fixed** model set in `.claude/agents/worker-*.md` frontmatter and never vary by tier or task signal.
- Only two workers scale with tier: `worker-architect` (via the `--architect=inline|sonnet|opus` overlay in `/swarm-plan`) and `worker-builder` (via `--builder=sonnet|opus` in `/swarm-execute`, implemented through orchestrator prompt injection of `model: opus`).
- The five workers that run under `/swarm-review` (reviewer, tester, researcher, doc-reviewer, plus optional architect/architecture-explorer) all run at their fixed models regardless of whether the diff is a one-line flag change (tier=low) or a cross-subsystem protocol change (tier=max).

Two quantitative observations from the research phase make this asymmetry hard to defend as-is:

1. **`worker-tester` is explicitly asked to do materially different work at tier=max than at tier=low** — "exhaustive edge-case coverage including protocol-level corner cases and cross-subsystem interactions" (tier=max) vs. unit tests on a single file (tier=low) — yet runs at Sonnet 4.6 in both cases. This is quality-asymmetric, not cost-asymmetric: max-tier testers may be under-modeled.
2. **`workflow-swarm.md` line 35 cites a stale rationale**: "Sonnet 4.6 within 1.2pp of Opus on SWE-bench at 5× lower cost." Per the research (`research_model_capability_matrix.md`), the current Opus 4.7 vs Sonnet 4.6 gap is 8.0pp on SWE-bench Verified (not 1.2pp — that was the Opus 4.6 number), and the 5× ratio describes Opus:Haiku, not Opus:Sonnet (Opus:Sonnet is 1.67×). The justification shipped in config no longer matches the model landscape.

The user's intent per the meta-plan is to (a) scale cheaper models *down* on small changes where they suffice and (b) scale *up* where complexity demands it, all without restructuring the tier grammar itself. This ADR proposes that strategy at the overlay-axis level — the same mechanism already used for architect and builder.

**Scope constraint (set at the meta-plan gate):** tier-coupled overlays only. No changes to agent frontmatter defaults. No new Claude Code primitives. No runtime LLM judge (future work).

## Decision Drivers

- **Cost discipline.** Research documents a 40–60% cost reduction from cascade routing (`research_multi_agent_model_routing.md`). Even a partial share of that is material when applied across every swarm run.
- **Quality floor.** No surveyed multi-agent framework downgrades review/judgment workers to Haiku. Frameworks use Haiku only for read-only exploration or narrow factual lookups. Any downgrade must preserve judgment-floor invariants.
- **Hard constraints from model capability.** Haiku 4.5's 200k context window is a blocker for codebase-wide workers. Workers that make many structured tool calls (builder, tester) stay Sonnet minimum per the CrewAI `function_calling_llm` pattern's rationale.
- **Consistency with existing grammar.** Two overlay axes already exist (`--architect`, `--builder`). New axes must follow the same `--axis=value1|value2` grammar, the same per-tier defaults table, the same "user flag wins except tier=max mandatory floor" precedence. Orthogonality over cleverness.
- **Tier=max is the safety floor.** Every proposed downgrade must leave tier=max equal to or stronger than the status quo. Optimization happens at tier=low and tier=high, never at tier=max.
- **Security paths override downgrades.** `oci/**`, `package_manager/**`, `auth`, `crypto`, `signing` paths must never produce a Haiku reviewer regardless of diff size. The existing breadth-escalation path (`review/classify.md` structural markers) provides the hook.

## Considered Options

### Option A: Status Quo — keep only `--architect` and `--builder` tier-coupled

Leave the seven tier-invariant workers alone. Update only the stale rationale string in `workflow-swarm.md`.

**Pros:**
- Zero new overlay axes, zero new classifier rules, minimal maintenance
- Lowest risk of introducing model thrash or silent quality regression
- Existing overlay grammar remains lean

**Cons:**
- Does not address user's stated intent (cheaper models for smaller changes across the swarm)
- Leaves the tester asymmetry unresolved at tier=max (under-modeled for the assigned work)
- Misses the clearest Haiku-viable wins identified in research: `worker-doc-reviewer` at tier=low, `worker-researcher` on narrow-scope lookups at tier=low, `worker-explorer` is already at the Haiku floor
- Misses the tier=max tester upgrade that the research implies is warranted

### Option B: LLM-judge dynamic routing per OpenHands

Introduce a cheap "judge" model that scores each pending worker task at runtime and routes to reasoning vs. default model based on the score. Per `research_multi_agent_model_routing.md`, OpenHands is the clearest example of this pattern in the survey.

**Pros:**
- Theoretically the most precise routing — reacts to actual task difficulty, not just structural signals
- Framework-agnostic pattern, documented in the literature

**Cons:**
- **Not a Claude Code primitive today.** Would require introducing a judge-model call before every worker invocation — new infrastructure, new failure mode, new cost line item (judge tokens are not free even on Haiku).
- Research identifies this as the "most transferable dynamic mechanism" but also notes it is unimplemented and represents a structural framework change, not a config change.
- Adds a runtime dependency to every swarm step. The existing meta-plan gate pattern (single approval point) is strictly simpler.
- Classify-time static routing already captures ~80% of the signal, per the Cursor 90/10 ratio finding. Marginal gain is small.
- Violates the meta-plan scope constraint ("no new Claude Code feature").

**Verdict:** Rejected for this ADR. Retained as future work.

### Option C: Tier-Coupled Expansion — new overlay axes per worker type (Recommended)

Extend the existing `--builder` / `--architect` overlay pattern to the workers where research supports tier-varying model selection, following the exact same axis grammar and precedence rules. Propagation mechanism stays the current one: orchestrator injects `model: <name>` in the worker prompt, mirroring how builder opus override works today.

**Pros:**
- Zero new Claude Code infrastructure — reuses the only runtime model override path that is known to work (`worker-builder` opus injection, shipping today)
- Each new axis is independent (orthogonal axes — a user can override one without touching others) and independently testable/revertible
- Maps directly onto the research findings: tester up at max, doc-reviewer down at low, researcher down at low-narrow-scope, rest stays Sonnet
- Honors security-path escalation: same structural markers that raise `--breadth` can raise the reviewer/tester model floor
- Announcement protocol already exists — each new axis surfaces in the meta-plan gate output per `workflow-swarm.md`

**Cons:**
- Four new overlay axes (reviewer, tester, researcher, doc-reviewer) expands the config surface area — more to document, more to maintain, more to test
- Users who don't read release notes will not notice the classifier-driven changes; mitigated by the meta-plan gate announcement printing the resolved config per axis
- Propagation still relies on prompt injection — if the Claude Code invocation API tightens its handling of `model:` in prompt strings, every axis breaks at once (same risk as today's builder override, but multiplied)

**Weighted comparison:**

| Criterion | A (Status Quo) | B (LLM Judge) | C (Tier Expansion) |
|---|---|---|---|
| Addresses user intent | No | Yes (most precise) | Yes (pragmatic) |
| Implementation cost | Nil | High (new infra) | Low (reuse grammar) |
| Revert path | N/A | Hard (infra to rip out) | Easy (revert one commit per axis) |
| Quality risk | None (does nothing) | Medium (judge mis-score) | Low (floors preserved) |
| Config surface growth | None | Medium | Medium (4 axes) |
| Matches meta-plan scope | Partially | No (out of scope) | Yes |

**Option C wins on the scope constraint and the cost/benefit profile.**

## Decision

**Adopt Option C: Tier-Coupled Expansion.**

OCX extends the existing overlay axis grammar with four new tier-coupled axes — `--reviewer`, `--tester`, `--researcher`, `--doc-reviewer` — that scale those workers' models per tier and per classifier signal, following the same `--axis=value1|value2` pattern already used for `--architect` and `--builder`. The propagation mechanism is orchestrator prompt injection of `model: <name>`, the identical mechanism currently used for builder opus override. Agent frontmatter defaults stay unchanged; overlays always win over frontmatter at runtime. Tier=max is the safety floor — every proposed axis leaves the max-tier model at or above the current Sonnet assignment. Security-sensitive paths (`oci/**`, `package_manager/**`, auth/crypto) prevent any downgrade for reviewer and tester regardless of tier.

Workers that research identified as already-optimal are not touched: `worker-explorer` stays Haiku (already at floor), `worker-architecture-explorer` stays Sonnet (needs 1M context), `worker-doc-writer` stays Sonnet across all tiers (no surveyed framework uses Haiku for writing tasks, and output quality cost is load-bearing for user-facing docs).

The stale "within 1.2pp" rationale in `workflow-swarm.md` is replaced with an accurate, model-version-dated sentence that references `research_model_capability_matrix.md` for the full breakdown.

## Consequences

### Positive

- **Cost reduction** proportional to the share of tier=low runs across the swarm. Realistic estimate from the research's cascade-routing numbers: 15–25% reduction in per-run average cost at tier=low (the largest volume tier), lower at tier=high, zero at tier=max. Doc-reviewer and researcher are the two biggest contributors because their frontmatter model (Sonnet) drops to Haiku at tier=low for the cases where signal supports it.
- **Quality increase at tier=max for tester** — edge-case / protocol-level test generation moves from Sonnet to Opus, closing the asymmetry called out in discover.
- **Consistent, learnable mental model** — every worker now has the same "tier default + classifier overlay + user flag" mental model. No worker is mysteriously tier-invariant.
- **Accurate documentation** — the rationale string downstream agents read in `workflow-swarm.md` now matches the actual model landscape.
- **Security floors preserved** — structural markers keep reviewer/tester at Sonnet+ on sensitive paths regardless of tier.

### Negative

- **Four new overlay axes** grow the swarm config surface. Each needs an entry in `overlays.md`, a trigger rule in `classify.md`, a row in the per-tier defaults cheat-sheet, and an argument-parser entry in the corresponding `SKILL.md`. Test coverage in `.claude/tests/test_ai_config.py` must extend with axis/flag parity checks.
- **Prompt-injection fragility compounds** — today's `worker-builder` opus override is one prompt-injection point; after this ADR there are five. If Claude Code tightens invocation-time model handling, all five break together. Mitigation: document the injection convention explicitly in `workflow-swarm.md`.
- **Axis announcement bloat** — meta-plan gate output grows longer as resolved per-axis config is printed. Mitigation: compact single-line format with source attribution (`reviewer=haiku (tier default)`, `tester=opus (classifier: cross-subsystem)`, etc.).
- **Downgrade-to-Haiku context risk** — worker prompts at tier=low typically fit in 200k, but research flags context-window mismatch as an anti-pattern. Mitigation: `--doc-reviewer=haiku` is fenced to single-file review tasks; full user-guide audits fall back to Sonnet via classifier rule.

### Neutral

- **Introduces explicit review of the "seven fixed workers" assumption.** Even if a future ADR decides to move back toward fixed-model workers for maintenance reasons, the explicit axis grammar makes that decision visible and reversible.
- **Creates precedent for intra-worker model split** (CrewAI `function_calling_llm` pattern) — future axes could split reasoning vs. tool-calling within a worker. This ADR does not do that, but the grammar supports it.

## Proposed Model Matrix

**Legend:** Cells show `tier default (→ override conditions)`. "—" means unchanged from existing overlay. **Bold** means proposed change from status quo.

| Worker | tier=low default | tier=high default | tier=max default | New axis | Classifier triggers |
|---|---|---|---|---|---|
| architect | inline | inline; sonnet on One-Way Door Medium; opus on One-Way Door High or keyword signals | opus (mandatory) | `--architect` (existing) | Existing: "new trait hierarchy", "novel algorithm", "cross-subsystem", "protocol change", "content-addressed storage layout change" |
| builder | sonnet | sonnet (→ opus via `--builder=opus` on ≥2 subsystems or novel-algorithm/cross-subsystem/protocol keywords) | opus (mandatory) | `--builder` (existing) | Existing: Subsystems Touched ≥2; keyword signals |
| reviewer | **haiku** (narrow-scope only) → **sonnet** on security paths or `package_manager/**` | sonnet | sonnet (→ **opus** when `--breadth=adversarial` AND tier=max) | **`--reviewer=haiku\|sonnet\|opus`** | tier=low AND no structural markers from the `review/classify.md` security list → haiku; any of `oci/**`, `package_manager/**`, auth/crypto/signing → sonnet minimum; tier=max + `--breadth=adversarial` → opus |
| tester | sonnet | sonnet | **opus** (upgrade from Sonnet) | **`--tester=sonnet\|opus`** | tier=max default = opus (driven by tier=max's documented "exhaustive edge-case coverage" work); tier=high stays sonnet; tier=low stays sonnet (per research: "no framework uses Haiku for judgment" and test generation is judgment-adjacent) |
| researcher | **haiku** (single-axis, narrow factual lookup) → sonnet otherwise | sonnet | sonnet | **`--researcher=haiku\|sonnet`** | tier=low AND `--research=1` AND axis is narrow factual lookup (not synthesis) → haiku; `--research=3` or tier≥high → sonnet (Claude Code "Plan=inherit" pattern: synthesis needs full capability); Haiku 200k context caps apply — if worker prompt + loaded docs exceed 150k projected, escalate to sonnet |
| doc-reviewer | **haiku** (single-file / narrow-scope audit) → sonnet for full user-guide audit | sonnet | sonnet | **`--doc-reviewer=haiku\|sonnet`** | tier=low AND diff touches ≤2 doc files AND no `docs/user-guide.md` → haiku; otherwise sonnet. Per research: Qodo study places Haiku 4.5 ahead of Sonnet 4.5 on single-pass PR review, single-file doc audit maps well. |
| doc-writer | sonnet | sonnet | sonnet | None proposed | Intentionally NOT tier-coupled — writing quality is load-bearing for user-visible docs, and Haiku trails Sonnet on long-form synthesis per research |
| architecture-explorer | sonnet | sonnet | sonnet | None proposed | Needs 1M context for whole-repo maps; Haiku's 200k cap is a hard blocker |
| explorer | haiku | haiku | haiku | None proposed | Already at the Haiku floor per existing frontmatter |

### Per-worker justifications (1–2 lines each, citing research)

- **reviewer at tier=low → haiku**: The surveyed frameworks keep Haiku for read-only exploration, but OCX tier=low reviews on ≤3 files / ≤100 lines (`review/classify.md` tier metrics) are closer to "single-pass code comment" than "multi-step agentic judgment." The Qodo 400-PR study places Haiku 4.5 ahead of Sonnet 4.5 in single-pass review mode. Fenced by structural markers so anything security-sensitive lands on Sonnet regardless.
- **reviewer at tier=max → opus on adversarial**: Adversarial breadth adds SOTA-gap / architect / CLI-UX perspectives — the exact "multi-step agentic chains" where the 8.0pp Opus-Sonnet SWE-bench gap materializes. Cost is tolerable because adversarial only fires at max and only when One-Way Door signals are already present.
- **tester at tier=max → opus**: Closes the quality asymmetry documented in the meta-plan. tier=max testers are asked to cover "protocol-level corner cases and cross-subsystem interactions" — this is novel-reasoning work. ARC-AGI-2 gap (10.5pp Opus over Sonnet on distribution-shifted reasoning) is the relevant benchmark.
- **researcher at tier=low with narrow-scope → haiku**: Mirrors Claude Code's own pattern — Explore uses Haiku for narrow codebase search, Plan uses inherit for synthesis. Narrow factual lookup with one axis is not synthesis.
- **doc-reviewer at tier=low → haiku**: Single-file doc audit is the exact workload Haiku 4.5 beats Sonnet 4.5 on per the Qodo study. Full user-guide audits are synthesis and stay Sonnet.
- **doc-writer kept Sonnet across all tiers**: Long-form writing quality matters for positioning documentation; Haiku trails on long-form synthesis and is not recommended by Anthropic for doc-writing tasks.
- **architecture-explorer kept Sonnet**: Context-window mismatch is a hard constraint. Haiku's 200k cap would silently truncate whole-repo maps. Already a documented anti-pattern in the research.
- **explorer kept Haiku**: Already optimal per frontmatter, no change proposed.

## Signal Propagation Mechanism

**Decision: option (2) — new overlay axes per worker type — implemented via option (1), orchestrator prompt injection.**

This is a two-layer answer because the options surveyed in `research_ocx_internal_model_signals.md` are not mutually exclusive. The grammar layer (overlay axes) is the user-facing surface. The runtime layer (prompt injection) is the mechanism. Both are needed and they fit together.

### Grammar layer — new overlay axes

Each new axis follows the existing contract verbatim:

- `--reviewer=haiku|sonnet|opus` — added to `swarm-execute/overlays.md` and `swarm-review/overlays.md` (reviewer runs in both pipelines)
- `--tester=sonnet|opus` — added to `swarm-execute/overlays.md` only (tester runs only in execute)
- `--researcher=haiku|sonnet` — added to `swarm-plan/overlays.md` only (researcher runs only in plan)
- `--doc-reviewer=haiku|sonnet` — added to `swarm-execute/overlays.md` and `swarm-review/overlays.md` (doc-reviewer fires in both when doc triggers match)

Every axis ships with:

1. Entry in the relevant `overlays.md` following the architect/builder entry shape (description, value table, per-tier defaults)
2. Trigger rule in the relevant `classify.md` with signal→value mapping
3. Row in the per-tier defaults cheat-sheet at the bottom of `overlays.md`
4. Argument-parser entry in the corresponding `SKILL.md`
5. Announcement line in the meta-plan gate output (`<axis>=<value> (<source>)`)

### Runtime layer — prompt injection

The orchestrator passes `model: <name>` as a line in the worker invocation prompt. This is the mechanism today for `worker-builder` opus override (documented at `workflow-swarm.md:47`: "orchestrator passes `model: opus`"). The frontmatter `model:` field in each `worker-*.md` remains the default; the prompt injection overrides it per-invocation.

**No changes to agent frontmatter.** Per meta-plan scope.

**No changes to Claude Code agent-invocation API.** This ADR assumes only what is already documented to work (the existing builder opus override).

### Rejected propagation alternatives

- **Shared `model-matrix.md` cross-read by all three classify files (option 4 from research):** Over-engineered for current scope. The three classify files already cross-read each other for the tier signal table (execute/classify.md explicitly reads plan/classify.md's table). A central matrix file would be a nice-to-have once five axes exist; it is not a precondition for correctness. Defer to future work once the axes are in place and drift becomes visible.
- **Per-tier defaults table extension (option 3 from research):** This IS part of the decision — each new axis adds a row to the per-tier defaults table in its `overlays.md`. Not a separate mechanism.

### Files edited per axis (reference)

| Axis | SKILL.md | overlays.md | classify.md |
|---|---|---|---|
| `--reviewer` | swarm-execute, swarm-review | swarm-execute, swarm-review | swarm-execute, swarm-review |
| `--tester` | swarm-execute | swarm-execute | swarm-execute |
| `--researcher` | swarm-plan | swarm-plan | swarm-plan |
| `--doc-reviewer` | swarm-execute, swarm-review | swarm-execute, swarm-review | swarm-execute, swarm-review |

Seven config files per execute+review axis; three per plan-only axis. All edits are pure additions (new axis entries), no deletions of existing content.

## Verification Honesty Update

Replace `workflow-swarm.md:35` (and `workflow-swarm.md:51`):

**Current (stale):**
> `worker-reviewer` | sonnet | Read, Glob, Grep, Bash | Code review/security/spec-compliance (diff-scoped, Sonnet 4.6 within 1.2pp of Opus on SWE-bench at 5× lower cost)

> **Model selection rationale:** Sonnet 4.6 is within 1.2pp of Opus 4.6 on SWE-bench at 5× lower cost. Reserve Opus for deep reasoning (architecture, complex implementation). Use Sonnet as default for review, testing, and mechanical refactoring.

**Proposed replacement:**
> `worker-reviewer` | sonnet (default) | Read, Glob, Grep, Bash | Code review/security/spec-compliance (diff-scoped; model scales per tier via `--reviewer` overlay — see `.claude/artifacts/adr_tier_model_correlation.md`)

> **Model selection rationale:** Opus 4.7 leads Sonnet 4.6 by 8.0pp on SWE-bench Verified at 1.67× higher input cost and lower throughput. The gap materializes on multi-step agentic chains and novel-reasoning work; it narrows to near-parity on single-pass review. OCX's policy: Opus for one-way-door architecture and max-tier complex implementation; Sonnet for standard review / testing / implementation; Haiku only for read-only exploration and narrow single-pass tasks. Per-tier overrides live in `.claude/artifacts/adr_tier_model_correlation.md` and the per-skill `overlays.md` files. Source benchmark data: `.claude/artifacts/research_model_capability_matrix.md`.

This replacement is ~60 words (the current is ~40); the length is justified because the old number was wrong and the new paragraph points at the research file rather than re-stating benchmarks that will age.

## Rollout Plan

Each step is an independently commit-able, independently revertible config change. Steps are ordered so cheap / low-risk changes ship first and each subsequent step builds on the grammar established by the prior one. No step depends on code changes in `crates/` or test changes in `test/`.

### Step 1: Fix the stale rationale (zero new axis)

**Files:** `.claude/rules/workflow-swarm.md` (lines 35, 51)
**Effect:** Replaces the "within 1.2pp / 5× lower cost" string with the accurate current-landscape version per the section above.
**Risk:** Nil. Pure doc fix.
**Gate:** `task claude:verify` (lint / structural tests).
**Revert path:** Single-file revert.

### Step 2: Add `--tester=sonnet|opus` axis (single skill)

**Files:** `.claude/skills/swarm-execute/overlays.md`, `classify.md`, `SKILL.md`.
**Effect:** tier=max defaults `--tester=opus`; tier=low/high stay sonnet. Classifier triggers on "Subsystems Touched ≥2" (reuses the existing builder trigger set) and on tier=max without any further signal (tier=max is itself the signal).
**Risk:** Low. Tester is already run at tier=max; this upgrades the model, does not add/remove invocations. Small cost increase at tier=max only.
**Gate:** `task claude:verify` + manual dry-run of `/swarm-execute --tier=max` to confirm resolved config prints `tester=opus`.
**Revert path:** Revert the three-file commit.

### Step 3: Add `--doc-reviewer=haiku|sonnet` axis (two skills)

**Files:** `swarm-execute/{overlays.md, classify.md, SKILL.md}`, `swarm-review/{overlays.md, classify.md, SKILL.md}`.
**Effect:** tier=low with ≤2 doc files and no `docs/user-guide.md` touch → `--doc-reviewer=haiku`. All other cases stay Sonnet.
**Risk:** Low. Haiku is Anthropic's recommended single-file review tier and the Qodo study puts it ahead of Sonnet 4.5 on single-pass review quality. Fenced by file-count trigger.
**Gate:** `task claude:verify` + dry-run with a 1-file doc diff (expect haiku) and a user-guide diff (expect sonnet).
**Revert path:** Revert the six-file commit.

### Step 4: Add `--researcher=haiku|sonnet` axis (plan skill only)

**Files:** `swarm-plan/{overlays.md, classify.md, SKILL.md}`.
**Effect:** tier=low with `--research=1` AND narrow-scope signal → `--researcher=haiku`. All multi-axis research (`--research=3`) stays Sonnet. Context-cap escalation: if projected prompt exceeds 150k, bump to Sonnet.
**Risk:** Low-medium. Haiku is appropriate for narrow factual lookups; context-cap fence is critical — document it explicitly in the axis trigger. Watch for silent truncation reports in post-launch usage.
**Gate:** `task claude:verify` + dry-run with a single-axis narrow-scope research target (expect haiku) and a three-axis research target (expect sonnet).
**Revert path:** Revert the three-file commit.

### Step 5: Add `--reviewer=haiku|sonnet|opus` axis (two skills, three values — the widest axis)

**Files:** `swarm-execute/{overlays.md, classify.md, SKILL.md}`, `swarm-review/{overlays.md, classify.md, SKILL.md}`.
**Effect:**
  - tier=low with no structural markers from the security list → `--reviewer=haiku`
  - Any of `oci/**`, `package_manager/**`, auth/crypto/signing paths → `--reviewer=sonnet` minimum (reuses existing breadth-escalation trigger set)
  - tier=max AND `--breadth=adversarial` → `--reviewer=opus`
  - Else: sonnet
**Risk:** Medium. Reviewer is the workhorse — regressions here are felt across every run. Three-value axis has more edge cases than two-value. Security-path fence is critical.
**Gate:** `task claude:verify` + targeted dry-runs at each value (low-trivial → haiku, oci-path → sonnet, max-adversarial → opus) + one real review run at tier=low to confirm Haiku quality on an actual small diff.
**Revert path:** Revert the six-file commit. If regression is suspected post-commit, revert and open an issue; do not partially disable.

### Step 6 (optional): Shared `.claude/skills/model-matrix.md`

**Files:** New `.claude/skills/model-matrix.md` (or similar location); cross-read references added to the three `classify.md` files.
**Effect:** Single source of truth for the full (worker × tier × override) matrix, following the precedent of `execute/classify.md:41-43` cross-reading `plan/classify.md`. No behavior change — drift prevention only.
**Risk:** Very low (additive, no behavior change). Only justified once all five axes are in place and duplication becomes visible.
**Gate:** `task claude:tests` — add a structural test that resolves cross-reads.
**Revert path:** Delete the file and revert the cross-read pointers.

**Order rationale:** Step 1 has zero risk and fixes a known-stale claim, so it ships first. Step 2 is a pure tier=max upgrade (no downgrades anywhere) and has the narrowest blast radius. Steps 3 and 4 are tier=low downgrades fenced by strong signals — low-risk. Step 5 is the widest axis and carries the most risk; it ships last so any regressions discovered in earlier steps inform trigger tuning. Step 6 is optional maintenance.

## Future Work (Out of Scope)

### OpenHands-style LLM judge (dynamic per-task routing)

Replace the classifier's static signal→overlay mapping with a cheap judge-model call that scores task complexity at invocation time and selects the reasoning model from the score. Per `research_multi_agent_model_routing.md`, this is the most transferable dynamic routing pattern in the survey and the strongest single mechanism for reacting to *actual* task difficulty rather than structural proxies. It would make `--builder=sonnet|opus` responsive to the actual complexity of the work, not just to Subsystems-Touched count. Requires new infrastructure: a judge-model invocation in every classify path, a per-worker complexity threshold, and a post-launch analytics loop to calibrate the threshold. Deferred because Claude Code has no native judge primitive; implementing one from scratch is a Claude-Code-subsystem-level change, not an AI-config change.

### Batch API integration

Batch Opus 4.7 at $2.50/MTok input undercuts standard Sonnet 4.6 at $3.00/MTok input. For non-latency-sensitive workers (post-session doc audits, offline review passes), a `--batch` overlay could route to batch Opus for strictly higher quality at strictly lower cost. Requires a distinct invocation path that knows how to yield while the batch completes (typically within the 24-hour window) and a resume/retrieve pathway. Deferred because the current swarm skills are all interactive; introducing async-batch semantics is a control-flow change, not a config change.

### Intra-worker model split (CrewAI `function_calling_llm` pattern)

Workers making many structured tool calls (builder during implementation, tester during test authoring) could use a cheaper model for tool dispatch and Sonnet for reasoning, per the CrewAI pattern surveyed in `research_multi_agent_model_routing.md`. This is not expressible in the `model:` frontmatter field of a Claude Code agent; it is a framework-level change to how the orchestrator invokes the worker. Deferred for the same reason as the LLM-judge mechanism: not a Claude Code primitive today.

## References

- `.claude/artifacts/meta-plan_tier_model_correlation.md` — approved meta-plan scope, in-scope / out-of-scope boundaries
- `.claude/artifacts/research_model_capability_matrix.md` — Opus 4.7 / Sonnet 4.6 / Haiku 4.5 capability + pricing table; SWE-bench and ARC-AGI-2 benchmark deltas; source for the "8.0pp / 1.67× / 200k context cap" numbers throughout this ADR
- `.claude/artifacts/research_multi_agent_model_routing.md` — framework-pattern survey (Aider, Cline, Claude Code, CrewAI, AutoGen, LangGraph, OpenHands, Cursor); source for the Routing Mechanism Taxonomy, Signal Inputs matrix, Anti-Patterns list, and "no framework uses Haiku for judgment" constraint
- `.claude/artifacts/research_ocx_internal_model_signals.md` — catalog of signals already computed by `classify.md`; source for the four propagation options and the "seven of nine workers are tier-invariant" baseline
- `.claude/rules/workflow-swarm.md` — canonical worker table, tier vocabulary, codex overlay; files to be edited in the rollout plan
- `.claude/skills/swarm-plan/overlays.md`, `classify.md` — existing `--architect` / `--research` axis definitions, grammar to mirror
- `.claude/skills/swarm-execute/overlays.md`, `classify.md` — existing `--builder` / `--loop-rounds` / `--review` / `--codex` axis definitions
- `.claude/skills/swarm-review/overlays.md`, `classify.md` — existing `--breadth` / `--rca` / `--codex` axis definitions; structural markers table reused for reviewer/tester security-path floor
- `.claude/agents/worker-*.md` — frontmatter defaults (unchanged by this ADR, referenced for baseline)
