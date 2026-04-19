---
name: research_ai_config_sota
description: 2025-2026 state of the art in AI coding-agent config design
type: research
audited_at: 2026-04-19
---

# SOTA AI Config Design Research

## TL;DR

Five findings are strong enough to be treated as convergent standards the OCX setup should double-down on: (1) **path-scoped rules beat global rules** — every leading config keeps per-session context tight by scoping rules to file-type globs, with the OCX `subsystem-*.md` pattern directly matching best practice; (2) **skills over CLAUDE.md for anything invocable** — the progressive disclosure model (description ~100 tokens at session start, body ~5K on invocation, supporting files on demand) is now the correct default for workflow instructions; (3) **subagents as the primary context-budget lever** — isolation of research/exploration in separate context windows returning summaries is validated by Anthropic, Trail of Bits, and the community as 40-60% context savings; (4) **hooks are the only deterministic enforcement layer** — advisory rules in CLAUDE.md are soft signals that degrade as context fills, hooks guarantee execution regardless; (5) **Agent Skills** (agentskills.io SKILL.md standard) is now cross-platform (Claude Code, OpenAI Codex, Cursor, Gemini CLI) — skills written to the base standard are portable assets. The OCX setup is architecturally ahead of industry norms but faces a CLAUDE.md bloat risk: the 200-line budget is the most commonly violated constraint, and violation causes instruction-following degradation the model cannot self-report.

## Anthropic Official Guidance

### Memory (CLAUDE.md + Auto Memory)

Source: https://code.claude.com/docs/en/memory (verified 2026-04-19)

**CLAUDE.md scoping levels** (precedence: managed > local > project > user):

| Scope | Location | Purpose |
|---|---|---|
| Managed policy | `/etc/claude-code/CLAUDE.md` (Linux) | IT-deployed org-wide, cannot be excluded |
| Project | `./CLAUDE.md` or `./.claude/CLAUDE.md` | Team-shared via git |
| User | `~/.claude/CLAUDE.md` | Personal cross-project preferences |
| Local | `./CLAUDE.local.md` | Personal per-project (gitignored) |

**Key constraints**: Target under 200 lines per file — longer files consume more context and reduce adherence. Block-level HTML comments (`<!-- -->`) are stripped before injection into context (useful for maintainer notes that cost zero tokens). Files in subdirectories load lazily (when Claude reads files in that directory), not at session start.

**Import syntax**: `@path/to/file` — files are expanded at launch. Max import depth: 5 hops. External imports require one-time approval dialog. Relative paths resolve from the importing file, not from cwd.

**Path-scoped rules** (`.claude/rules/`): YAML frontmatter with `paths:` glob controls when a rule loads. Rules without `paths:` load unconditionally. Rules load when Claude reads files matching the pattern — not on every tool use. Symlinks in `.claude/rules/` are supported (enables shared rule libraries across projects).

**Auto memory**: Enabled by default (requires v2.1.59+). Stored at `~/.claude/projects/<git-repo-derived-path>/memory/MEMORY.md`. First 200 lines or 25KB of `MEMORY.md` loaded at every session start. Topic files (e.g., `debugging.md`) loaded on demand. All worktrees of the same git repo share one auto memory directory. Machine-local, not synced. Configure `autoMemoryDirectory` in user/local settings to override.

**2026 notable addition**: `claudeMdExcludes` setting (array of glob patterns) allows skipping ancestor CLAUDE.md files in monorepos. Managed policy CLAUDE.md cannot be excluded.

**InstructionsLoaded hook**: Fires when instruction files are loaded — useful for debugging which `.claude/rules/` files actually activated for a session.

### Skills (SKILL.md)

Source: https://code.claude.com/docs/en/skills (verified 2026-04-19)

Skills follow the **Agent Skills open standard** (agentskills.io). Claude Code extends the standard with additional frontmatter fields.

**Core frontmatter** (all optional except where noted):

| Field | Notes |
|---|---|
| `name` | Directory name, lowercase+hyphens, max 64 chars |
| `description` | **Primary discovery signal.** "What it does + when to use it." Front-load keywords. 1,536-char cap per entry in the combined listing. |
| `when_to_use` | Additional trigger phrases; counts toward the 1,536-char cap |
| `disable-model-invocation: true` | Manual-only skills (commit, deploy) — removed from Claude's context entirely |
| `user-invocable: false` | Background knowledge skills — not shown in `/` menu |
| `allowed-tools` | Pre-approved tools while skill is active (space-separated) |
| `model` | Override model for this skill |
| `effort` | Override effort level (low/medium/high/xhigh/max) |
| `context: fork` | Run in isolated subagent (protects main context) |
| `paths` | Glob — skill auto-loads only when working with matching files |
| `hooks` | Lifecycle hooks scoped to this skill |

