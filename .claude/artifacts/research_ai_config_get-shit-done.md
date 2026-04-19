---
name: research_ai_config_get-shit-done
description: Deep study of the gsd-build/get-shit-done AI config repo
type: research
source_repo: https://github.com/gsd-build/get-shit-done
audited_at: 2026-04-19
---

# gsd-build AI Config Research

## TL;DR

GSD (v1.37.1) is a mature npm-distributed meta-prompting framework that installs 81 commands, 78 workflows, 33 agents, and 7+ hooks into `~/.claude/`. Its most valuable infrastructure patterns for OCX are: (1) a **two-hook context monitor** that injects advisory warnings into agent conversations when context drops below 35%/25%; (2) **role-based model profiles** (quality/balanced/budget) orthogonal to OCX's task-complexity tiers; (3) **"absence = enabled"** workflow flag semantics; (4) **agent skill injection** for project-specific knowledge without bloating worker definitions; and (5) **advisory-only prompt injection guards** including novel summarization-survival patterns. The full pipeline is far beyond OCX's scope, but these infrastructure patterns are extractable in under 400 lines of code total.

## Purpose & Positioning

GSD is installed globally via `npx get-shit-done-cc` and turns Claude Code into a complete project management system: requirements → research → planning → wave execution → verification → UAT. It targets users who want AI to autonomously execute entire features. OCX's `.claude/` is a single-project config for one development team, not a general-purpose framework. GSD's patterns are inspirations for OCX infrastructure, not migration targets.

## Inventory

| Category | Count | Notable files (with approx size) |
|---|---|---|
| Commands (`commands/gsd/`) | 81 | `execute-phase.md` (~2KB), `quick.md` (~5KB), `autonomous.md` (~1KB), `plan-phase.md` (~1.5KB) |
| Workflows (`get-shit-done/workflows/`) | 78 | Referenced only in depth-1 clone; each command points to `@~/.claude/get-shit-done/workflows/<name>.md` |
| Agents | 33 (21 with full docs) | Documented in `docs/AGENTS.md`; gsd-planner, gsd-executor, gsd-verifier, gsd-plan-checker are key |
| References (`get-shit-done/references/`) | 35 | `model-profiles.md`, `gates.md`, `context-budget.md`, `verification-patterns.md` |
| Hooks | 7 JS + 3 shell | `gsd-context-monitor.js` (193 lines), `gsd-statusline.js` (~300 lines), `gsd-prompt-guard.js` (97 lines), `gsd-workflow-guard.js` (94 lines), `gsd-read-guard.js` (82 lines), `gsd-read-injection-scanner.js` (~130 lines), `gsd-check-update.js` (~100 lines) + `gsd-phase-boundary.sh`, `gsd-validate-commit.sh`, `gsd-session-state.sh` |
| SDK (`sdk/src/`) | ~50 TS files | `context-engine.ts`, `context-truncation.ts`, `query/` registry with 30+ typed handlers |
| Tests | 150+ `.test.cjs` | Model profile tests, wave execution, parallel commit safety, hook opt-in, agent size budget |
| Docs | 9 English + 5 Japanese | `ARCHITECTURE.md` (~600 lines), `CONFIGURATION.md` (~550 lines), `AGENTS.md` (~500 lines) |
| Templates | ~15 | `state.md`, `phase-prompt.md`, `UAT.md`, `UI-SPEC.md`, `codebase/` mapping templates |

Total config size: ~500KB source. Agent size budget enforced by test: XL=1600 lines, LARGE=1000, DEFAULT=500. Agent distribution: min ~50 lines, median ~300 lines, max ~1600 lines (gsd-executor/gsd-planner).

## Architecture

### Canonical Source + Sync Model

There is no auto-sync mechanism. The npm package is the source. `bin/install.js` (~3000 lines) deploys to runtime-specific config directories at install time, performing runtime adaptation: tool name mapping, path replacement, frontmatter conversion for 10 runtimes (Claude Code, OpenCode, Kilo, Gemini CLI, Codex, Copilot, Antigravity, Trae, Cline, Augment). Commands reference workflows via absolute `@~/.claude/get-shit-done/workflows/*.md` paths. Updates require `npx get-shit-done-cc` re-run. Locally-modified files backed up to `gsd-local-patches/` before overwriting; `/gsd-reapply-patches` re-applies them post-update.

### Phase-Boundary Hook Protocol

