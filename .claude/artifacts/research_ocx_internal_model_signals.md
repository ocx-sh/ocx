# OCX Internal Signals for Model Routing

**Research date**: 2026-04-19
**Purpose**: Domain-axis research for OCX tier→model correlation audit. Catalogs task-complexity signals that exist in OCX's swarm pipeline and could be propagated to per-worker model selection.

## Signals already computed by `classify.md` (swarm-plan)

| Signal | Purpose | Stored in plan artifact? | Passed to workers today? |
|---|---|---|---|
| `Tier` (low/high/max) | Primary classifier output | Yes — `Classification.Tier:` | Indirectly: swarm-execute reads verbatim from plan header |
| `Reversibility` (Two-Way / One-Way Med / High) | Escalates tier; gates `--codex` | Yes — `Classification.Reversibility:` | Indirectly: execute maps to `--codex` and suggests `--builder=opus` |
| `Scope` (Small/Medium/Large) | Fallback when `Tier:` absent | Yes — `Classification.Scope:` | Indirectly as fallback tier only |
| `--architect=inline\|sonnet\|opus` overlay | Design phase worker model | Yes — `Classification.Overlays:` | Yes: swarm-plan launches worker-architect with the named model |
| `--research=skip\|1\|3` overlay | Researcher worker count | Yes — `Classification.Overlays:` | Yes: controls researcher launch count |
| `codex=on\|off` overlay | Cross-model gate | Yes — `Classification.Overlays:` | Yes: controls codex-adversary invocation |
| Estimated file count (≤3, ≤15) | Tier signal | No — consumed, not recorded | No |
| Confidence flag | Meta-plan gate trigger | No | No |
| GitHub labels | Tier hints / codex trigger | No — results reflected in overlays | No |
| GitHub PR file list | Discover scope input | No | No |
| Keyword signals ("novel algorithm", "cross-subsystem", etc.) | Overlay trigger | No — consumed; result in overlays | No |

Sources: `.claude/skills/swarm-plan/classify.md:1-82`, `.claude/skills/swarm-plan/SKILL.md:167-195`

## Signals computed elsewhere in the pipeline

### swarm-execute/classify.md — reads from plan header

| Source field | Signal | Surfaced to workers? |
|---|---|---|
| `Tier:` field | Used verbatim | Via resolved tier → builder model (sonnet/opus) |
| `Scope:` field | Fallback only | Via tier fallback |
| `Reversibility: One-Way Door Medium/High` | Forces `--codex` | Controls codex-adversary |
| `Overlays: codex=on\|off` | Adopted verbatim | Controls codex-adversary |
| `Overlays: architect=opus` | Suggests `--builder=opus` | Affects builder worker model |
| `Subsystems Touched` (plan body prose) | ≥2 subsystems → `--builder=opus` | Yes — only plan-body signal that currently affects model choice |

### swarm-review/classify.md — computed live from diff, NOT stored

| Signal | Source command | Surfaced today? |
|---|---|---|
| `file_count` | `git diff <base>...HEAD --name-only \| wc -l` | Announcement string only; not passed to reviewer workers |
| `lines_changed` | `git diff <base>...HEAD --shortstat` | Announcement string only |
| `subsystems_touched` | Paths matched against `.claude/rules.md` "By subsystem" table | Announcement string; controls tier escalation |
| `structural_markers` | Path patterns: `oci/**`, `package_manager/**`, auth/crypto, `Cargo.toml` deps, `ocx_schema/**`, new `crates/*/Cargo.toml` | Controls overlay selection (breadth + codex) but not model selection |
| `pr_labels` | `gh pr view --json labels` | Controls tier escalation; not passed to workers |

## Current model assignments across all workers

