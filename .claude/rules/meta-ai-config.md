---
paths:
  - .claude/**
---

# AI Configuration Meta-Rule

Govern how `.claude/` artifacts (skills, rules, agents, hooks) maintained. Load when work on any `.claude/` file.

## Three Activation Layers

Claude Code have three rule-activation mechanisms. Each different purpose — conflate = dead rules or context bloat.

| Layer | Activation | Use for | Example |
|---|---|---|---|
| **Rule** (`.claude/rules/*.md`) | `paths:` glob — fire when edit match file | Standards/context need *while writing* file | `quality-rust.md` on `**/*.rs` |
| **Skill** (`.claude/skills/<name>/SKILL.md`) | `description` match by LLM vs current task | Workflow + criteria for task topic | `deps` for "add a crate" |
| **Catalog** (`.claude/rules.md`) | Read-on-demand during planning, point from CLAUDE.md | Discover what rules exist *before* file open | — |

Path-scoped rules no fire during plan/research/architecture — no file open. Skills need LLM already know skill exist. Catalog (`.claude/rules.md`) close gap: authoritative map browse during plan/research. Any `.claude/rules/` change must reflect in catalog same commit; structural tests enforce parity.

### Current Global Rules (no `paths:` frontmatter)

Three rules under `.claude/rules/` no `paths:` frontmatter, load unconditional every session. Also list in `rules.md` "Globals" footer. Any change to set must update both enumeration and `rules.md`.

1. `quality-core.md` — universal code quality
2. `product-tech-strategy.md` — tech golden paths
3. `workflow-intent.md` — work-type router (must fire at first touch)

Two more always-load files reach Claude by different mechanism, *not* count here because no use path-scope frontmatter layer:

- `.claude/rules.md` — catalog itself; `@`-import from `CLAUDE.md`
- `CLAUDE.md` — project root instructions; load by Claude Code direct

`meta-ai-config.md` path-scoped to `.claude/**` (not true global); load when AI config files edit.

If strict count drift from 3, `test_global_rule_count_matches` fail.

## Core Principle: Context Budget

Every rule, skill description, CLAUDE.md line compete same context window. Bloat config = Claude ignore instructions.

| Artifact | Budget | Impact |
|----------|--------|--------|
| CLAUDE.md | <200 lines | Load every request — every line cost attention |
| Rules (global) | <200 lines each | Load every request — minimize globals |
| Rules (scoped) | <200 lines each | Load only on path match — prefer scoping |
| Skill descriptions | 2% of context window total | All descriptions load at session start |
| Skill body (SKILL.md) | <500 lines | Load only when invoked — safe for detail |
| Hooks | Zero context cost | External scripts, never in context window |
| Subagents | Isolated context | No impact on main session |

**Decision tree — where this instruction belong?**

```
Must Claude know it every session?
├─ Yes → Is it file/directory-specific?
│  ├─ Yes → .claude/rules/ with paths: scoping
│  └─ No → CLAUDE.md (only if removing it causes mistakes)
└─ No → Is it invoked manually or auto-triggered?
   ├─ Manual with side effects → Skill with disable-model-invocation: true
   ├─ Auto-triggered by context → Skill with good description
   └─ Pure automation, no LLM judgment → Hook (deterministic, zero cost)
```

## Research Protocol

**Every edit to AI config must be research-informed.** AI config work use **canonical multi-agent research pattern** define in `/swarm-plan` (Phases 1-2: Discover + Research). No reinvent — delegate.

Spawn workers parallel before write:

- **`worker-explorer`** (1-2 agents) — Check existing `.claude/` artifacts for conventions, cross-refs, prior decisions. Map neighborhood of artifact create or change.
- **`worker-researcher`** (1-3 agents, split by axis when non-trivial):
  - *Claude Code / tooling axis* — `code.claude.com/docs`, frontmatter conventions, new hook/skill/agent features
  - *Domain axis* — best practices for artifact subject (Rust patterns, OCI spec, cargo-deny, testing, etc.)
  - *Community axis* — how other projects structure similar artifacts

Persist substantial findings as `.claude/artifacts/research_[topic].md` so future AI config sessions reuse. Never author skill, rule, agent from memory alone.

See `/swarm-plan` "Research as a Reusable Primitive" for full pattern contract.

## Artifact Conventions

### Rules (`.claude/rules/*.md`)

- `paths:` frontmatter for scoped rules; omit for global
- <200 lines. If longer, split by domain.
- Structure: types → invariants → gotchas → cross-refs
- Dead glob detection: after rename directories, verify `paths:` patterns still match files
- **Shareable quality rules** use `quality-*.md` naming convention (e.g., `quality-rust.md`, `quality-python.md`). Must be **project-independent** — no refs to OCX types, modules, conventions. OCX-specific patterns live in `arch-principles.md` and `subsystem-*.md`. Shareable rules use broad `paths:` globs (e.g., `**/*.rs`) so activate regardless of project layout.

### Skills (`.claude/skills/<name>/SKILL.md` — canonical flat layout)

- `description` = #1 discovery factor — write trigger phrasing (Contextual Signal Only / CSO policy, see `.claude/artifacts/adr_ai_config_skill_description_csopolicy.md`). Forbidden verbs: `dispatches|runs|iterates|orchestrates|performs|executes|handles` — cause Claude read description as workflow and skip body. Front-load discriminating keywords (truncation cut from end). Max 1024 chars per skill.
- `argument-hint` must be quoted string
- `allowed-tools` NOT supported in frontmatter
- `disable-model-invocation: true` for action skills with side effects (commit, deploy, release)
- `triggers:` (required for `user-invocable: true` skills) — list of 3–7 literal phrases UserPromptSubmit routing hook match against user prompts (case-insensitive substring match). Hook read field at runtime from each SKILL.md — no encode triggers in hook code. Rules: each trigger ≥2 words OR clear domain token (`deps`, `commit`, `finalize`); no duplicates across skills. When add new user-invocable skill, add `triggers:` same commit or `test_user_invocable_skills_have_triggers` fail.
- Progressive disclosure: SKILL.md <500 lines, reference files for details
- `context: fork` to run in isolated subagent (protect main context)
- **No category subdirectories.** Claude Code discover skills at `.claude/skills/<name>/SKILL.md` exact, no recurse deeper for in-project skills. Nest for grouping (e.g., `personas/`, `operations/`) silently break `/slash-command` discovery. Enforce at test layer.

### Agents (`.claude/agents/worker-{name}.md`)

- `model`: haiku (exploration), sonnet (implementation/review), opus (architecture)
- `tools`: minimum need for role
- Keep concise — agents inherit project rules auto
- **Minimal anchored preamble + catalog pointer.** Agents point at `.claude/rules.md` for full rule catalog, then inline short "Always Apply" preamble (≤5 block-tier anchors, each tag with source rule file). Preamble fire at attention even when path-scoped auto-load no trigger yet. Anchors must cite source file so drift visible at review. Replace earlier "deliberate redundancy" pattern where entire rule checklists duplicate into agent bodies — that approach cause drift and heavy maintenance cost.

### Hooks (`.claude/hooks/*.py`)

- Zero context cost — only deterministic enforcement mechanism
- Exit 0 = proceed, exit 2 = block + feed stderr to Claude
- Types: `command` (shell/Python), `prompt` (LLM single-turn), `agent` (LLM multi-turn with tools)
- Write in Python (stdlib only), invoke via `uv run` for cross-platform compat
- PEP 723 inline script metadata (`# /// script`) for future dependency declaration
- Shared utilities in `hook_utils.py` — import via `sys.path` insertion
- Use `PreToolUse` for blocking, `PostToolUse` for logging/reminders (never exit non-zero)

## Plan Status Protocol

Every plan in `.claude/state/plans/plan_*.md` carries a `## Status` block at the top — first 30 lines after H1 — so `/next` and the user can read current state at a glance without scanning the full plan.

### Schema

```markdown
## Status

- **Plan:** plan_<slug>
- **Active phase:** <N> — <phase title>
- **Step:** <skill or activity, e.g. /swarm-execute → implementation>
- **Last update:** <YYYY-MM-DD> (after <commit-sha-short>: <subject>)
```

Allowed `Step` values:
- `/swarm-plan → plan-approved`
- `/swarm-execute → <stage>` (Stub, Specify, Implement, Review-Fix Loop)
- `/swarm-review → round N`
- `awaiting /swarm-review`
- `awaiting /swarm-execute (review-fix loop)`
- `awaiting /finalize`
- `finalized` (terminal — `/finalize` writes this then deletes `current_plan.md`)

### Global pointer

`.claude/state/current_plan.md` (gitignored, per-worktree):

```markdown
# Current Plan Pointer

- **Plan:** .claude/state/plans/plan_<slug>.md
- **Branch:** <branch-name>
- **Updated:** <YYYY-MM-DD HH:MM UTC>
```

`/next` reads pointer first, jumps straight to referenced plan's Status block. Absent pointer → `/next` falls back to plan-glob, then commit-subject heuristic with user prompt (state-fixer path).

### Per-skill mutation table

| Skill | Reads | Writes |
|---|---|---|
| `/swarm-plan` | — | Init Status in new plan; write `current_plan.md` |
| `/swarm-execute` | Status | Flip `Step` on phase entry/advance; bump `Last update` |
| `/swarm-review` | Status | Flip `Step` on round entry; set `awaiting /finalize` or `awaiting /swarm-execute` on verdict |
| `/commit` | Status | Bump `Last update` only (no phase advance) |
| `/finalize` | Status | **Refuse if Step ≠ `finalized` and `Active phase` not last** (`--force` overrides); on success set `Step: finalized`, delete `current_plan.md` |
| `/next` | `current_plan.md` then Status | Read-only fast path; falls back to commit-subject heuristic with `AskUserQuestion`, then writes `current_plan.md` + injects Status block (state-fixer path) |

Phase advancement (`Active phase: N → N+1`) is the orchestrator/plan-author decision encoded as Step transition — never an automatic side-effect of commits.

### Backfill + structural test

- `.claude/templates/artifacts/plan.template.md` and `bugfix_plan.template.md` carry a Status block at top so every new plan gets one for free.
- `.claude/tests/test_ai_config.py::TestPlanStatusBlock` enforces the invariant: every `plan_*.md` (excluding `meta-plan_*.md`) must contain `## Status` block with all four mandatory fields. Skips silently on a fresh checkout where no plans exist.
- `.claude/hooks/post_tool_use_tracker.py` `CONFIG_REMINDERS` table fires when a plan file is edited — reminds to bump `Last update` and check `current_plan.md` freshness.

### Why both files

- `current_plan.md` is the **fast path** for `/next` (read one small file, jump to referenced plan).
- Status block in plan file is the **truth** (survives `current_plan.md` deletion, captures plan-internal phase progression).
- Together: `current_plan.md` answers "which plan?", Status block answers "where in that plan?". Either alone is incomplete.

### Subplans (parent-stack)

A plan may spawn a subplan (e.g. a high-tier review opens its own `plan_review_*.md`, or a discovered cross-cutting refactor needs its own plan before the parent can resume). The Status schema supports nesting via an optional `**Parent plan:**` field:

```markdown
## Status

- **Plan:** plan_review_X
- **Parent plan:** plan_project_toolchain (resume after Step: finalized)
- **Active phase:** 1 — Findings triage
- **Step:** /swarm-review → round 1
- **Last update:** 2026-04-25 (after 9c2b4c9: ...)
```

Protocol:

1. **Spawn**: when a skill creates a subplan, it (a) writes the new plan with `Parent plan:` set to the current `current_plan.md` target, (b) repoints `current_plan.md` to the new subplan. The parent's Status block is untouched (its `Step` already records what triggered the spawn).
2. **Run**: standard mutation rules apply to the subplan only. `/next` always reads the active `current_plan.md`, so suggestions track the deepest in-flight plan.
3. **Return**: when the subplan reaches `Step: finalized`, `/finalize` checks `Parent plan:`. If present, instead of deleting `current_plan.md` it repoints `current_plan.md` back to the parent and bumps the parent's `Last update`. If absent, original behaviour (delete `current_plan.md`).
4. **Stack depth**: kept implicit via the chain of `Parent plan:` fields — no explicit stack file. `/next` follows one hop at most when reporting; deeper introspection is on-demand.

This keeps the common (single-plan) case zero-cost while making nested workflows recoverable.

## Cross-Session Learnings Store

Project-local JSONL store for recurring technical patterns (oci-client quirks, clippy suppressions, test flakiness). Separate from human-author `MEMORY.md`. Full schema + policy in `.claude/artifacts/adr_ai_config_cross_session_learnings_store.md`.

- **Canonical:** `.claude/state/learnings.jsonl` (per-worktree, gitignored); pending queue at `.claude/hooks/.state/learnings-pending.jsonl`; schema-mismatches quarantine to `learnings-orphan.jsonl`
- **Capture:** subagent emit `[LEARNING] { ... }` JSON → `subagent_stop_logger.py` parse + redact secrets + append → `stop_validator.py` merge at session end with fingerprint dedup, then TTL prune + confidence decay
- **Schema v1 required fields:** `schema_version=1`, `id` (uuid-v4), `created_at` (ISO-8601 UTC), `source_agent`, `category` ∈ {rust, python, ts, oci, test, clippy, mirror, build, other}, `fingerprint` (sha256(category|normalized_summary)[:16]), `summary` (≤160 chars), `confidence` (0–1), `ttl_days`. Also store: `source_session`, `evidence_ref`, `confidence_updated_at`, `occurrence_count`
- **Staged rollout:** Stage 1 (30 days from merge, track via `.claude/state/.day30-review-reminder`) log-only (`[LEARNINGS] N captured, M unique total`); Stage 2 tune promotion thresholds and emit `[LEARNING PROMOTION CANDIDATE]` blocks when `occurrence_count ≥ N` AND `confidence ≥ C`. Promotion to `MEMORY.md` always human action
- **Cleanup:** drop past `created_at + ttl_days` (default 90d); drop `confidence < 0.3`; fingerprint dedup replenish `confidence += 0.15`; decay `−0.02/day` since `confidence_updated_at`
- **Schema migration:** field additions permit no migration; renames/removals need migration script + `schema_version` bump same commit

## Anti-Patterns

1. **Global rule >200 lines** — same problem as bloat CLAUDE.md
2. **Rules match `src/**/*`** — too broad, effectively another CLAUDE.md
3. **Duplicate content** across CLAUDE.md, rules, skills — single source of truth
4. **Verbose SKILL.md** without progressive disclosure — move reference material to support files
5. **No `disable-model-invocation`** on action skills — Claude trigger unpredictable
6. **Too many auto-trigger skills** — description budget fill up, skills excluded
7. **Dead glob patterns** — rules silently never fire after directory rename
8. **Config drift** — embed project knowledge stale as code evolve
9. **Category subdirectories under `.claude/skills/`** — break slash command discovery. Caught structural by `test_all_skills_at_flat_layout`.
10. **Language rules with project-specific content** — `quality-{lang}.md` files shareable across repos. Any ref to OCX types (`PackageErrorKind`, `ReferenceManager`) or modules (`ocx_lib/`, `crates/ocx_mirror/`) belong in `arch-principles.md` or `subsystem-*.md`, not language rule. Enforced structural by `test_shareable_rules_no_ocx_leak`.
11. **Catalog drift** — add, remove, rename rule in `.claude/rules/` no update `.claude/rules.md` same commit. Enforced by `test_catalog_covers_all_rules` and `test_catalog_references_resolve`.
12. **Missing `triggers:` on user-invocable skill** — UserPromptSubmit routing hook matcher silently exclude skill, natural-language prompts never route. Enforced structural by `test_user_invocable_skills_have_triggers`.

## Consistency Checks

When edit any `.claude/` artifact:

- [ ] Frontmatter follow conventions for artifact type
- [ ] Cross-refs point to existing files
- [ ] New rules reference subsystem context rules where relevant
- [ ] CLAUDE.md stay under 200 lines
- [ ] Global rules total manageable (current 3 — monitor growth; see `### Current Global Rules` above for strict definition)
- [ ] AI config structural tests pass: `task claude:tests`