There is no PostToolUse hook enforcing phase transitions. Boundary management is split three ways: (1) `gsd-phase-boundary.sh` (PostToolUse shell hook) detects phase-transition file writes and updates STATE.md — a state tracker, not a gate; (2) `gsd-workflow-guard.js` (PreToolUse, opt-in via `hooks.workflow_guard: true`) advises against out-of-workflow edits; (3) actual phase gates live in workflow `.md` files as orchestration logic (e.g., plan-phase blocks when RESEARCH.md has unresolved open questions via `gsd-sdk query` calls). All enforcement is advisory, never blocking.

### Wave-Based Parallel Execution

1. Thin orchestrator (~15% context budget) reads all `PLAN.md` files in the phase directory.
2. Analyzes `depends_on:` frontmatter to build a dependency DAG.
3. Plans with no unmet dependencies form Wave 1 — spawned as parallel `Task()` subagents.
4. Each subagent gets a **fresh 200K context window** (up to 1M for supported models): the specific PLAN.md, PROJECT.md, STATE.md, and CONTEXT.md. For 1M-class models: also prior wave SUMMARY.md files and REQUIREMENTS.md.
5. Parallel agents commit with `--no-verify` (avoids build lock contention; cargo lock fights in Rust specifically cited). Orchestrator runs `git hook run pre-commit` once after each wave.
6. `STATE.md` writes protected by `.lock` file with PID-based stale lock detection (10s timeout, spin-wait with jitter, atomic `O_EXCL` creation).
7. After all waves: `gsd-verifier` runs sequentially against all artifacts.

"Fresh context per wave" is achieved by Task() spawn — each subagent has zero conversation history. Context rot is eliminated architecturally, not by discipline.

### Context-Monitor Infrastructure

Two hooks communicate via a `/tmp/claude-ctx-{session_id}.json` bridge file:

The `statusLine` hook (`gsd-statusline.js`) fires on every statusLine event and writes `{ session_id, remaining_percentage, used_pct, timestamp }`. The `PostToolUse` hook (`gsd-context-monitor.js`) fires after every tool use, reads the bridge file, and injects `additionalContext` into the agent's conversation when thresholds are crossed:

- `remaining > 35%`: exit silently
- `remaining <= 35%` (WARNING): "Avoid starting new complex work. Inform the user so they can prepare to pause."
- `remaining <= 25%` (CRITICAL): "Do NOT start new complex work. Inform the user — context is nearly exhausted."

Debounce: 5 tool calls between repeated warnings at same severity. Severity escalation (WARNING → CRITICAL) bypasses debounce. On CRITICAL with active GSD project: fire-and-forget subprocess records session stop state to STATE.md for `/gsd-resume-work`. Safety: 10s stdin timeout, 60s stale metrics TTL, path-traversal guard on session ID, all in try/catch with silent fail.

### Model Profiles

Single `model_profile` config key resolved via `gsd-tools.cjs resolve-model <agent-name>` at spawn time. Returns model alias embedded in Task() spawn prompt.

| Agent Role | `quality` | `balanced` (default) | `budget` | `inherit` |
|---|---|---|---|---|
| gsd-planner | Opus | **Opus** | Sonnet | Session model |
| gsd-roadmapper | Opus | Sonnet | Sonnet | Session model |
| gsd-executor | Opus | **Sonnet** | Sonnet | Session model |
| gsd-phase-researcher | Opus | Sonnet | **Haiku** | Session model |
| gsd-codebase-mapper | Sonnet | **Haiku** | Haiku | Session model |
| gsd-verifier | Sonnet | Sonnet | **Haiku** | Session model |
| gsd-plan-checker | Sonnet | Sonnet | Haiku | Session model |

Philosophy: planning agents need Opus (reasoning critical); execution agents use Sonnet (plan provides the reasoning); research/verification agents use Haiku (pattern extraction). Per-agent overrides via `model_overrides: { "gsd-executor": "opus" }`. Non-Claude runtimes use `resolve_model_ids: "omit"` (runtime picks default).

GSD profiles are **role-based** (what kind of cognitive work). OCX's tiers are **task-complexity-based** (how hard is this task). These are orthogonal dimensions that can be combined.

## Notable Patterns (ranked by OCX adoption value)

### Pattern 1: Context Monitor (bridge file + PostToolUse injection) — 5/5

**What**: Two hooks share a `/tmp` bridge file. The statusLine hook captures `remaining_percentage` (only available to statusLine hooks). A PostToolUse hook reads this file after every tool use and injects advisory warnings into `additionalContext` below 35%/25% thresholds. Agents receive these warnings in their conversation and can respond accordingly.

**Where**: `hooks/gsd-context-monitor.js` (193 lines), `hooks/gsd-statusline.js` (~300 lines)