| Worker | Model (frontmatter) | Tier-coupled? | Override mechanism |
|---|---|---|---|
| `worker-architect` | opus | Yes — via `--architect=` overlay | User flag or classifier; tier=max mandatory opus |
| `worker-architecture-explorer` | sonnet | No — fixed | None |
| `worker-builder` | sonnet | Yes — via `--builder=` overlay | Orchestrator passes `model: opus` in prompt; tier=max mandatory |
| `worker-explorer` | haiku | No — fixed | None |
| `worker-reviewer` | sonnet | No — fixed | None |
| `worker-tester` | sonnet | No — fixed | None |
| `worker-researcher` | sonnet | No — fixed | None |
| `worker-doc-reviewer` | sonnet | No — fixed | None |
| `worker-doc-writer` | sonnet | No — fixed | None |

**Seven of nine workers are tier-invariant.** Only architect and builder have tier-coupled model selection today.

**Builder override mechanism:** Opus override passed dynamically in the orchestrator prompt (`orchestrator passes model: opus` — workflow-swarm.md:47), not via frontmatter. The `--builder` overlay axis is the documented surface; prompt injection is the implementation.

Sources: `.claude/agents/*.md` frontmatter, `.claude/rules/workflow-swarm.md:27-51`

## Cheaply computable additional signals

1. **`git diff --stat <base>`** → file_count + lines_changed. ~0ms. Already computed by swarm-review; not available at plan or execute time. Directly maps to tier thresholds.

2. **`git diff --name-only <base> | path-mapper`** → subsystem set + count. ~milliseconds. Already computed by swarm-review from `.claude/rules.md` "By subsystem" table. Could be run at swarm-execute Phase 1 against the current diff.

3. **Plan header `Subsystems Touched` count** — already in plan body, already parsed by execute at Phase 1. Currently triggers only `--builder=opus`. Zero new infrastructure to extend.

4. **Plan header `Reversibility:`** — already read by execute. Currently only wires to `--codex`. Could also gate reviewer model selection (One-Way Door → heavier review).

5. **Plan header `--loop-rounds` resolved value** — computed by overlay resolution, announced in the config block. `loop-rounds=1` is a direct proxy for Two-Way Door simplicity. No git commands needed.

6. **Structural marker path categories** from review/classify.md — `oci/**`, `auth/`, `crypto/`, `signing/`, `package_manager/**` — already defined as security/complexity escalation signals and mirrored in the hook's `CONTEXT_REMINDERS` table (`.claude/hooks/post_tool_use_tracker.py:28-35`). Zero new logic to reuse.

## Potential signal → model mapping structure

Four locations where the mapping could live, ranked least to most new infrastructure:

### 1. Orchestrator prompt injection (current mechanism for builder)

Extend the `model: opus` prompt injection pattern to other workers. Requires no new grammar. Works today. Downside: opaque to users, no override path.
- Implementation sites: per-phase worker launch blocks in `swarm-execute/tier-low.md`, `tier-high.md`, `tier-max.md`

### 2. New overlay axes per worker type

Extend the overlay grammar with `--reviewer=haiku|sonnet`, `--researcher=haiku|sonnet`, etc. Follows the exact same `--builder=sonnet|opus` and `--architect=inline|sonnet|opus` pattern. User flag wins; mandatory floor at tier=max.
- Implementation sites: `swarm-execute/overlays.md` (axis definition), `swarm-execute/classify.md` (trigger rules), `swarm-review/overlays.md` (breadth-aware reviewer model axis)

### 3. Per-tier defaults table extension

Add model rows to the existing defaults cheat-sheet tables in each `overlays.md`. Keeps all per-tier defaults co-located with other axes. Requires option 2 first.
- Implementation site: bottom of `swarm-execute/overlays.md` and `swarm-review/overlays.md`

### 4. Shared `model-matrix.md` cross-read by all three classify files

A single table of (worker × tier × signal-override) → model. All classify files cross-read it via `Read`. Follows the precedent of execute/classify.md:41-43 cross-reading plan/classify.md directly for the free-text tier signal table. Single source of truth for model routing logic.
- Implementation site: new file (e.g., `.claude/skills/swarm-execute/model-matrix.md` or a shared location)

## Precedent patterns in existing swarm skills

