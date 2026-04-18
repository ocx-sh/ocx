# Plan: Tier ↔ Worker Model Correlation

## Classification

- **Scope**: Medium (AI-config only; 6 sequential steps; ~15 files touched total)
- **Reversibility**: Two-Way Door (each step is a single revertible commit)
- **Tier**: `high`
- **Overlays**: `architect=sonnet` (completed), `research=3` (completed), `codex=off` (Two-Way Door)
- **Subsystems Touched**: 1 (AI-config / `.claude/**`)

## Source Artifacts

- ADR: `.claude/artifacts/adr_tier_model_correlation.md`
- Research: `.claude/artifacts/research_{model_capability_matrix,multi_agent_model_routing,ocx_internal_model_signals}.md`
- Meta-plan: `.claude/artifacts/meta-plan_tier_model_correlation.md`

## Execution Mode

**Config-only plan.** No Rust, no Python, no build artifacts. All changes are Markdown edits to `.claude/rules/` and `.claude/skills/swarm-*/`. No contract-first TDD cycle (Stub/Specify/Implement/Review) applies — those phases assume code. Replaced with a **per-step edit → verify → commit** cycle, one commit per step.

**Verification gate after every step**: `task claude:verify` (runs `claude:lint:links`, `claude:lint:shell`, `claude:tests` — structural parity and dead-glob detection).

**No pushes.** Each step is a local commit only. Human decides when to push.

## Step 1 — Fix stale rationale in workflow-swarm.md

**Objective**: Replace the "Sonnet 4.6 within 1.2pp of Opus on SWE-bench at 5× lower cost" string with an accurate current-landscape version that points to the research artifact.

**Files**:
- `.claude/rules/workflow-swarm.md` lines 35 and 51

**Edit specification**:

Replace line 35 (worker table row for `worker-reviewer`):
```
| `worker-reviewer` | sonnet | Read, Glob, Grep, Bash | Code review/security/spec-compliance (diff-scoped, Sonnet 4.6 within 1.2pp of Opus on SWE-bench at 5× lower cost) |
```
with:
```
| `worker-reviewer` | sonnet (default) | Read, Glob, Grep, Bash | Code review/security/spec-compliance (diff-scoped; model scales per tier via `--reviewer` overlay — see `.claude/artifacts/adr_tier_model_correlation.md`) |
```

Replace line 51 (Model selection rationale block) with the paragraph in ADR section "Verification Honesty Update" (paraphrased):
> **Model selection rationale:** Opus 4.7 leads Sonnet 4.6 by 8.0pp on SWE-bench Verified at 1.67× higher input cost and lower throughput. The gap materializes on multi-step agentic chains and novel-reasoning work; it narrows to near-parity on single-pass review. OCX's policy: Opus for one-way-door architecture and max-tier complex implementation; Sonnet for standard review / testing / implementation; Haiku only for read-only exploration and narrow single-pass tasks. Per-tier overrides live in `.claude/artifacts/adr_tier_model_correlation.md` and the per-skill `overlays.md` files. Source benchmark data: `.claude/artifacts/research_model_capability_matrix.md`.

**Acceptance**:
- `task claude:verify` passes (catalog parity, dead-glob detection)
- `grep -n "1.2pp" .claude/rules/workflow-swarm.md` returns empty
- `grep -n "8.0pp" .claude/rules/workflow-swarm.md` returns at least one match

**Commit message**: `chore(claude): correct stale Sonnet vs Opus benchmark rationale`

**Risk**: Nil (pure doc fix).

**Revert**: `git revert <sha>`.

## Step 2 — Add `--tester=sonnet|opus` axis (swarm-execute only)

**Objective**: Introduce the `--tester` overlay axis with tier=max default `opus`, all other tiers default `sonnet`. Upgrades tier=max test generation from Sonnet to Opus to close the asymmetry documented in the ADR.

**Files**:
- `.claude/skills/swarm-execute/SKILL.md` — argument parser entry
- `.claude/skills/swarm-execute/overlays.md` — axis definition + per-tier defaults table row
- `.claude/skills/swarm-execute/classify.md` — trigger rules
- `.claude/skills/swarm-execute/tier-max.md` — prompt injection for tester worker launch

**Edit specification**:
1. `SKILL.md`: add `--tester=sonnet|opus` to the argument syntax block (follows `--builder=`).
2. `overlays.md`: add a `### tester axis` section after the builder axis section, mirroring the existing shape (description, value table, per-tier defaults). Add a `tester` row to the per-tier defaults table at the end.
3. `classify.md`: add a tester trigger rule — default at each tier, overridable by user flag, mandatory opus at tier=max.
4. `tier-max.md`: in the `worker-tester` launch block, pass `model: opus` as part of the prompt (identical pattern to builder opus override).