**Progressive disclosure**: skill `description` always in context (~100 tokens), full SKILL.md body loaded on invocation (<5,000 tokens), supporting files (`references/`, `scripts/`, `assets/`) loaded on demand. Keep SKILL.md under 500 lines; move reference material to supporting files.

**Skill body lifecycle**: Skill content enters conversation as a single message on invocation and persists for the session. After compaction, skills are re-attached within a 25,000-token shared budget (5,000 tokens per skill, most recent first). Old skills can be dropped entirely from the re-attach budget.

**Dynamic context injection**: `` !`command` `` syntax runs shell commands before skill content reaches Claude — output replaces the placeholder. Useful for injecting live data (git status, PR diffs, env vars) into skill prompts.

**Skill description budget**: Total budget for all skill descriptions scales at 1% of context window, with 8,000-character fallback. When budget is exhausted, descriptions are truncated — Claude loses the keywords it needs to invoke the skill automatically. Keep individual descriptions tight; `disable-model-invocation: true` removes a skill from the budget entirely.

**Scope precedence**: enterprise > personal (`~/.claude/skills/`) > project (`.claude/skills/`) > plugin. No category subdirectories within `.claude/skills/` — flat layout only; nesting silently breaks slash-command discovery.

**Bundled skills**: `/simplify`, `/batch`, `/debug`, `/loop`, `/claude-api` are included in every session without configuration.

### Subagents

Source: https://code.claude.com/docs/en/sub-agents (verified 2026-04-19)

Subagents run in isolated context windows. They cannot spawn other subagents (prevents nesting loops). They inherit parent permissions with optional additional restrictions.

**Built-in agents**:
- **Explore** (Haiku, read-only): codebase search/analysis; thoroughness levels quick/medium/very-thorough
- **Plan** (inherits model, read-only): research during plan mode
- **general-purpose** (inherits model, all tools): complex multi-step tasks

**Custom agent format** (`.claude/agents/<name>.md`):

```markdown
---
name: worker-name
description: When to delegate here
model: haiku|sonnet|opus
tools: [Read, Grep, Glob, Bash]
memory: user|project|none
---
System prompt content here
```

**Scope precedence**: managed > `--agents` CLI flag > `.claude/agents/` > `~/.claude/agents/` > plugin agents.

**Agent teams** (experimental, `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`): Multiple Claude sessions coordinated via shared task lists. Team lead assigns work; teammates each have their own context window and can communicate directly (unlike subagents which only report to main). Optimal team size: 3-5 agents; ~5-6 tasks per agent. Strong use cases: parallel research, new modules with clear interfaces, debugging with competing hypotheses.

**Persistent memory for subagents**: `memory: user` gives the agent a persistent memory directory at `~/.claude/agent-memory/`. Subagent accumulates insights across conversations independently.

### Hooks

Source: https://code.claude.com/docs/en/hooks (verified 2026-04-19)

**Four handler types**: `command` (shell script), `http` (POST to external endpoint), `prompt` (single-turn LLM evaluation), `agent` (LLM with tools for complex verification).

**Lifecycle events** (partial list): `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionRequest`, `PermissionDenied`, `InstructionsLoaded`, `SubagentStart`, `SubagentStop`, `FileChanged`, `CwdChanged`, `PreCompact`, `PostCompact`, `TaskCreated`, `TaskCompleted`.

**PreToolUse decision outcomes** via `hookSpecificOutput`: `allow`, `deny`, `ask`, `defer`. Can also provide `updatedInput` to rewrite the tool's input before execution.

**Exit code semantics**: 0 = success, 2 = blocking error (action prevented, stderr fed to Claude), other = non-blocking error.

**Configuration locations**: `~/.claude/settings.json` (user), `.claude/settings.json` (project, committed), `.claude/settings.local.json` (project-local, gitignored), managed policy, plugin `hooks/hooks.json`, skill/agent frontmatter.

**Security model**: `disableAllHooks: true` in settings disables globally. Enterprise: `allowManagedHooksOnly` to block user/project/plugin hooks while allowing force-enabled plugins. Hooks are "guardrails, not walls" — they can be bypassed if the user has sufficient permissions.