## Structural Validation Tests

`.claude/tests/test_ai_config.py` = automated enforcement layer for checklist above. Tests live alongside config they validate (not in `test/`, which for OCX binary acceptance tests). Run as part of `task verify` (via `claude:tests`) and catch:

- **Rule glob validity**: every `paths:` glob in scoped rules match at least one file on disk (dead glob detection)
- **CLAUDE.md consistency**: line budget, stated principle count match headings, stated worktree count match table rows
- **Cross-reference accuracy**: workflow filenames in rules exist on disk, artifact paths in skills use correct directories
- **Agent correctness**: tool/body consistency (no commands need tools not in frontmatter), completion protocol compliance
- **Hook safety**: no `set -e` in PostToolUse hooks, conditional log trim
- **Taskfile robustness**: empty file list guards for lint tasks

### When to extend the tests

| Trigger | Test to add |
|---------|------------|
| New skill added | Cover auto (orphan detection + path existence) |
| New scoped rule added | Cover auto (glob match validation) |
| New keyword trigger | Cover auto (overlap + noise detection) |
| New agent with tool restrictions | Add test verify body no use commands outside `tools:` |
| New convention in CLAUDE.md | Add test count/validate stated fact |
| New hook script | Add safety tests (no `set -e` for PostToolUse, exit code discipline) |
| New cross-reference pattern | Add test resolve reference to real file |