**Acceptance**:
- `task claude:verify` passes
- `/swarm-execute --tier=max` — the announcement block (printed on every run before the tier file loads, per `swarm-execute/SKILL.md:82-96`) includes `tester=opus (tier default)`
- `/swarm-execute --tier=low` — announcement includes `tester=sonnet (tier default)`
- User flag `--tester=sonnet` at tier=max is silently overridden; announcement prints `tester=opus (tier=max mandatory — user flag overridden)`. Mirrors the builder mandatory-at-max pattern at `swarm-execute/overlays.md:35-39`.

**Commit message**: `chore(swarm): add --tester=sonnet|opus axis with tier=max opus default`

**Risk**: Low (upgrade-only; no Haiku downgrade).

**Revert**: Single-commit revert; no downstream dependencies.

## Step 3 — Add `--doc-reviewer=haiku|sonnet` axis (swarm-execute + swarm-review)

**Objective**: Enable Haiku for single-file / narrow-scope doc audits at tier=low; stay Sonnet for full user-guide audits.

**Files**:
- `.claude/skills/swarm-execute/{SKILL.md, overlays.md, classify.md, tier-low.md}`
- `.claude/skills/swarm-review/{SKILL.md, overlays.md, classify.md, tier-low.md}`
- Executor must verify `tier-low.md` is the worker-doc-reviewer launch site before editing; if launch happens in another tier file, expand the file list accordingly.

**Edit specification**:
1. Add `--doc-reviewer=haiku|sonnet` to argument syntax in both skills' SKILL.md.
2. Add `### doc-reviewer axis` section to both `overlays.md` files.
3. Add trigger rule to both `classify.md` files: `tier=low` AND `diff touches ≤2 doc files` AND `no docs/user-guide.md` → `haiku`; otherwise `sonnet`.
4. Prompt injection `model: haiku` at worker-doc-reviewer launch sites in tier-low files.

**Trigger signal specification**: The "diff touches ≤2 doc files" condition uses the existing `swarm-review/classify.md` diff metric (`file_count` of `website/**/*.md` or `CHANGELOG.md` paths). Document this precisely in the trigger rule so the classifier author can implement it mechanically.

**Acceptance**:
- `task claude:verify` passes
- 1-file doc diff at tier=low → announcement (`swarm-execute/SKILL.md:82-96`) prints `doc-reviewer=haiku (tier=low, narrow-scope)`
- `website/src/docs/user-guide.md` edit → announcement prints `doc-reviewer=sonnet (user-guide trigger)`
- tier=high → announcement prints `doc-reviewer=sonnet (tier default)`

**Commit message**: `chore(swarm): add --doc-reviewer=haiku|sonnet axis with narrow-scope fence`

**Risk**: Low. Qodo 400-PR study places Haiku 4.5 ahead of Sonnet 4.5 on single-pass review quality (source: `.claude/artifacts/research_model_capability_matrix.md`). Fenced by file-count trigger and user-guide exclusion.

**Revert**: Single-commit revert.

## Step 4 — Add `--researcher=haiku|sonnet` axis (swarm-plan only)

**Objective**: Enable Haiku for narrow factual lookups at tier=low; stay Sonnet for synthesis and multi-axis research.

**Files**:
- `.claude/skills/swarm-plan/SKILL.md`, `overlays.md`, `classify.md`, `tier-low.md`, `tier-high.md`, `tier-max.md`

**Edit specification**:
1. Add `--researcher=haiku|sonnet` to argument syntax.
2. Add `### researcher axis` section to `overlays.md`.
3. Trigger rule: `tier=low` AND `--research=1` AND narrow-scope signal → `haiku`. Context-cap escalation: if projected worker prompt + loaded docs exceed 150k tokens, bump to `sonnet`.
4. Prompt injection in `tier-low.md` at worker-researcher launch.

**Narrow-scope signal specification** (deferred finding from architect): operationalize as "prompt mentions a single named concept AND no `--research=3` axis AND no `cross-subsystem` keyword." Document the heuristic with examples in `classify.md` so the next author can refine it.

**Context-cap guard**: add a note in the trigger rule: "if the researcher would need WebFetch on multiple sources or read >5 files, escalate to sonnet regardless of tier." This prevents silent truncation.

**Acceptance**:
- `task claude:verify` passes
- `/swarm-plan low "check if crate X has a CVE"` → announcement (per swarm-plan SKILL.md Step 5 "Announce final config") prints `researcher=haiku (tier=low, narrow scope)`
- `/swarm-plan high "add storage layer"` → announcement prints `researcher=sonnet (tier default)` (research=1 but not narrow scope)
- `/swarm-plan max "protocol refactor"` → announcement prints `researcher=sonnet` × 3 parallel (research=3 overrides haiku)

**Commit message**: `chore(swarm): add --researcher=haiku|sonnet axis for narrow-scope lookups`