- **Overlay axis grammar** (`--axis=value1|value2` with per-tier defaults table) is consistent across all three skills. New axes follow the same pattern: add to `overlays.md` axis definitions → add trigger rules to `classify.md` → add to per-tier defaults table → add to `SKILL.md` argument parser.

- **Cross-skill file reads**: execute/classify.md cross-reads plan/classify.md directly (line 41-43: "Read ... directly"). A shared model-matrix can follow this same pattern.

- **Mandatory-at-tier=max floor**: `builder=opus` and `codex=on` are mandatory at tier=max regardless of user flags. This provides a safety floor pattern for any new model axes.

- **User flag always wins** (except tier=max mandatory constraints): a `--reviewer=haiku` from the user would downgrade even if the classifier picked sonnet. The exception pattern provides a safety floor.

- **Security path escalation** already raises `--breadth` from minimal → full → adversarial based on structural marker paths (`oci/**`, `package_manager/**`, auth/crypto). The same paths could also escalate model selection, reusing the existing condition.

## Observations

1. **The richest signal set (swarm-review) is ephemeral.** `file_count`, `lines_changed`, `subsystems_touched`, `structural_markers` are computed live and discarded after tier selection. Adding a `--reviewer-model` axis driven by these signals would require either persisting them in a meta-plan file or recomputing at tier dispatch time. Cheapest path: recompute at start of tier file via `git diff --shortstat`.

2. **Five plan-header fields are already persisted across plan→execute handoff and partially wired to model selection.** `Scope`, `Reversibility`, `Tier`, `Overlays`, `Subsystems Touched` survive the handoff. Three already affect builder model. Extending to reviewer/tester/researcher requires only new rules in execute/classify.md — no changes to plan phase.

3. **`Subsystems Touched ≥2` is the highest-confidence existing signal for complexity.** Already triggers `--builder=opus`. Same signal could gate `--researcher=sonnet` minimum (vs. haiku for single-subsystem work). As free text in plan body today, requires heuristic parsing; adding a structured `Subsystems: N` count field to the Classification block would make it machine-readable.

4. **`--loop-rounds` is a free proxy signal with no parsing cost.** `loop-rounds=1` (tier=low, Two-Way Door) and `loop-rounds=3` (tier=high/max) are already resolved and announced. A rule `loop-rounds=1 → reviewer-model=haiku` would need no git commands and no new plan fields.

5. **Security/complexity path escalation already exists at the breadth level but not the model level.** Review/classify.md structural_markers already raise `--breadth` to full/adversarial based on path patterns. Adding a model-escalation condition to the same markers is a one-line extension and would prevent haiku reviewers on security-sensitive diffs.

## Sources (file:line refs within OCX)

- `.claude/skills/swarm-plan/classify.md:1-82` — tier signal table, confidence rules, overlay triggers, GitHub context inputs
- `.claude/skills/swarm-plan/SKILL.md:44-46, 167-195` — handoff block format, Subsystems Touched parsing
- `.claude/skills/swarm-execute/classify.md:16-78` — plan header fields consumed, Subsystems Touched trigger for builder=opus
- `.claude/skills/swarm-execute/SKILL.md:44-46` — plan header parsing protocol
- `.claude/skills/swarm-execute/overlays.md:26-38` — builder axis definition, mandatory tier=max constraint
- `.claude/skills/swarm-review/classify.md:18-76` — diff metric computation, structural markers table, PR label signals
- `.claude/skills/swarm-review/SKILL.md:100-111` — announce string (only place diff metrics surface today)
- `.claude/rules/workflow-swarm.md:27-51` — canonical worker table with models, builder opus override rationale
- `.claude/agents/worker-*.md` (all 9 files) — frontmatter `model:` for each worker type
- `.claude/hooks/post_tool_use_tracker.py:28-35` — CONTEXT_REMINDERS: path→subsystem mapping (mirrors review structural markers)
- `.claude/artifacts/meta-plan_tier_model_correlation.md:29-42` — prior current-state snapshot confirming tier-coupled knob count