### Running the tests

```sh
task claude:tests                      # via task runner (used by task verify)
cd .claude/tests && uv run pytest -v   # directly
```

### Principle: test the contract, not the content

AI config tests validate **structural invariants** (paths exist, counts match, conventions hold). NOT validate semantic correctness (rule advice accurate? skill produce good output?). Semantic validation = job of `/meta-validate-context` skill, invoke on demand.

## Automated Staleness Detection

PostToolUse hook (`post-tool-use-tracker.sh`) fire on every `Edit|Write` and output AI config update reminders when infrastructure files change. Two reminder types:

1. **`context_reminder`** — source subsystem changes (e.g., edit `crates/ocx_lib/src/oci/`) → remind to update match `subsystem-*.md` rule
2. **`config_reminder`** — infrastructure changes (Taskfiles, Vue components, VitePress config, CI workflows, Cargo.toml, deny.toml, mirror configs) → list specific AI config files need update

**When add new infrastructure dependency** (new Taskfile, new component library, new CI tool), add corresponding `config_reminder` entry to `post-tool-use-tracker.sh` that map file pattern to all AI config files that reference it. Ensure future changes trigger update reminders auto.

## When to Update

| Trigger | Action |
|---------|--------|
| New subsystem created | Add `subsystem-{name}.md` scoped rule |
| Codebase pattern changed | Update affected subsystem rules |
| New tool integrated | Add to relevant skills, `quality-core.md`, and `config_reminder` in hook |
| Taskfile changed | Update CLAUDE.md, quality-core.md, relevant skill Task Runner sections |
| Vue component added/changed | Update `subsystem-website.md`, `docs-style.md`, documentation skill |
| CI workflow changed | Update `subsystem-ci.md` rule |
| Claude Code new release | Run `/meta-maintain-config research "Claude Code features"` |
| Directory renamed | Verify `paths:` globs in scoped rules still match |
| Config feels stale | Run `/meta-maintain-config refresh` |