**Key design point**: Hooks are the **only deterministic enforcement layer** in Claude Code. CLAUDE.md instructions are advisory; the model may choose not to follow them when context is full. Hooks run regardless of context state.

### Settings & MCP

Source: https://code.claude.com/docs/en/settings (verified 2026-04-19)

**Four scopes** (precedence managed > user > project > local):
- `~/.claude/settings.json` (user)
- `.claude/settings.json` (project, committed)
- `.claude/settings.local.json` (project, gitignored)
- Managed policy (IT-deployed)

**Key settings**: `model`, `effortLevel`, `autoMemoryEnabled`, `autoMemoryDirectory`, `claudeMdExcludes`, `permissions.allow/deny`, `sandbox.enabled`, `enableAllProjectMcpServers`, `availableModels`.

**MCP configuration**: servers defined in settings JSON with `command`/`args` or `url` for remote servers. Scoped per project or user. Anthropic maintains an official MCP registry at `api.anthropic.com/mcp-registry`. The `ENABLE_TOOL_SEARCH=auto` (default) defers full tool schemas until needed (~120 tokens for names only at session start vs. full schemas).

### Claude Agent SDK

Source: https://code.claude.com/docs/en/agent-sdk/overview (verified 2026-04-19)

Renamed from "Claude Code SDK" in late 2025 to reflect it had become a general-purpose agent runtime. Python (`claude_agent_sdk`, v0.1.48+) and TypeScript (`@anthropic-ai/claude-agent-sdk`, v0.2.71+).

**Core pattern**: `query(prompt, options)` streaming generator. `ClaudeAgentOptions` accepts `allowedTools`, `permissionMode`, `hooks` (as callbacks), `agents` (as `AgentDefinition` dicts), `mcpServers`, `resume` (session ID), `settingSources`.

**SDK vs. CLI**: Same capabilities, different interface. CLI for interactive development; SDK for CI/CD pipelines, custom applications, production automation. Workflows translate directly.

**Filesystem config**: SDK loads `.claude/` configuration (skills, CLAUDE.md, subagents) from the working directory unless `settingSources` restricts it.

### Prompt Caching

Source: https://code.claude.com/docs/en/costs + https://platform.claude.com/docs/en/build-with-claude/prompt-caching (verified 2026-04-19)

**Cache economics**: Cache reads cost 0.1× input price (10× discount). Cache writes cost 1.25× regular input. Break-even: ~4 turns for a cached block to pay for itself.