**How**: Bridge file written by statusLine hook, read by PostToolUse hook. Debounce (5 tool calls), severity escalation, stale-metrics guard (60s TTL), stdin timeout (10s), path-traversal sanitization on session ID, complete try/catch silent fail.

**OCX applicability**: Adopt directly. OCX already has a statusLine hook at `.claude/hooks/statusline.py`. Add a 150-line PostToolUse hook that reads the bridge file — no changes to existing hooks needed. The advisory language pattern ("Inform the user so they can...") is the correct approach (non-imperative). This is the highest-value pattern in the entire repo. Agents are currently context-blind.

---

### Pattern 2: Role-Based Model Profile Resolution — 5/5

**What**: A single `model_profile` config key (quality/balanced/budget/inherit) maps agent roles to model tiers. Planner gets Opus, executor gets Sonnet, mapper gets Haiku. Resolved at spawn time by a CLI utility. Per-agent overrides available for fine-tuning.

**Where**: `docs/CONFIGURATION.md` §Model Profiles, `get-shit-done/bin/lib/model-profiles.cjs`

**How**: Orchestrator calls `resolve-model <agent-name>` before each Task() spawn. Returns model alias embedded in the spawn prompt. `inherit` returns empty string, deferring to session model. The profile table is a simple 2D lookup (agent-name × profile → model-alias).

**OCX applicability**: Adapt. OCX's tier overlay (`tier=low/auto/high/max`) already dispatches models, but the agent role dimension is implicit. Formalizing role-to-model assignments (worker-architect=opus, worker-builder=sonnet, worker-reviewer=sonnet, worker-researcher=haiku for OCX-equivalent roles) in one documented place, with an `inherit` escape hatch, improves transparency and enables future per-agent tuning.

---

### Pattern 3: "Absence = Enabled" Workflow Flags — 4/5

**What**: All `workflow.*` flags in `config.json` default to `true` when the key is absent. A minimal `{}` config enables the full pipeline. Users disable standard features explicitly; they never need to "enable" defaults. New experimental capabilities use the inverse: `features.*` flags default to `false` when absent (opt-in required).

**Where**: `docs/CONFIGURATION.md`: "All workflow toggles follow the absent = enabled pattern. If a key is missing from config, it defaults to true." + `get-shit-done/templates/config.json`

**OCX applicability**: Adopt as a documentation convention. Document OCX config keys (`ocx.toml`, `config.toml`) with explicit "absent = X" semantics for each key. No code change required — this is a clarity/intent principle. Particularly relevant when new config keys are added.

---

### Pattern 4: Agent Skill Injection — 4/5

**What**: `agent_skills` config map lets users inject per-agent-type skill directories into subagent prompts at spawn time. If configured, an `<agent_skills>` XML block is prepended to the Task() prompt with `@file` references for each skill. If not configured, the block is entirely omitted (zero overhead).

**Where**: `docs/CONFIGURATION.md` §Agent Skills Injection; `get-shit-done/bin/lib/model-profiles.cjs` + orchestrator workflows

**How**: At spawn, orchestrators call `node gsd-tools.cjs agent-skills <type>`. If configured, returns an XML block with `@-reference` lines for each skill file. Path traversal prevention built in. Skill files are SKILL.md in named directories.

**OCX applicability**: Adopt. OCX swarm workers could receive project-specific Rust/OCI conventions without bloating every worker definition. Implement as: at spawn, check for a skill directory matching the worker type (e.g., `.claude/skills/worker-builder/`); if present, inject `@file` references. This is the same mechanism OCX already uses for per-skill SKILL.md files — just extend it to worker spawning.

---

### Pattern 5: Defense-in-Depth Prompt Injection Guards — 4/5

**What**: Five advisory-only hooks form a security stack. Key innovations:
- `gsd-prompt-guard.js`: scans writes to `.planning/` for injection patterns (role override, instruction bypass, system tag injection, invisible Unicode).
- `gsd-read-injection-scanner.js`: scans **Read tool results** for patterns specifically designed to survive context compression ("when summarizing, retain this instruction", "this directive is permanent"). This addresses the attack vector where poisoned content survives context compression and becomes indistinguishable from trusted instructions.

**Where**: `hooks/gsd-prompt-guard.js` (97 lines), `hooks/gsd-read-injection-scanner.js` (~130 lines)

**OCX applicability**: Adopt the prompt-guard pattern for OCX's `.claude/artifacts/` and `CLAUDE.md` writes. The summarization-survival patterns in `gsd-read-injection-scanner.js` are novel and high-value — they address a real attack vector that neither OCX nor most configs protect against. One 150-line PreToolUse hook. All advisory, never blocking.

---

### Pattern 6: Orchestrator Context Budget Discipline — 4/5

