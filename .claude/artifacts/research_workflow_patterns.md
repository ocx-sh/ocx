# Research: AI-Ready Development Workflow Patterns

**Date:** 2026-04-12
**Context:** Informing refactor of OCX workflow rules to cover bug fixes, refactoring, and improve workflow discovery.

## Bug Fix Workflows

Industry consensus: **reproduce → regression test → fix → verify**. The regression test comes *before* the fix — this prevents fixes that address symptoms instead of root causes.

- IBM Agentic RCA Workflow: sequential pipeline agent model (context seeding → semantic diff → root cause → fix candidate)
- AgentAssay: token-efficient regression testing for non-deterministic agents

Sources: [IBM TD Commons](https://www.tdcommons.org/cgi/viewcontent.cgi?article=9718&context=dpubs_series), [AgentAssay](https://arxiv.org/abs/2603.02601)

## Refactoring Workflows

Non-negotiable gate: **characterization tests before any transformation**. Never scope a refactor as "refactor this package" — always one transformation at a time. Martin Fowler's Two Hats Rule (already in `quality-core.md`) should be a hard-encoded rule, not a convention.

Sources: [Augment Code - Safe Legacy Refactoring](https://www.augmentcode.com/guides/safe-legacy-refactoring-ai-tools-vs-manual-analysis-in-2025)

## AI-Ready Planning Patterns

Martin Fowler's "Encoding Team Standards" article: move judgment from people's heads into versioned repository artifacts. Four-part instruction anatomy: role definition, context requirements, categorized standards, output format. Apply at generation, development, review, and CI stages.

Sources: [Fowler - Encoding Team Standards](https://martinfowler.com/articles/reduce-friction-ai/encoding-team-standards.html), [LLMx AI-Ready Codebase Guide](https://llmx.tech/blog/ai-ready-codebase-claude-cursor-integration-guide/)

## GitHub Issues as AI Context

CCPM pattern (Ran Aroussi): PRD → technical plan → decompose to issues with acceptance criteria → sync to GitHub → agents pick up. Critical: acceptance criteria in issue bodies become the agent's exit condition.

GitHub's Agentic Workflows (Feb 2026 technical preview): agents execute directly from natural-language issue descriptions.

Sources: [CCPM](https://aroussi.com/post/ccpm-claude-code-project-management), [GitHub Agentic Workflows](https://dev.to/aidevme/github-agentic-workflows-ai-agents-are-coming-for-your-repository-maintenance-tasks-and-thats-a-2dl5)
