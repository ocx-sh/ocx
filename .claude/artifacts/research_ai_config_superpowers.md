---
name: research_ai_config_superpowers
description: Deep study of the obra/superpowers AI config repo
type: research
source_repo: https://github.com/obra/superpowers
audited_at: 2026-04-19
---

# Superpowers AI Config Research

## TL;DR

Superpowers is a multi-host AI config plugin (14 skills, 0 hooks, 3 deprecated commands) built around one dispatch mechanic: the `using-superpowers` skill loads at session start via description-matching and mandates that the agent invoke a skill tool before any response. The core workflow enforces TDD as an iron law with subagent-per-task execution and two-stage review (spec compliance then code quality). The most distinctive finding is "Claude Search Optimization" (CSO): descriptions must state *only when to trigger*, never summarize the skill body — because a description that summarizes the workflow becomes a shortcut Claude follows instead of reading the full skill. Config is written in plain SKILL.md files with no templates; multi-host support is achieved via per-host INSTALL.md plus symlinks, not template generation.

## Purpose & Positioning

A complete software development methodology distributed as a zero-dependency plugin. Targets Claude Code, Codex, OpenCode, Cursor, Gemini, and Copilot CLI via a shared `skills/` directory with host-specific install docs. Not a project-level config — a global persona overlay for any codebase.

## Inventory

| Category | Count | Notable files (line counts) |
|---|---|---|
| Skills (SKILL.md) | 14 | `writing-skills` 655, `test-driven-development` 371, `systematic-debugging` 296, `subagent-driven-development` 277, `using-git-worktrees` 218, `receiving-code-review` 213, `finishing-a-development-branch` 200, `dispatching-parallel-agents` 182, `brainstorming` 164, `writing-plans` 152, `verification-before-completion` 139, `using-superpowers` 117, `requesting-code-review` 105, `executing-plans` 70 |
| Skill supporting files | ~25 | Prompt templates, reference .md, graphviz .dot, utility scripts — one per skill dir |
| Agents | 1 | `agents/code-reviewer.md` (49 ln) |
| Commands (deprecated) | 3 | `commands/write-plan.md`, `brainstorm.md`, `execute-plan.md` — redirect stubs only |
| Hooks | 0 | None. Zero PreToolUse / PostToolUse hooks. |
| Multi-host configs | 2 | `.codex/INSTALL.md`, `.opencode/INSTALL.md` |
| Root config | 2 | `CLAUDE.md` (86 ln — contributor PR guidelines, not AI config), `GEMINI.md` (2 ln — @-imports) |

**Total config size**: ~3,200 lines across 14 SKILL.md files (excluding supporting material).

**Skill size distribution** (SKILL.md bodies only):
- min: 70 lines (`executing-plans`)
- median: ~183 lines
- max: 655 lines (`writing-skills`)
- p90: ~300 lines

## Architecture

### Dispatch Model (skill-centric, commands deprecated)

Entry point is `using-superpowers/SKILL.md`. Its `description` fires on session start because it says `"Use when starting any conversation"`. It mandates: invoke a skill tool before every response, even clarifying questions. Commands predated skills and are now deprecated stubs redirecting to skill equivalents.

All dispatch runs through the Anthropic skill-discovery mechanism: metadata description → model decides to load → reads full SKILL.md on demand. The `@` syntax is deliberately avoided for cross-references because it force-loads files immediately. Cross-skill references use `superpowers:skill-name` strings, which trigger on-demand loading only when the agent invokes the skill tool.

### Session-Start Hook Protocol

There are no hooks. "Session-start context injection" is achieved entirely through the `using-superpowers` skill's description field. This is LLM-driven, not deterministic. No shell scripts, no Python hooks, no hook configs. The model reads the description, decides it applies to the current conversation, and loads the full skill body as its first action.

### Multi-Host Strategy (vs gstack templates)

No templates, no template generation. Each host gets its own INSTALL.md with host-specific symlink or plugin install instructions. The skill content is written once. Divergence risk is mitigated by keeping skill files host-agnostic — platform-specific tool names are isolated to `using-superpowers/references/copilot-tools.md`, `codex-tools.md`. `GEMINI.md` is 2 lines: `@./skills/using-superpowers/SKILL.md` and a tools reference.

## Notable Patterns (ranked by OCX adoption value)

### Pattern 1: CSO — Description as Trigger-Only — score 5/5

**What**: The skill `description` field should state *only* the triggering conditions — never summarize the workflow. If a description summarizes what the skill does, Claude follows the description as a shortcut and skips the full skill body.