**Risk**: Low-medium (context-cap guard is critical; watch for truncation reports post-launch).

**Revert**: Single-commit revert.

## Step 5 — Add `--reviewer=haiku|sonnet|opus` axis (swarm-execute + swarm-review)

**Objective**: Widest axis — three values, two skills. Haiku at tier=low for non-security diffs; Sonnet default; Opus at tier=max under adversarial breadth.

**Files**:
- `.claude/skills/swarm-execute/SKILL.md`, `overlays.md`, `classify.md`, `tier-low.md`, `tier-high.md`, `tier-max.md`
- `.claude/skills/swarm-review/SKILL.md`, `overlays.md`, `classify.md`, `tier-low.md`, `tier-high.md`, `tier-max.md`

**Edit specification**:
1. Add `--reviewer=haiku|sonnet|opus` to argument syntax in both skills.
2. Add `### reviewer axis` section to both `overlays.md` files (three-value table).
3. Trigger rule in both `classify.md`:
   - tier=low AND no structural markers from `swarm-review/classify.md:48-61` → `haiku`
   - Any of `oci/**`, `package_manager/**`, `auth/**`, `crypto/**`, `signing/**`, new `crates/*/Cargo.toml`, `deny.toml`, `ocx_schema/**` → `sonnet` minimum (overrides haiku)
   - tier=max AND `--breadth=adversarial` → `opus`
   - Otherwise: `sonnet`
4. Prompt injection at all reviewer launch sites in all six tier files.

**Security-path reuse**: pull the structural markers list from `swarm-review/classify.md:48-61` rather than duplicating — cross-read pattern per ADR.

**Acceptance**:
- `task claude:verify` passes
- tier=low trivial diff → announcement prints `reviewer=haiku`
- tier=low touching `crates/ocx_lib/src/oci/` → announcement prints `reviewer=sonnet (structural marker)`
- tier=max `--breadth=adversarial` → announcement prints `reviewer=opus`
- tier=high → announcement prints `reviewer=sonnet (tier default)`
- One real review run at tier=low on a small diff to confirm Haiku-review quality isn't regressing (human spot-check)

**Commit message**: `chore(swarm): add --reviewer=haiku|sonnet|opus axis with security floor`

**Risk**: Medium (reviewer is the workhorse; regressions felt across every run). The structural-marker security floor is critical.

**Revert**: Single-commit revert; do not partially disable.

## Step 6 (optional) — Shared `model-matrix.md`

**Objective**: Drift-prevention infrastructure. Single source of truth for (worker × tier × override) matrix, cross-read by all three `classify.md` files.

**Files**:
- New: `.claude/skills/model-matrix.md` (or under a `swarm-common/` dir; decide at implementation)
- Modify: three `classify.md` files to add cross-read pointers

**Gate**: Extend `.claude/tests/test_ai_config.py` with a structural test verifying the cross-read references resolve.

**Condition for shipping**: only justified once Steps 2–5 are all committed AND duplication between the three skills becomes visible. Judgment call; may be skipped.

**Commit message**: `chore(swarm): centralize model matrix in shared reference`

**Risk**: Very low (additive, no behavior change).

**Revert**: Delete file, revert pointers.

## Handoff Block (for /swarm-execute)

### Classification
- **Scope**: Medium
- **Reversibility**: Two-Way Door
- **Tier**: high
- **Overlays**: architect=sonnet (done), research=3 (done), codex=off

### Artifacts
- `.claude/artifacts/adr_tier_model_correlation.md`
- `.claude/artifacts/research_model_capability_matrix.md`
- `.claude/artifacts/research_multi_agent_model_routing.md`
- `.claude/artifacts/research_ocx_internal_model_signals.md`
- `.claude/artifacts/plan_tier_model_correlation.md` (this file)

### Executable Phases
This plan does **not** use the Stub → Specify → Implement → Review cycle because there is no code. Instead, execute Steps 1–5 sequentially; Step 6 is optional. Each step is one commit, each commit has a verify gate and a single-commit revert path.

**Recommended executor**: `worker-builder` with focus=`refactoring` (markdown edits only) and `model: sonnet`. `model: opus` is overkill for config edits.

### Deferred Findings (from ADR)
- Prompt-injection fragility compounds (5 injection points after Step 5 vs. 1 today) — no remediation in scope
- `--researcher=haiku` context-cap threshold (150k) is a guess; calibrate post-launch
- "Narrow-scope lookup" heuristic is not yet machine-parseable; refine during Step 4 implementation
- No explicit post-rollout telemetry design — noted for a future ADR
- Step 6 is judgment-gated on whether duplication becomes visible in practice

### Next Step
```
/swarm-execute .claude/artifacts/plan_tier_model_correlation.md
```

Or execute steps one-at-a-time manually, committing after each `task claude:verify` passes.