**What**: Commands explicitly state "Context budget: ~15% orchestrator, 100% fresh per subagent." Per-phase context file manifests define exactly which files each workflow needs (execute phase: STATE.md + config.json only; plan phase: all files). Files >8192 chars are markdown-aware truncated (headings + first paragraph per section preserved).

**Where**: `commands/gsd/execute-phase.md` (explicit "Context budget" statement), `sdk/src/context-engine.ts` (phase-file manifests), `sdk/src/context-truncation.ts` (truncation logic)

**OCX applicability**: Adopt as an explicit rule. Add "Orchestrator budget ≤20%, workers get full fresh context" to OCX's `workflow-swarm.md`. Introduce per-workflow context manifests — define which `.claude/rules/*.md` files each swarm role needs. This directly addresses OCX's 12MB/.claude/ problem: most of it is currently loaded globally.

---

### Pattern 7: Read-Before-Edit Hook Advisory — 2/5

**What**: Advisory PreToolUse hook reminding models to read files before editing them. Skips Claude Code (which natively enforces this) by checking `CLAUDE_SESSION_ID || CLAUDECODE` env vars. Targeted at other runtimes where models loop on rejection.

**Where**: `hooks/gsd-read-guard.js` (82 lines)

**OCX applicability**: Skip. OCX is Claude Code-only, and Claude Code natively enforces read-before-edit. The env-var detection pattern is worth knowing for potential future multi-runtime work.

---

### Pattern 8: Granularity-Calibrated Agent Output — 3/5

**What**: Agents calibrate output depth based on a granularity tier from config: `full_maturity` (3-5 items with maturity signals), `standard` (3-4 items), `minimal_decisive` (2 options with a decisive recommendation). This matches output verbosity to project needs.

**Where**: `docs/AGENTS.md` — gsd-assumptions-analyzer and gsd-advisor-researcher agent descriptions

**OCX applicability**: Adapt — wire OCX's tier (low/auto/high/max) to output verbosity in worker prompts. A `tier=low` review should produce 2-3 focused actionable findings, not a 20-item list. A `tier=max` review should be exhaustive. Simple to implement: add tier-based output calibration instructions to worker prompt templates.

## "Absence = Enabled" Philosophy

Concretely: a fresh GSD project with `{}` in `config.json` gets the full pipeline: domain research before planning, plan verification loop (max 3 iterations), post-execution verifier, Nyquist test validation, UI design contracts, node repair on failure, code review. All on by default.

To disable plan checking: `"workflow": { "plan_check": false }`. To disable the verifier: `"workflow": { "verifier": false }`.

New experimental features (`features.thinking_partner`, `features.global_learnings`, `intel.enabled`) use the **inverse** convention: absent = disabled, explicit opt-in required. This makes upgrades non-breaking — new features don't activate until the user enables them.

The two namespaces are explicitly documented and semantically distinct: `workflow.*` = absent-enabled (standard operations), `features.*` = absent-disabled (experimental).

## Defense-in-Depth Checkers

Four checker agents, all read-only (no Write/Edit tool permissions — they evaluate, they never modify):

1. **gsd-plan-checker** (max 3 iterations, feeds back to gsd-planner): 8-dimension plan verification — requirement coverage, atomicity, dependency ordering, file scope, verification commands, context fit, gap detection, Nyquist compliance.
2. **gsd-integration-checker**: Cross-phase integration verification during milestone audit. End-to-end flow analysis.
3. **gsd-verifier** (post-execution): Goal-backward analysis against phase goals, not task completion. Includes a **test quality audit** that catches: disabled/skipped tests on requirements, circular test patterns (system generating its own expected values), assertion strength (existence vs. value vs. behavioral), expected value provenance. Test quality blockers override an otherwise-passing verification.
4. **gsd-security-auditor**: Verifies declared threat mitigations from PLAN.md exist in implemented code. Does NOT scan blindly — verifies only what was declared. Produces `{phase}-SECURITY.md`.

The read-only tool restriction is the key design invariant: checkers evaluate, they do not fix. This prevents silent problem masking.

## Token/Context Discipline

GSD's context discipline is layered:

- **Per-phase file manifests** (`sdk/src/context-engine.ts`): each workflow type declares exactly which `.planning/` files it needs. Execute: STATE.md + config.json. Research: + ROADMAP.md + CONTEXT.md. Plan: all files. Verify: + REQUIREMENTS.md + PLAN/SUMMARY files.
- **Markdown-aware truncation** (`sdk/src/context-truncation.ts`): files >8192 chars are truncated preserving YAML frontmatter + headings + first paragraph per section. A `[collapsed N lines — see full file]` pointer replaces the rest.
- **Milestone extraction**: ROADMAP.md narrowed to current milestone only.
- **Adaptive enrichment for 1M-class models**: orchestrator checks `context_window` config; if >=500K, sends prior wave SUMMARY.md files + REQUIREMENTS.md to workers.
- **Agent size budgets**: test-enforced line limits enforced by `tests/agent-size-budget.test.cjs`. Overflow requires PR rationale.
- **Shared `@file` includes**: boilerplate blocks extracted from 5+ agent definitions to shared references — `mandatory-initial-read.md`, `project-skills-discovery.md`, `debugger-philosophy.md`. Single source of truth, lower per-dispatch footprint.
- **Command files as thin shells**: each command is 15-60 lines pointing at `@~/.claude/get-shit-done/workflows/<name>.md`. No context loading in commands.

OCX's 12MB / 30K lines of `.claude/` is the opposite of this architecture. Most OCX config loads globally per-session. GSD demonstrates the alternative: phase-specific manifests, on-demand loading, external reference files, size budgets.

## Anti-Patterns for OCX

1. **Full pipeline adoption**: 81 commands + 78 workflows + 33 agents is a complete product. OCX needs infrastructure patterns, not a project management framework.
2. **Multi-runtime abstraction**: GSD invests heavily in 10 runtimes. OCX is Claude Code-only. Any multi-runtime complexity would be pure overhead.
3. **Global npm install model**: GSD installs to `~/.claude/` globally. OCX's `.claude/` is per-project. The deployment models are incompatible.
4. **File-based project state as first-class output**: `.planning/` directories are user-facing artifacts GSD encourages committing to git. OCX's `.claude/artifacts/` are internal planning notes. Merging these hierarchies would confuse both purposes.
5. **Monolithic orchestrators**: GSD's thin-orchestrator model works because workflow logic is in separate referenced files. Combining workflow logic into command files would recreate the context-heavy anti-pattern GSD explicitly designed against.

## Direct Applicability Summary

| Pattern | OCX Fit | Estimated Payoff |
|---|---|---|
| Context monitor (bridge file + PostToolUse injection) | Adopt | High — agents currently context-blind; ~150-line hook fixes it |
| Role-based model profile table | Adapt — formalize existing tier assignments | Medium — makes implicit routing explicit, enables per-role overrides |
| Absence = enabled convention | Adopt as documentation principle | Low-medium — clarifies intent, no code change |
| Agent skill injection | Adopt — extend swarm worker spawn | Medium — project knowledge injection without worker bloat |
| Prompt injection guards (write + read-time) | Adopt prompt-guard + read-injection scanner | Medium — protects artifacts/ from injection at zero token cost |
| Orchestrator context budget discipline | Adopt as explicit swarm rule | Medium — prevents orchestrator bloat, formalizes existing pattern |
| Granularity-calibrated output | Adapt (tier → verbosity in worker prompts) | Low — simple improvement to worker output quality |
| Read-before-edit hook | Skip | None — Claude Code native enforcement |
| Full pipeline (discuss→plan→execute→verify→UAT) | Skip | N/A — OCX has equivalent swarm workflow |
| Multi-runtime abstraction | Skip | Negative — adds complexity for zero benefit |

## Sources

| File | What it covers |
|---|---|
| `docs/ARCHITECTURE.md` | System architecture, agent model, hook system, wave execution (600 lines) |
| `docs/AGENTS.md` | 21 agents with roles, tools, spawn patterns, permissions matrix (500 lines) |
| `docs/CONFIGURATION.md` | Full config schema, model profiles, workflow toggles, absence=enabled (550 lines) |
| `docs/context-monitor.md` | Context monitor architecture, thresholds, setup (116 lines) |
| `hooks/gsd-context-monitor.js` | Complete PostToolUse implementation with debounce + bridge file (193 lines) |
| `hooks/gsd-prompt-guard.js` | Injection guard with pattern list (97 lines) |
| `hooks/gsd-read-injection-scanner.js` | Read-time scanner with summarization-survival patterns (~130 lines) |
| `hooks/gsd-workflow-guard.js` | Workflow advisory guard (94 lines) |
| `hooks/gsd-read-guard.js` | Read-before-edit advisory (82 lines) |
| `sdk/src/context-engine.ts` | Per-phase context file manifests |
| `sdk/src/context-truncation.ts` | Markdown-aware truncation (8192 char default) |
| `CHANGELOG.md` | Feature trajectory through v1.37.1 (active development confirmed) |
| `https://github.com/gsd-build/get-shit-done` | Repository root |