**Where**: `writing-skills/SKILL.md` Section "Claude Search Optimization (CSO)"

**How**: Empirically tested. When `subagent-driven-development` description said "dispatches subagent per task with code review between tasks", Claude did ONE review. After changing to "Use when executing implementation plans with independent tasks in the current session", Claude read the flowchart and correctly performed two-stage review. Bad: any description containing workflow words ("dispatches", "generates", "runs"). Good: trigger-condition-only language starting with "Use when...".

**OCX applicability**: Direct. OCX skill descriptions likely violate this. Any description containing process words is a CSO violation. Audit all 20+ skill descriptions.

---

### Pattern 2: Red Flags + Rationalization Tables — score 5/5

**What**: Every discipline-enforcing skill includes a "Red Flags — STOP and Start Over" list of thoughts that precede violations, plus a rationalization table mapping excuse to reality.

**Where**: All discipline skills — `test-driven-development`, `systematic-debugging`, `verification-before-completion`, `using-superpowers`.

**How**: Based on Meincke et al. (2025): persuasion techniques doubled LLM compliance (33% → 72%). Authority framing, commitment/consistency anchors, explicit rationalization counters close loopholes. The `writing-skills` skill documents the full psychology including the seven persuasion principles.

**OCX applicability**: Direct. `workflow-bugfix.md` has phases and gates but no rationalization table. `quality-core.md` has anti-pattern tiers but no "red flags" framing. Adding 5-row rationalization tables to `workflow-bugfix.md`, `workflow-refactor.md`, and `quality-core.md` would improve phase-skipping compliance at low token cost.

---

### Pattern 3: No-Placeholder Discipline in Plans — score 5/5

**What**: Plan documents must contain complete code, exact file paths, exact commands with expected output. Named "plan failures": TBD, TODO, "add appropriate error handling", "similar to Task N", steps describing what without showing how.

**Where**: `writing-plans/SKILL.md` Section "No Placeholders"

**How**: Every task step follows TDD: write failing test → run → implement → run → commit. Steps are 2-5 minutes each. Plan header embeds `REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development`. Self-review after writing checks spec coverage, placeholder scan, and type consistency across tasks.

**OCX applicability**: Direct. OCX plan templates in `.claude/templates/artifacts/` should adopt this no-placeholder mandate. Especially important for swarm plans where worker-builder receives tasks without seeing the full plan context.

---

### Pattern 4: Subagent Context Isolation Protocol — score 4/5

**What**: Subagents must never inherit the controller's session context. Controller extracts all task text upfront, provides it verbatim in the dispatch prompt. Subagents are never told to "read the plan file" — the controller provides the full text.

**Where**: `subagent-driven-development/SKILL.md` + `implementer-prompt.md`

**How**: Implementer template includes: scene-setting paragraph, full task text (pasted), explicit invitation to ask questions before starting. Four report status codes: DONE, DONE_WITH_CONCERNS, NEEDS_CONTEXT, BLOCKED — each with prescribed controller response. "Bad work is worse than no work" — subagent escalation is explicitly encouraged.

**OCX applicability**: OCX workers use `context: fork` for isolation. The four-status-code report protocol and "never make subagent read plan file" constraint are not currently standardized in `workflow-swarm.md`. Adding these would improve orchestration clarity.

---

### Pattern 5: Two-Stage Review (spec compliance then code quality) — score 4/5

**What**: After each task, dispatch a spec-compliance reviewer before dispatching a code-quality reviewer. Order is strict — do not start quality review before spec compliance is confirmed.

**Where**: `subagent-driven-development/SKILL.md` + separate prompt templates per review type

**How**: Prevents code-quality approval of work that missed half the spec. A dedicated final code-reviewer subagent runs after all tasks complete as an integration pass. Separate subagent dispatches with separate prompt templates ensure focus.

**OCX applicability**: OCX's Review-Fix Loop runs spec and quality in a single reviewer pass. Separating into two sequential dispatches would improve spec coverage. Worth piloting at `tier=max`.

---

### Pattern 6: Iron Law Framing — score 4/5

**What**: Non-negotiable rules follow the format `NO [FORBIDDEN ACTION] WITHOUT [REQUIRED PRECONDITION]`, followed by "No exceptions" and an explicit list of forbidden workarounds by name.

**Where**: `test-driven-development/SKILL.md`, `systematic-debugging/SKILL.md`, `verification-before-completion/SKILL.md`, `writing-skills/SKILL.md`.