**March 2026 TTL regression** (GitHub issue #46829): Anthropic silently changed the default cache TTL from 1 hour to 5 minutes sometime between Feb 27 and Mar 8, 2026. Impact: any pause longer than 5 minutes in a session causes the entire cached context to expire; next turn re-pays cache_write cost at 1.25× input instead of cache_read at 0.1×. Claude Code sessions with idle gaps are now significantly more expensive than during the 1-hour TTL period.

**January 2026 cache scope beta**: `prompt-caching-scope-2026-01-05` — adds a `scope` field to `cache_control` objects for workspace-level isolation (effective Feb 5, 2026). Caches now isolated per workspace within an organization.

**CLAUDE.md caching implication**: CLAUDE.md is loaded every session. Do not edit CLAUDE.md mid-session — it invalidates the prompt cache and forces a cache_write on the next turn. Keep CLAUDE.md stable during a working session.

**`--resume` session cache issue**: When resuming sessions via `--resume`, the prompt cache breaks silently — the API rebuilds tokens from scratch on every turn instead of reading cached tokens. This is a known issue as of April 2026.

## Cursor Rules (`.cursor/rules/`)

Source: https://cursor.com/docs/rules + community forum research (verified 2026-04-19)

**Evolution timeline**:
- 2023: `.cursorrules` (single flat Markdown file in project root)
- 2024: `.cursor/` folder with `index.mdc`
- 2025: Multi-file `.cursor/rules/*.mdc` architecture
- 2025 (v2.2+): Folder-based rules — `.cursor/rules/<name>/RULE.md` — for readability; `.mdc` files remain functional

**Current MDC frontmatter**:
```yaml
---
description: "What this rule does and when to use it"
alwaysApply: false
globs: ["src/**/*.ts"]
---
```

**Four rule types**:
1. **Always Apply** (`alwaysApply: true`): Every chat session, no condition
2. **Auto-Attached** (`globs: [...]`): Activated when working with matching files
3. **Agent-Requested** (description only, no globs): Agent decides relevance from description
4. **Manual** (neither): Invoked via `@rule-name` in chat

**Organization guidance**: Under 500 lines per file. Organize in folders (`frontend/components.md`). Version-control for team sharing. Use `@filename` references instead of copying content. "Start simple — add rules only when noticing repeated mistakes."

**Enterprise features**: Organization-wide rules from dashboard (Team/Enterprise plans) with enforcement options. Remote rules importable from GitHub repositories into `.cursor/rules/imported/`.

**AGENTS.md note**: Cursor also reads `AGENTS.md` in subdirectories for simpler per-directory instructions (alternative to `.cursor/rules/`).

**Key philosophical difference from Claude Code**: Cursor's agent-requested rules require the LLM to already know about the rule from its description in the index — there is no path-based lazy loading equivalent to Claude Code's `paths:` frontmatter. Auto-attached (globs) is the closest equivalent.

## Community Patterns

### Aider

Source: https://aider.chat/docs/usage/conventions.html + https://aider.chat/docs/config/aider_conf.html (verified 2026-04-19)

Aider uses a two-file config split:

1. **`.aider.conf.yml`** — machine/session configuration (model selection, API keys, editor settings, UI preferences). Hierarchical: home dir → git root → cwd. Later files take precedence.

2. **`CONVENTIONS.md`** — behavioral instructions in plain Markdown. Loaded via `--read CONVENTIONS.md` or `read: CONVENTIONS.md` in `.aider.conf.yml`. Marked read-only (Claude cannot modify it) and cached when prompt caching is enabled.

**Interesting structural quirk**: Aider recommends loading CONVENTIONS.md as a _read-only_ file rather than a system prompt or editable file. This prevents the model from attempting to "fix" the conventions file during editing sessions and leverages prompt caching for the stable content. Community maintains a shared conventions repository at `github.com/Aider-AI/conventions` — importable without copy-paste.

**Model-agnostic design**: CONVENTIONS.md syntax is intentionally plain Markdown with no tool-specific directives, making conventions portable across LLM providers.

### Continue.dev

Source: https://docs.continue.dev/customize/deep-dives/rules (verified 2026-04-19)

Rules live in `.continue/rules/` as `.md` files with YAML frontmatter:
```yaml
---
name: api-conventions
description: REST API design for our services
globs: "src/api/**/*.ts"
alwaysApply: false
---
```

Rules apply to Agent, Chat, and Edit modes but **not autocomplete**. They are joined with newlines to form the system message for each applicable mode.

**Precedence stack**: Hub assistant rules → referenced Hub rules → local workspace rules → global rules.

**Dual local/Hub system**: Continue.dev maintains a "Hub" for shared team rules alongside local workspace rules. Teams can manage shared standards on the Hub without requiring bidirectional sync — a deliberate design choice to decouple personal from shared config.

**Regex matching** (`regex:` field): rules can trigger based on file content patterns, not just file paths — useful for targeting rules to files that import specific libraries or use specific patterns.

### Windsurf (Cascade)

Source: https://docs.windsurf.com/windsurf/cascade/cascade + industry coverage (verified 2026-04-19)

Windsurf uses two behavioral customization mechanisms:

1. **Memories**: Automatically generated from conversation patterns. Cascade observes recurring preferences and creates structured rules without manual authoring. Users can add manual memories via simple chat ("always respond in French").

2. **Workflows**: Reusable Markdown rulebooks that can be invoked in Cascade. Functionally similar to Claude Code skills but tied to the Windsurf IDE lifecycle.

**Ownership note**: Cognition AI (the company behind Devin autonomous coding agent) acquired Windsurf in late 2025. Pricing revamped March 2026 to usage-based tiers.

**Key difference from Claude Code**: Windsurf's Memories are auto-generated (no manual authoring required), while Claude Code's auto memory is supplemental to human-authored CLAUDE.md. Windsurf treats memory as the primary customization path; Claude Code treats CLAUDE.md as primary with auto memory as additive.

## Token/Context Research (2025-2026)

### Prompt Caching Best Practice

Sources: https://www.claudecodecamp.com/p/how-prompt-caching-actually-works-in-claude-code + https://github.com/anthropics/claude-code/issues/46829 (verified 2026-04-19)

The March 2026 TTL regression (1h → 5min) has fundamentally changed the economics of long sessions with idle time. Practical guidance:

- **Work in contiguous bursts** — idle gaps >5 minutes now force a full cache_write re-pay on the next turn
- **Never edit CLAUDE.md mid-session** — invalidates the cache for the entire stable-prefix block
- **Stable prefixes first** — CLAUDE.md, rules, and tool schemas should be front-loaded in the context to maximize cache hit rate
- **Cache scope beta (Jan 2026)**: If using the API directly with workspace isolation, add the `scope: "workspace"` field to `cache_control` to ensure caches are isolated per workspace

The cache_write costs 1.25× vs. cache_read at 0.1× — a 12.5× differential. On a 50K-token context with 20 turns, the difference between always-hit vs. always-miss is approximately 9× in input costs.

### Context Rot / Lost-in-the-Middle

Sources: https://towardsdatascience.com/deep-dive-into-context-engineering-for-ai-agents/ + https://medium.com/@juanc.olamendy/context-engineering-the-invisible-discipline (verified 2026-04-19)

**Context rot**: LLM performance degrades as context fills, even within the technical token limit. The "effective context window" for high-quality performance is estimated at significantly less than the advertised maximum (~256K tokens for models with 1M+ advertised windows).

**Lost-in-the-middle** (empirically validated): Models pay disproportionate attention to content at the beginning and end of context. Critical instructions and essential context should be at these positions.

**Practical implication for CLAUDE.md**: Instructions buried in a long CLAUDE.md may receive less attention than instructions near the top or bottom. Keep CLAUDE.md short (under 200 lines) so every line is in the "high-attention" zone. The Anthropic docs confirm: "Bloated CLAUDE.md files cause Claude to ignore your actual instructions."

**Context rot mitigations**:
- **Context Offloading**: Move information to external system (MCP tools, files)
- **Context Reduction**: `/compact` with explicit focus instructions
- **Context Retrieval**: Load information dynamically via skills/subagents on demand
- **Context Isolation**: Subagents with separate context windows

### Skill Description Budget

Source: https://code.claude.com/docs/en/skills (verified 2026-04-19)

All skill descriptions are loaded at session start so Claude knows what's available. Total budget scales at **1% of context window** with an 8,000-character fallback. Each entry is capped at **1,536 characters** (combined `description` + `when_to_use`).

When the budget is exhausted, descriptions are truncated — Claude loses the keywords needed to auto-invoke skills. This is a silent failure: the skill exists but Claude cannot match it to user requests.

**Design implications for OCX**:
- `disable-model-invocation: true` on action skills (commit, deploy) removes them from the description budget entirely
- `user-invocable: false` on background knowledge skills keeps them in budget but not in the slash menu
- Front-load keywords in `description` — truncation cuts from the end
- Monitor total skill count: OCX currently has ~30 skills; budget pressure becomes significant above ~50

### Multi-Agent Coordination

Sources: https://towardsdatascience.com/why-your-multi-agent-system-is-failing-escaping-the-17x-error-trap (2025) + https://mikemason.ca/writing/ai-coding-agents-jan-2026/ (verified 2026-04-19)

**The "bag of agents" anti-pattern**: Dumping a shared transcript into every sub-agent creates the opposite of specialization — every agent reads everything and inherits everyone else's mistakes. Error amplification: in a 5-agent pipeline, a 10% per-step error rate compounds to a ~41% failure rate at the output.

**Principle**: "Share memory by communicating, don't communicate by sharing memory" (GoLang concurrency applied to agents). Agents should receive focused task briefs, not full conversation history.

**Validated patterns**:
- Hierarchical architectures (lead agent + specialists)
- Git-based memory systems for durable state
- Rigorous verification loops (stubs → tests → implementation, not implementation → tests)
- One agent per context window; summary-only communication between agents

**OCX relevance**: The OCX swarm model (worker-explorer → worker-researcher → worker-architect → worker-builder → worker-tester → worker-reviewer) correctly implements hierarchical isolation. Each worker gets a focused brief, not the full conversation history. This is aligned with the SOTA research.

## OSS Reference Configs Beyond Our Three

*(Excludes: gstack, superpowers, get-shit-done — covered separately)*

### 1. everything-claude-code (affaan-m)

GitHub: https://github.com/affaan-m/everything-claude-code

The largest public Claude Code harness: 183 skills, 48 agents, 34 rules, 20+ hook scripts. Originally Claude Code-only, evolved to cross-platform (Cursor, OpenCode, Codex).

**Distinctive features**:
- **Instinct-based continuous learning v2**: Hooks capture successful patterns with confidence scoring; high-confidence patterns extracted into skills automatically across sessions
- **NanoClaw v2 model routing**: Routes requests to cheap models (Haiku, DeepSeek, Gemini) based on task classification
- **AgentShield integration**: 102 OWASP-aligned security rules; scans CLAUDE.md, settings, hooks, MCP configs for secrets, permission issues, hook injection, MCP server risk
- **DRY adapter pattern**: Single hook script shared across Claude Code/Cursor/Codex via thin adapter layer — avoids maintaining duplicate hook implementations per tool
- **Strategic compaction**: Hooks suggest compaction breakpoints (not automatic) with explicit "preserve these context elements" guidance

**What OCX could borrow**: The DRY adapter pattern for cross-worktree hook sharing. The instinct-extraction concept (surfacing successful patterns for skill promotion) as a periodic config review trigger.

### 2. Trail of Bits claude-code-config

GitHub: https://github.com/trailofbits/claude-code-config

Security-firm opinionated defaults. Focus: "skills encode expertise, not procedures."

**Distinctive features**:
- **Skills over checklists**: Trail of Bits explicitly distinguishes skills that encode *how to think* (security reasoning patterns, vulnerability taxonomy) vs. command sequences. Skills bundle reference material + decision logic, not step lists
- **Layered security constraints**: OS-level sandbox (`/sandbox` with Seatbelt/bubblewrap) + deny rules for SSH keys / cloud credentials / package tokens + hooks as a pattern-blocking layer. Explicitly states "hooks are guardrails, not walls"
- **`/insights` continuous feedback loop**: Weekly session review surfaces patterns (repeated mistakes, workflows that work), then systematizes findings into CLAUDE.md rules, hooks, or skills. Treats config as a living system with a formal improvement cycle
- **CLAUDE.md as a hierarchical system**: Global defaults (language philosophy, hard metric limits) + project-specific layers. "Put stable context in CLAUDE.md, not the conversation"

**What OCX could borrow**: The `/insights` pattern — a periodic review skill that analyzes session history and proposes config improvements. The "skills encode expertise not procedures" framing as a skill design principle.

### 3. claude-code-showcase (ChrisWiles)

GitHub: https://github.com/ChrisWiles/claude-code-showcase

Full-stack config example demonstrating all Claude Code extension points in one coherent project.

**Distinctive features**:
- **`UserPromptSubmit` hook for skill suggestion**: Analyzes incoming prompt keywords, file paths, and intent to proactively suggest relevant skills before Claude responds. Example: detecting "test" in prompt → suggests testing-patterns skill. This is hook-based routing that reduces reliance on skill description matching alone
- **Ticket-integrated commands**: `/ticket <ID>` reads JIRA/Linear requirements, implements, and updates ticket status — MCP + slash command + workflow in one composable unit
- **PostToolUse auto-format chain**: Every file edit → auto-format → lint → test run, all via hooks. No manual discipline required

**What OCX could borrow**: The `UserPromptSubmit` skill-suggestion hook pattern as a way to compensate for skill description budget limitations.

### 4. danielrosehill Claude-Code-Projects-Index

GitHub: https://github.com/danielrosehill/Claude-Code-Projects-Index

Not a config library — a registry of 75+ "agent workspace" repositories.

**Distinctive pattern**: The "agent workspace model" — a Git repository structured as a self-contained environment for a non-development domain (sysadmin, legal research, health documentation, financial planning). Each workspace has: CLAUDE.md for agent instructions, slash commands for task automation, MCP server configurations, subagent definitions. Demonstrates that Claude Code's config system is general-purpose, not development-only.

**What OCX could borrow**: The insight that `.claude/` config is a reusable workspace pattern, not just a dev-tool customization layer. OCX's own config could be extracted as a distributable "OCX developer workspace" template.

### 5. Jeffallan/claude-skills

GitHub: https://github.com/Jeffallan/claude-skills

66 specialized skills (v0.3, December 2025) + 9 workflow commands.

**Distinctive features**:
- **Atlassian MCP integration**: Jira and Confluence operations as first-class slash commands; `epic-driven development` workflow that reads epics → generates stories → creates implementation plans from Jira directly
- **Domain-organized skill library**: Skills organized by domain (product, engineering, data, devops) with explicit cross-references between related skills
- **Versioned skill bundles**: Skills tagged by version; teams can pin to a bundle version rather than taking continuous updates

**What OCX could borrow**: The versioned skill bundle concept — OCX's `.claude/` config could tag a stable config version for reproducibility when onboarding new contributors.

## Convergent Patterns (Strong Signal)

| Pattern | Anthropic Official | Cursor | Aider | Continue.dev | Windsurf | Community Configs |
|---|---|---|---|---|---|---|
| Path/glob-scoped rules | `paths:` frontmatter | `globs:` frontmatter | — | `globs:` frontmatter | — | universal |
| Skills/commands for on-demand workflows | SKILL.md progressive disclosure | agent-requested rules | read-only conventions | Hub rules | Workflows | all major configs |
| Subagents as context isolation | core feature | — | — | — | — | all advanced configs |
| Hooks as deterministic enforcement | 4 handler types | — | — | — | — | Trail of Bits, everything-claude-code |
| Description-based skill discovery | `description` field | agent-requested rules | — | `description` field | — | universal |
| `<200 line` CLAUDE.md budget | explicit guidance | `<500 line` rule files | concise CONVENTIONS.md | short rules | — | all configs |
| Version-controlled config | committed `.claude/` | committed `.cursor/rules/` | committed CONVENTIONS.md | committed `.continue/rules/` | — | universal |
| Disable auto-invocation for side-effecting actions | `disable-model-invocation` | manual @rule | — | — | — | commit/deploy skills |
| Cross-platform SKILL.md standard | agentskills.io | adopts standard | — | — | — | Claude Code + Codex + Gemini CLI |

## Divergent Patterns (Design Axes for OCX)

| Axis | Side A | Side B | Who picks which |
|---|---|---|---|
| Memory primary mechanism | Auto-generated (Windsurf Memories) | Human-authored (CLAUDE.md first) | Anthropic = human-primary; Windsurf = auto-primary; OCX follows Anthropic |
| Skill invocation model | Manual-only (`disable-model-invocation: true`) for everything | LLM-auto for reference skills, manual for action skills | Trail of Bits = mostly manual; everything-claude-code = mostly auto; OCX uses hybrid (action skills = manual, knowledge skills = auto) |
| Rule scope philosophy | All rules always loaded (simpler) | Path-scoped rules (context-efficient) | Aider/Continue = always-loaded for CONVENTIONS.md; Claude Code/Cursor/OCX = path-scoped preferred |
| Config granularity | Few large files (Aider 1 CONVENTIONS.md) | Many small focused files (OCX ~50 rules + skills) | Depends on project complexity; OCX complexity warrants many small files |
| Workflow enforcement | Advisory (natural language rules) | Deterministic (hooks guarantee execution) | Claude Code docs: both; Trail of Bits: hooks preferred; OCX: both but hooks for non-negotiables |
| Cross-project skill sharing | Plugin/marketplace model | Per-project committed files | Anthropic: plugins as distribution; OCX: committed + may benefit from plugin extraction |
| Agent model selection | Uniform model across agents | Per-agent model routing (Haiku for search, Sonnet for impl, Opus for architecture) | everything-claude-code/OCX: tiered; most simpler configs: uniform |

## Implications for OCX

1. **CLAUDE.md discipline is the highest-ROI improvement axis.** The 200-line budget is the most commonly violated constraint across the community. OCX's CLAUDE.md imports many sub-files via `@` syntax — that's correct. The remaining risk is ensuring the root CLAUDE.md itself stays under budget after all imports resolve. The March 2026 cache TTL regression makes this even more critical: a bloated CLAUDE.md that's stable is cached, but a bloated CLAUDE.md that's frequently edited costs 1.25× per turn.

2. **The OCX subsystem-rule path-scoping pattern is validated as SOTA.** Every major tool (Cursor, Continue.dev, Claude Code itself) converges on path-scoped contextual loading. The OCX `subsystem-*.md` + `paths:` frontmatter approach is directly aligned with best practice. Keep it; resist the temptation to promote subsystem rules to globals.

3. **Skill description budget pressure will grow.** At ~30 skills, OCX is below the saturation point. As the config grows (new workflows, new research patterns), the 1% budget cap will start truncating descriptions silently. Preemptive strategy: `disable-model-invocation: true` on all action skills (already done for commit/deploy), `user-invocable: false` on pure background knowledge skills, and front-loading keywords in descriptions.

4. **The Agent Skills open standard (agentskills.io) is now cross-platform.** OCX's SKILL.md files should be audited for standard compliance — specifically that `name` and `description` are present (required fields). Claude Code-specific extensions (`context: fork`, `effort`, `model`, `hooks`) are safe to keep as they are ignored by other tools.

5. **`UserPromptSubmit` hook for skill routing is an underused pattern.** The claude-code-showcase approach — analyzing incoming prompt keywords to proactively surface relevant skills — complements description-based matching and doesn't consume description budget. Worth exploring as OCX's skill catalog grows.

6. **The `/insights` periodic review loop (Trail of Bits) should be formalized.** OCX's `meta-maintain-config` skill covers this use case. The Trail of Bits approach of explicitly scheduling weekly reviews and systematically converting findings into config improvements is worth adopting as a recurring process, not just a reactive one.

7. **Subagent context isolation is validated as the primary context budget lever.** Anthropic documentation, Trail of Bits, and the research literature all converge on 40-60% context savings from routing exploration/research to isolated subagents. OCX already does this (worker-explorer, worker-researcher). The evidence supports expanding this pattern to more workflows.

8. **Plugins are now a viable distribution mechanism for `.claude/` config.** With 9,000+ plugins and Anthropic's official marketplace, extracting OCX's `.claude/` config as a plugin (or referencing installable skills from the marketplace) becomes a real option for contributor onboarding. Currently, the entire `.claude/` directory is checked into the repo. A skills plugin would allow pinning to stable versions.

## Sources

| Source | Type | Date | What It Covers |
|---|---|---|---|
| https://code.claude.com/docs/en/memory | Official docs | 2026-04-19 | CLAUDE.md scoping, auto memory, path-scoped rules |
| https://code.claude.com/docs/en/skills | Official docs | 2026-04-19 | SKILL.md format, frontmatter, progressive disclosure, description budget |
| https://code.claude.com/docs/en/sub-agents | Official docs | 2026-04-19 | Subagent configuration, built-ins, agent teams |
| https://code.claude.com/docs/en/hooks | Official docs | 2026-04-19 | Hook types, lifecycle events, security model |
| https://code.claude.com/docs/en/settings | Official docs | 2026-04-19 | Settings scopes, MCP configuration |
| https://code.claude.com/docs/en/agent-sdk/overview | Official docs | 2026-04-19 | Agent SDK patterns, renamed from Claude Code SDK |
| https://code.claude.com/docs/en/best-practices | Official docs | 2026-04-19 | CLAUDE.md guidelines, context management patterns |
| https://agentskills.io/specification | Specification | 2026-04-19 | Agent Skills open standard SKILL.md format |
| https://cursor.com/docs/rules | Official docs | 2026-04-19 | MDC format, four rule types, v2.2 folder-based rules |
| https://github.com/anthropics/claude-code/issues/46829 | GitHub issue | 2026-03 | Cache TTL regression 1h→5min documentation |
| https://aider.chat/docs/usage/conventions.html | Official docs | 2026-04-19 | CONVENTIONS.md read-only caching pattern |
| https://docs.continue.dev/customize/deep-dives/rules | Official docs | 2026-04-19 | Continue.dev rule format and Hub system |
| https://docs.windsurf.com/windsurf/cascade/cascade | Official docs | 2026-04-19 | Cascade Memories and Workflows |
| https://github.com/affaan-m/everything-claude-code | GitHub repo | 2026-04-19 | Largest public Claude Code harness; instinct-based learning |
| https://github.com/trailofbits/claude-code-config | GitHub repo | 2026-04-19 | Security-firm opinionated config; skills-over-procedures |
| https://github.com/ChrisWiles/claude-code-showcase | GitHub repo | 2026-04-19 | UserPromptSubmit skill-routing hook; ticket integration |
| https://github.com/danielrosehill/Claude-Code-Projects-Index | GitHub repo | 2026-04-19 | Agent workspace model across non-dev domains |
| https://github.com/Jeffallan/claude-skills | GitHub repo | 2026-04-19 | Versioned skill bundles; Atlassian MCP integration |
| https://towardsdatascience.com/why-your-multi-agent-system-is-failing-escaping-the-17x-error-trap | Blog post | 2025 | 17x error amplification; bag-of-agents anti-pattern |
| https://medium.com/@juanc.olamendy/context-engineering-the-invisible-discipline | Blog post | 2025 | Context engineering taxonomy; context rot definition |
| https://www.claudecodecamp.com/p/how-prompt-caching-actually-works-in-claude-code | Blog post | 2026 | Prompt caching mechanics in Claude Code |
