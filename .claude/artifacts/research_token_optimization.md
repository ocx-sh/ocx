# Research: Token Optimization for Claude Code

**Date**: 2026-04-18
**Goal**: Drastically reduce Claude Code token consumption. Prefer transparent, "under the hood" solutions over manual discipline.

---

## TL;DR — Recommended Action Plan

Rank-ordered by effort-to-savings ratio, tuned for the OCX workflow (Rust + Taskfile + heavy CLI output):

| # | Action | Effort | Expected savings | Type |
|---|--------|--------|------------------|------|
| 1 | **Install [RTK](https://github.com/rtk-ai/rtk)** — transparent bash-output compressor | 5 min (`rtk init -g`) | 60–90 % on tool-call output; real-world report of 10 M tokens saved in one session | Tool, under-the-hood |
| 2 | **Audit + baseline with [ccusage](https://github.com/ryoppippi/ccusage)** | 5 min (`npx ccusage`) | 0 % direct, but required to see what the other actions are saving | Observability |
| 3 | **Tighten CLAUDE.md / MCP hygiene** (see §4) | 30 min | 10–40 % on cached-input cost (CLAUDE.md is paid every turn) | Configuration |
| 4 | **Default to Sonnet; route sub-work to Haiku subagents** | Config once, habit forever | 40–60 % on multi-part workflows; Haiku ≈ 15× cheaper than Opus | Configuration |
| 5 | **PreToolUse hook to strip `cargo test` / log noise** | 1 h once | 5–10× on test/log-heavy sessions | Hook, under-the-hood |
| 6 | **Lower `/effort` on routine work** | Per-session flag | 30 % on thinking tokens | Manual |
| 7 | **Evaluate [claude-code-router](https://github.com/musistudio/claude-code-router)** for cheap-model arbitrage | 1 day | Variable; only worthwhile if you run background/bulk tasks | Infrastructure |

Pairing #1 + #3 + #4 is the minimum viable configuration and typically yields a > 50 % drop in cost without any workflow change.

---

## 1. Where the tokens actually go

Before optimizing, understand the cost structure:

| Bucket | Paid when | Lever |
|---|---|---|
| **System prompt + tool schemas** | Every turn (cached after first hit) | MCP hygiene, `ENABLE_TOOL_SEARCH=auto` |
| **CLAUDE.md + auto-loaded rules** | Every turn (cached) | File size, path-scoping of rules |
| **Conversation history** | Every turn (rolling) | `/clear`, `/compact`, subagent isolation |
| **Tool-call output** (grep, cat, test runs) | Appended to history forever | RTK, PreToolUse hooks, `head_limit`, `offset`/`limit` |
| **Claude's output** (explanations, diffs, thinking) | Once as output, then forever as history input | `/effort`, concise style, diffs over rewrites |

Prompt caching gives a **10×** discount on cached input (0.1× the regular input price), but the 5-minute TTL (down from 1 hour as of 2026-03-06) means idle sessions lose the cache and re-pay the full price on the next turn. Implication: bursty, contiguous sessions are far cheaper than scattered work.

---

## 2. Claude Code built-ins (use these first — zero dependency)

### Model selection
- `/model sonnet|haiku|opus` — switch mid-session.
- `availableModels` in `settings.json` restricts choices to cost-conscious options.
- `opusplan` alias: Opus for planning, Sonnet for execution.
- Haiku ≈ 15× cheaper per token than Opus for equivalent mechanical output.
- **Fast mode** (`/fast`) is 2.5× faster but ~3× higher per-token cost — use sparingly.

### Context management
- `/clear` between unrelated tasks (use `/rename` + `/resume` to keep history reachable).
- `/compact [focus]` to summarize history; add a `# Compact instructions` block in CLAUDE.md to steer what survives.
- `/cost` to inspect spend; `/context` to see what's consuming context.

### Subagents as context firewalls
- Each subagent runs in its own isolated context window; only the summary returns.
- Set `model: haiku` in the subagent frontmatter for mechanical work.
- Verbose operations (log parsing, doc fetch, multi-file exploration) belong here.
- **Estimated savings**: 40–60 % on multi-part sessions.

### Extended thinking budget
- `/effort low|medium|high|xhigh|max` — thinking tokens are billed as output.
- `MAX_THINKING_TOKENS=8000` caps the budget globally.
- Routine tasks (formatting, renames, helpers) do not need `xhigh` thinking.

### Hooks (the truly "under-the-hood" lever)
- `PreToolUse` hooks run *before* tool execution and can rewrite input. Examples:
  - Force `Grep` output to `files_with_matches` for large queries.
  - Inject `head -100` onto `cat` / log reads.
  - Strip "PASS" lines from `cargo test`, keeping only failures.
- Savings compound because the hook prevents verbose output from ever entering context.

### Deferred tool schemas
- `ENABLE_TOOL_SEARCH=auto` (default) loads only tool *names* upfront (~120 tokens) and fetches full schemas on demand. Already active.

---

## 3. Third-party tools — catalog with pros/cons

### 3.1 CLI output compression (highest ROI for this workflow)

**[RTK (Rust Token Killer)](https://github.com/rtk-ai/rtk)**
- **What**: Transparent bash-proxy that intercepts command output before it reaches Claude Code. Filters duplicates, collapses progress bars, truncates log spam.
- **Integration**: `rtk init -g` installs a hook; zero config afterwards.
- **Savings**: Benchmarks — `cargo test` / `pytest` −90 %, git ops −75–92 %, `ls`/`tree` −80 %, `cat` −70 %. One community report: 10 M tokens saved (89 %) in a session.
- **Pros**: Single Rust binary, MIT, actively maintained (122 releases), no API key, no LLM calls. Matches OCX's Rust stack philosophy.
- **Cons**: Only affects tool-call output; trust a third-party binary in your shell; may hide output you actually need during debugging (togglable).

### 3.2 Prompt caching middleware

**Anthropic native prompt caching (already active)**
- Cache reads cost 0.1× input price. Hygiene is the whole game — see §4.

**[LiteLLM prompt cache routing](https://docs.litellm.ai/docs/tutorials/claude_code_prompt_cache_routing)**
- **What**: Proxy that pins requests to the same deployment so cache hits aren't missed across regions/accounts.
- **Pros**: Unified cost tracking across providers.
- **Cons**: Overkill for solo/small team; adds operational moving part.

**[flightlesstux/prompt-caching](https://github.com/flightlesstux/prompt-caching)**
- **What**: MCP plugin that automates `cache_control` breakpoints for *SDK-based apps*.
- **Cons**: Redundant for Claude Code end-users; Claude Code already handles its own caching.

### 3.3 Token counting / observability

**[ccusage](https://github.com/ryoppippi/ccusage)** — Parses `~/.claude/projects/` JSONL; daily/monthly/session cost breakdowns. MIT, zero config. Retrospective only.

**[CodeBurn](https://github.com/AgentSeal/codeburn)** — Goes a step beyond ccusage: reads the same JSONL transcripts, but also runs heuristic diagnosis (`codeburn optimize`) that flags specific waste patterns — files re-read across sessions, low Read:Edit ratio (retry-loop indicator), uncapped bash output, unused MCP servers, bloated `.claude/` configs. For each finding it suggests a copy-paste fix. Tracks a "one-shot rate" metric (edits succeeding without retry) as a prompt-quality signal. MIT, TypeScript + Swift menubar (macOS), Node 20+, `npm i -g codeburn`. ~2.7k stars, actively maintained.
- **Pros**: Zero integration (read-only on existing logs); multi-provider; goes from "what did I spend" to "why did I spend it and how to fix."
- **Cons**: *Diagnoses* but does not reduce tokens — it's advice, not infrastructure. No quantified before/after benchmarks. Heuristics may false-positive on patterns that earn their cost (large CLAUDE.md that lives in the cache). Node-only.
- **Where it fits**: Complements ccusage; use after RTK + hygiene fixes to find the remaining waste. Not a substitute for RTK, claude-code-router, or any in-flight tool — different layer of the stack.

**[Claude-Code-Usage-Monitor](https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor)** — Real-time terminal monitor with burn-rate predictions. Useful for 5-hour billing-window awareness.

**[tokencost](https://github.com/AgentOps-AI/tokencost)** — Python library, 400+ model pricing. For custom tooling only.

### 3.4 Model routing / proxying

**[claude-code-router](https://github.com/musistudio/claude-code-router)**
- **What**: Node proxy that routes Claude Code requests to different models by task type (Haiku for background, reasoning models for planning, DeepSeek/Gemini for cheap work). Auto-switches to a long-context model above ~60 K tokens.
- **Pros**: Dramatic cost arbitrage if you're willing to mix providers; `/model` command for dynamic switching.
- **Cons**: Adds latency + a moving part; requires multiple API keys; quality variance across providers.

### 3.5 Codebase packaging (mostly redundant with Claude Code)

**[Repomix](https://github.com/yamadashy/repomix)** — Packs a repo into one XML/MD file with tree-sitter compression (~70 % reduction when using signature-only mode). Useful for broad-context refactors; static snapshot.

**[code2prompt](https://github.com/mufeedvh/code2prompt)** — Handlebars-templated codebase-to-prompt CLI.

**[files-to-prompt](https://github.com/simonw/files-to-prompt)** — Minimal Python CLI; no compression.

**[aider repomap](https://aider.chat/docs/repomap.html)** — Graph-based (tree-sitter + PageRank) symbol ranking; achieves 4–6 % context utilization vs. full-file loading. Not directly consumable by Claude Code, but the pattern is worth watching as a future MCP opportunity.

### 3.6 Agent memory layers

**[Mem0](https://github.com/mem0ai/mem0)** — Persistent memory store (Apache 2.0, YC/$24 M Series A). Claims 90 % token cost reduction vs. full-context approaches on the LoCoMo benchmark.
- **Cons**: Requires custom Claude Code wrapper — no native hook. For short-session coding workflows, `/compact` + the built-in `auto memory` system already cover most of this.

**Zep** — Graph-based alternative; higher memory-store cost; contested benchmarks; same integration barrier.

**Claude Code's own `auto memory`** — Already active in this project (`~/.claude/projects/*/memory/MEMORY.md`). Use it; don't add a second system.

### 3.7 Semantic code search

**[Code Context MCP](https://www.pulsemcp.com/servers/code-context)** — Embedding-based "find the function that does X" search via MCP, reducing exploratory reads.
- **Pros**: Eliminates a whole class of full-file loads.
- **Cons**: Needs an embedding model / vector store dependency. Naive RAG over code is increasingly criticized — use only when grep falls short.

---

## 4. Configuration hygiene (free, compounding)

### CLAUDE.md
- Target **< 200 lines of essentials**. This repo's CLAUDE.md is currently larger than that — moving subsystem guidance into `.claude/rules/subsystem-*.md` (already done) and skills (partly done) is the right trajectory.
- Every line is paid on every cached turn. Aggressively prune.
- Include a `# Compact instructions` section so `/compact` preserves what matters.
- **Do not edit CLAUDE.md mid-session** — it invalidates the prompt cache and re-charges the whole system prompt on the next turn.

### Path-scoped rules
- Already well-applied in this repo (`subsystem-oci.md` only loads under `crates/ocx_lib/src/oci/**`, etc.). Keep it that way; do not promote subsystem rules to globals.

### MCP servers
- Run `/mcp` and disable any server not needed in the current project. Each server adds schema listing overhead even with deferred tool search.
- Prefer CLI tools (`gh`, `aws`, `gcloud`) over their MCP equivalents — CLI adds zero tool-listing cost.

### Skills
- Skills load on invocation, not at session start. Move single-task workflows out of CLAUDE.md into `.claude/skills/`.

### Settings (`settings.json`)
```json
{
  "model": "sonnet",
  "effortLevel": "medium",
  "enableAllProjectMcpServers": false
}
```

---

## 5. Workflow anti-patterns to stop doing

| Anti-pattern | Cost |
|---|---|
| Re-reading the same file multiple times | Full tokens each time |
| `Read` without `offset`/`limit` on large files | Entire file charged |
| `Grep` with `output_mode: content` when `files_with_matches` would do | 10–100× overhead |
| Running `cargo test` / `pytest` in the main conversation | All output enters permanent context (fixed by RTK or a hook) |
| Vague prompts ("clean up this module") | Triggers exploratory reads |
| Opus for routine formatting / renames | 5–15× premium over Sonnet/Haiku |
| Loading full third-party docs via WebFetch | Thousands of tokens, ~50 used |
| Extended thinking on trivial tasks | Tens of thousands of output tokens default |
| Agent teams left running when idle | ~7× token burn vs. single session |
| Mid-session CLAUDE.md edits | Invalidates prompt cache |

---

## 6. What NOT to bother with (for this project)

- **Mem0 / Zep** — Claude Code's built-in `auto memory` + `/compact` covers the same need without a custom integration.
- **Repomix / code2prompt** — Redundant with Claude Code's interactive file-loading; useful only for one-shot LLM prompts outside the CLI.
- **`flightlesstux/prompt-caching`** — SDK-layer; Claude Code already handles this.
- **`DISABLE_PROMPT_CACHING`** variants — almost always costs more, not less.

---

## 7. Sources

**Anthropic official**
- [Manage costs effectively](https://code.claude.com/docs/en/costs)
- [Model configuration](https://code.claude.com/docs/en/model-config.md)
- [Context window](https://code.claude.com/docs/en/context-window.md)
- [Sub-agents](https://code.claude.com/docs/en/sub-agents.md)
- [Hooks](https://code.claude.com/docs/en/hooks.md)
- [Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)

**Third-party tools**
- [RTK (Rust Token Killer)](https://github.com/rtk-ai/rtk)
- [ccusage](https://github.com/ryoppippi/ccusage)
- [CodeBurn](https://github.com/AgentSeal/codeburn)
- [Claude-Code-Usage-Monitor](https://github.com/Maciek-roboblog/Claude-Code-Usage-Monitor)
- [tokencost](https://github.com/AgentOps-AI/tokencost)
- [claude-code-router](https://github.com/musistudio/claude-code-router)
- [Repomix](https://github.com/yamadashy/repomix)
- [code2prompt](https://github.com/mufeedvh/code2prompt)
- [files-to-prompt](https://github.com/simonw/files-to-prompt)
- [aider repomap](https://aider.chat/docs/repomap.html)
- [Mem0](https://github.com/mem0ai/mem0) · [Paper](https://arxiv.org/abs/2504.19413)
- [LiteLLM prompt cache routing](https://docs.litellm.ai/docs/tutorials/claude_code_prompt_cache_routing)

**Commentary / benchmarks**
- [How Prompt Caching Actually Works in Claude Code — Claude Code Camp](https://www.claudecodecamp.com/p/how-prompt-caching-actually-works-in-claude-code)
- [Anthropic Cache TTL Downgrade — Pixels and Pulse](https://thepixelspulse.com/posts/anthropic-cache-ttl-downgrade-developer-costs/)
- [Claude Code Context Buffer: The 33K–45K Token Problem — claudefa.st](https://claudefa.st/blog/guide/mechanics/context-buffer-management)
- [Branch8 — Claude Code token optimization](https://branch8.com/posts/claude-code-token-limits-cost-optimization-apac-teams)
- [RTK 89 % savings report](https://github.com/Kilo-Org/kilocode/discussions/5848)