**How**: "Violating the letter of the rules IS violating the spirit of the rules" is stated explicitly to close the "I'm following the spirit" loophole. Each Iron Law is followed by specific prohibited workarounds named verbatim (e.g., "Don't keep it as 'reference'", "Don't 'adapt' it while writing tests").

**OCX applicability**: OCX uses similar imperative language inconsistently. Applying Iron Law framing to `workflow-bugfix.md` Phase 3 (regression test before fix is mandatory) and `workflow-refactor.md` Phase 1 (safety net before any change) would strengthen the most commonly skipped gates.

---

### Pattern 7: SUBAGENT-STOP Guard — score 4/5

**What**: `using-superpowers/SKILL.md` opens with `<SUBAGENT-STOP>`: "If you were dispatched as a subagent to execute a specific task, skip this skill." Prevents subagents from loading the orchestrator-only dispatch skill.

**Where**: `using-superpowers/SKILL.md` lines 6-8.

**How**: Custom XML-like tag respected by Claude Code. Paired with the skill's agent-role context to avoid meta-skill loading when the model is in execution mode.

**OCX applicability**: OCX has separate agent files for workers which implicitly scopes to implementation. However there is no guard against a worker loading `swarm-plan` or `swarm-review`. Adding `<SUBAGENT-STOP>` to orchestrator skills is a 3-line, low-cost guard.

---

### Pattern 8: Skill Type Classification (Technique / Pattern / Reference) — score 3/5

**What**: Three skill types requiring different testing approaches. Discipline-enforcing: pressure testing + rationalization tables. Technique: application + variation scenarios. Reference: retrieval + gap testing.

**Where**: `writing-skills/SKILL.md` Sections "Skill Types" and "Testing All Skill Types"

**How**: Shapes what kind of baseline testing is required before writing the skill. "Write skill before testing? Delete it. Start over." applies to all types equally; only the test format differs.

**OCX applicability**: Useful mental model when creating new OCX skills. No config change required — adopt as a writing guide.

---

### Pattern 9: Graphviz Flowcharts for Decision Points Only — score 2/5

**What**: Skills use inline `digraph` DOT syntax only for non-obvious decision flows, process loops, or A-vs-B decisions. Never for reference material (use tables), code examples (use code blocks), or linear steps (use numbered lists).

**Where**: Multiple SKILL.md files. Style guide in `writing-skills/graphviz-conventions.dot`. Rendering via `writing-skills/render-graphs.js`.

**OCX applicability**: Skip. Claude Code doesn't render DOT natively. Adopt the principle (flowcharts only for genuine decision points) without adopting the DOT format.

---

### Pattern 10: Skill TDD (pressure-test before deploying) — score 3/5

**What**: "NO SKILL WITHOUT A FAILING TEST FIRST." Run subagent pressure scenarios without the skill (RED), write the skill to address specific failures (GREEN), close loopholes (REFACTOR). Pressure types: time pressure, sunk cost, authority, exhaustion — combined for maximum stress.

**Where**: `writing-skills/SKILL.md` + `testing-skills-with-subagents.md` + `examples/CLAUDE_MD_TESTING.md`

**How**: A subagent is dispatched with a task that should trigger the skill behavior. Exact rationalizations used by the non-compliant agent are documented verbatim. Those rationalizations become the content of the rationalization table in the skill body.

**OCX applicability**: Aspirational. OCX does not currently pressure-test skills. Adopting the concept informally ("was this skill tested in a session before shipping?") would improve quality. Full subagent pressure testing is high-effort but worth doing for any new discipline-enforcing rules.

## The `writing-skills` Meta-Skill

The most elaborately documented skill (655 lines). Key prescriptions:

1. **TDD mapping**: Skill creation follows RED-GREEN-REFACTOR. Baseline scenario without skill (RED) → minimal skill addressing those failures (GREEN) → close loopholes (REFACTOR).
2. **CSO**: Description = trigger conditions only. Never summarize workflow. Describe the problem, not the solution.
3. **Token targets**: Getting-started skills `<150 words`, frequently-loaded `<200 words`, other `<500 words`. Verified with `wc -w`.
4. **No `@` links for cross-references**: `@skills/foo/SKILL.md` force-loads immediately. Use `superpowers:skill-name` for on-demand loading.
5. **Flat namespace**: All skills in one searchable namespace. Separate files only for heavy reference (100+ lines) or reusable tools.
6. **One excellent example**: Pick the most relevant language. Never implement in 5+ languages.
7. **Checklists become TodoWrite todos**: Invoke skill → convert checklist to TodoWrite tasks.
8. **Iron Law applies to skills too**: "If you didn't watch an agent fail without the skill, you don't know if the skill teaches the right thing."

