# Research: Claude Code Ecosystem Tools & Knowledge Frameworks

**Date:** 2026-04-12
**Context:** Evaluating external tools, skills, and knowledge frameworks for potential adoption into OCX AI configuration.

## Claude Code Skills & Hooks

### obra/superpowers (Jesse Vincent)

Multi-platform skill plugin (Claude Code, Cursor, Codex, etc.) with 12 composable skills. No PreToolUse/PostToolUse hooks — all enforcement through natural-language skill prompts injected via a single SessionStart hook.

**Worth stealing:**
1. **Ordered two-stage review** (`subagent-driven-development`): Spec compliance reviewed *before* code quality — explicitly prohibited from being reversed. Prevents quality polish on wrong-spec code.
2. **Anti-hedging language policing** (`verification-before-completion`): Bans "should", "probably", "seems to", "Done!" before verification evidence. Treats optimistic language as epistemic dishonesty, not style.
3. **Spec-first hard gate** (`brainstorming`): Writes spec to dated path, self-reviews for placeholders/contradictions, then waits for human approval before any implementation skill invokes.

**Already covered by OCX:** worktree management, branch finalization, plan execution, code review lifecycle.

Source: https://github.com/obra/superpowers

### claude-mem (thedotmack)

SQLite + Chroma vector DB memory system. Captures tool-usage observations automatically via 5 lifecycle hooks. Semantic search across past sessions. Progressive disclosure (broad search first, detail on demand).

**Interesting:** Complements built-in auto-memory (editorial decisions) with interaction history ("what did I do last week"). `<private>` tag for excluding sensitive content.

**Not needed yet:** OCX's structured .claude/ directory + auto-memory handles current scale. Worth revisiting if cross-session continuity becomes a bottleneck.

Source: https://github.com/thedotmack/claude-mem

### awesome-claude-code (hesreallyhim)

Community catalog. Three standouts:
- **Dippy**: AST-based bash command auto-approval with safety discrimination (more principled than blanket allow-lists)
- **claude-devtools / ccflare**: Session observability (compaction visualization, subagent trees, context usage)
- **TDD-enforced hooks**: Block commits violating TDD principles

**Trend:** Community converging on hook-driven safety gates and formalized agentic playbooks.

Source: https://github.com/hesreallyhim/awesome-claude-code

### ui-ux-pro-max-skill (nextlevelbuilder)

High stars (63k), mediocre prompt engineering. One genuinely interesting pattern: **data externalization** — design knowledge in CSV databases queried via BM25 search at invocation time, not embedded in prompts. Keeps skills lean, reference data versioned separately.

**Transferable:** The externalization pattern for skills with large reference corpora (e.g., OCI spec rules, platform quirks as queryable data rather than inline prompt text).

Source: https://github.com/nextlevelbuilder/ui-ux-pro-max-skill

## Knowledge Frameworks

### LightRAG (HKUDS)

Dual-mode RAG: semantic similarity + knowledge graph traversal. Benchmarks show gains on comprehensiveness (67.6% vs 32.4% vs naive RAG).

**Verdict: Not relevant for OCX.** The `.claude/rules/` system already provides structured, human-curated context with path-glob delivery. LightRAG solves unstructured corpus comprehension — OCX's knowledge base is already structured. High integration complexity (separate service, no native MCP).

Source: https://github.com/hkuds/lightrag

### NotebookLM (Google)

Source-grounded Q&A, audio overviews, multi-document synthesis. Enterprise API in alpha.

**Verdict: No value for OCX.** Human-facing reading tool, not machine-readable context. The gap it fills (understand 20 documents quickly) is not the bottleneck.

### Obsidian

Markdown knowledge base with graph view and backlinking. MCP bridges exist (obsidian-claude-code-mcp, mcp-obsidian).

**Verdict: Low value for OCX specifically.** `.claude/` already provides structured, cross-referenced context. Potentially useful as a cross-project knowledge base if the per-project model outgrows itself.

## Recommendation Summary

| Tool | Verdict | Action |
|------|---------|--------|
| obra/superpowers | **Cherry-pick patterns** | Adopt ordered review + anti-hedging language policing + spec-first gate |
| claude-mem | **Watch** | Revisit if cross-session memory becomes a bottleneck |
| Dippy (from awesome list) | **Investigate** | AST-based approval could replace blanket bash allow-lists |
| ui-ux-pro-max data externalization | **Extract pattern** | Apply to skills with large reference corpora |
| LightRAG | **Skip** | Solves wrong problem for a structured codebase |
| NotebookLM | **Skip** | Human-facing tool, not AI context |
| Obsidian | **Skip for now** | Revisit for cross-project knowledge if needed |