**Divergence from Anthropic official guidance**: Anthropic's `anthropic-best-practices.md` (included as reference) recommends descriptions include both "what it does" and "when to use it". Superpowers' tested position: description = trigger conditions only. Including "what it does" creates shortcuts Claude follows instead of reading the body. Superpowers has eval evidence; Anthropic guidance does not address this failure mode.

## TDD Enforcement Mechanism

Not a hook. Not a path-scoped rule. Three reinforcing layers:

1. **Skill description**: `"Use when implementing any feature or bugfix, before writing implementation code"` — broad enough to trigger on nearly any coding task.
2. **`using-superpowers` mandate**: Agents must invoke skills before any task. TDD is invoked before implementation starts.
3. **Plan documents embed the requirement**: `writing-plans/SKILL.md` specifies every task step follows TDD. Plan headers include `REQUIRED SUB-SKILL: Use superpowers:test-driven-development`. Implementer prompt templates include TDD instructions.
4. **Red Flags list**: 13 named rationalizations with explicit counters. "Delete code" is the correct response to every listed excuse.

Entirely context-engineering. No hook-based enforcement.

## Token/Context Discipline

Evidence of deliberate discipline:

- Median SKILL.md: ~183 lines. Only 2 of 14 exceed 500 lines (`writing-skills` at 655, `test-driven-development` at 371).
- No CLAUDE.md bloat: repo-level `CLAUDE.md` is 86 lines of contributor guidelines, not AI config.
- Progressive disclosure mandated: reference material in separate files, not inline. `@` for within-skill references, `superpowers:name` for cross-skill (on-demand only).
- Zero global auto-load rules. Every skill is on-demand.
- Session-start cost: 14 skill descriptions (metadata only). Bodies load only when invoked.

**Key token insight**: The `writing-skills` skill documents that description-as-workflow-summary causes Claude to bypass the skill body entirely. A verbose description does NOT save tokens — it trades correctness for token savings. This is the most counterintuitive finding in the repo and directly contradicts naive token-optimization instincts.

## Anti-Patterns for OCX

1. **Hooks for behavioral enforcement**: Superpowers uses zero hooks. OCX has Python hooks for behavioral reminders that could instead be context-engineered at the rule/skill level with lower maintenance cost. Keep hooks for deterministic checks only (path validation, format guards).

2. **Description-as-workflow-summary**: Any OCX skill description containing "generates...", "runs...", "dispatches..." is a CSO violation turning the description into a shortcut. All 20+ OCX skill descriptions need audit.

3. **Monolithic SKILL.md without progressive disclosure**: OCX skills exceeding 500 lines should split into SKILL.md (overview + quick reference + links) + supporting reference files.

4. **No rationalization tables on workflow rules**: `workflow-bugfix.md`, `workflow-refactor.md` list phases and gates but don't name the rationalizations agents use to skip them. Adding 5-row rationalization tables to each would improve compliance at minimal token cost.

5. **No SUBAGENT-STOP guards on orchestrator skills**: Workers could accidentally load `swarm-plan` — add a 3-line guard.

## Direct Applicability Summary

| Pattern | OCX Fit | Estimated Payoff |
|---|---|---|
| CSO — description as trigger-only | Adopt immediately | High — fixes skill bypass for all 20+ descriptions |
| Red Flags + Rationalization Tables | Adopt for discipline rules | High — low token cost, high compliance payoff |
| No-Placeholder mandate in plans | Adopt in plan templates | High — directly applicable to plan.template.md |
| Subagent context isolation protocol | Adopt wording | Medium — add 4-status-code report protocol to workflow-swarm.md |
| Two-stage review (spec then quality) | Adapt for tier=max | Medium — adds cost, improves spec coverage |
| Iron Law framing | Adopt for non-negotiable rules | Medium — 2-3 lines per rule |
| SUBAGENT-STOP guard | Adopt in orchestrator skills | Low — 3-line addition to swarm-plan, swarm-review |
| Skill type classification | Adopt as writing guide | Low — mental model, no config change |
| Graphviz flowcharts | Skip | None — Claude Code doesn't render DOT |
| Skill TDD (pressure testing) | Aspirational | Low immediate, high long-term if adopted |

**Highest-value single action for OCX**: Audit all 20+ skill descriptions against the CSO criterion. Any description that summarizes workflow (contains process words like "dispatches", "generates", "runs", "iterates") should be replaced with trigger-condition-only language. 30-minute audit, structural correctness improvement affecting every skill invocation.
