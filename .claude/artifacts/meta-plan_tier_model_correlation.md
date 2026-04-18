# Meta-Plan: Tier ↔ LLM Model Correlation Audit

**Target (free text)**: "Correlation of tiers and the llm model to use. Double check how the llm model is selected and whether we could pass additional information to use cheaper models on smaller changes. This should correlate with the complexity of the task itself, ie architectural decisions are more likely to need better llms."

**Classification status**: low-confidence — scope straddles pure audit (research-only) vs. config changes (low/high tier). Meta-plan gate firing per `classify.md`.

## Classification

| Axis | Value | Rationale |
|---|---|---|
| **Tier** | `high` | Touches 3 swarm skills (`swarm-plan`, `swarm-execute`, `swarm-review`), 9 agent files, and the shared `workflow-swarm.md` tier/model tables. Multi-subsystem (AI config). Not breaking, but One-Way Door Medium — once tier→model defaults shift, downstream plan/execute runs adopt them. |
| **Reversibility** | Two-Way → One-Way Medium | Config edits are git-reversible, but pattern diffusion (new classifier signals, new overlay defaults) is harder to undo once documented. |
| **Overlays** | `--architect=sonnet`, `--research=3`, `--codex=off` | Architect needed (trade-off design), 3-axis research warranted (tech=model capabilities/pricing, patterns=how other frameworks route models, domain=OCX's current claims like "Sonnet 4.6 within 1.2pp of Opus"). Codex off — reversible config, cost > value for Two-Way-ish work. |
| **Confidence** | Low | User's phrasing ("double check") suggests they might want audit-only; but "could we pass additional information" implies implementation. Gate resolves this. |

## GitHub Context

No PR or issue referenced. Free-text prompt only. No GitHub fetch performed.

## Current-State Snapshot (already gathered)

Model assignments today (from `.claude/agents/*.md` frontmatter):

| Worker | Model | Notes |
|---|---|---|
| `worker-architect` | opus | Fixed |
| `worker-architecture-explorer` | sonnet | Fixed |
| `worker-builder` | sonnet | Opus override documented for "complex implementation" — orchestrator passes `model: opus` |
| `worker-doc-reviewer` | sonnet | Fixed |
| `worker-doc-writer` | sonnet | Fixed |
| `worker-explorer` | haiku | Fixed |
| `worker-researcher` | sonnet | Fixed |
| `worker-reviewer` | sonnet | Fixed; rationale: "Sonnet 4.6 within 1.2pp of Opus on SWE-bench at 5× lower cost" |
| `worker-tester` | sonnet | Fixed |

Tier-coupled model knobs today:

- `/swarm-plan` — `--architect=inline|sonnet|opus` overlay (the ONLY tier-coupled model selector in plan phase)
- `/swarm-execute` — `--builder=sonnet|opus` overlay (mandatory opus at tier=max)
- `/swarm-review` — no explicit model overlay today; reviewer is fixed sonnet

Key observation: **reviewer, tester, researcher, explorer models are tier-invariant.** Only architect and builder scale with tier. User's hypothesis ("cheaper for smaller changes") likely targets exactly this asymmetry — low-tier reviews could plausibly use haiku, low-tier research could use haiku for narrow lookups, etc.

## Workers I Would Launch

### Discover phase (parallel, ~2 workers)
1. **`worker-architecture-explorer`** — Map every place a model is selected across `.claude/agents/`, `.claude/skills/swarm-*/`, `workflow-swarm.md`. Produce a matrix of (worker × tier × current model × justification).
2. **`worker-explorer`** — Find every orchestrator prompt that passes `model: X` dynamically (e.g., builder opus overrides). Identify where additional task-signal information *could* be threaded through (e.g., Plan header → Execute → worker prompt).

### Research phase (parallel, 3 axes)
3. **`worker-researcher` (tech axis)** — Current Anthropic model capability + pricing matrix: Opus 4.7, Sonnet 4.6, Haiku 4.5. Focus on code tasks (SWE-bench), reasoning benchmarks, context-window efficiency. Look for public per-token pricing or relative cost ratios. Validate the existing "Sonnet within 1.2pp of Opus" claim against current data.
4. **`worker-researcher` (patterns axis)** — How do multi-agent frameworks handle cost-aware model routing? Survey CrewAI, AutoGen, LangGraph, Semantic Kernel, Aider, Cline. Specifically: tier-based routing, task-signal propagation, escalation mechanisms, dynamic model selection.
5. **`worker-researcher` (domain axis)** — What signals in OCX's own planning pipeline could inform cheaper-model selection? E.g., file count from `git diff --stat`, change complexity heuristics, subsystem scope, One-Way Door markers. Look for signals already computed by `classify.md` that aren't being propagated to downstream workers.

### Design phase (sequential after research)
6. **`worker-architect` (sonnet)** — Produce `adr_tier_model_correlation.md`: cost/quality trade-off table per worker, proposed model matrix keyed on (worker × tier × overlay signal), signal-propagation mechanism (plan header → execute prompt → worker frontmatter override), rollout plan.

### Review phase (parallel, 1-2 workers)
7. **`worker-reviewer` (spec-compliance)** — Plan consistency: do proposed defaults honor existing mandatory constraints (opus at tier=max for architect/builder)? Does the signal-propagation design fit the existing overlay grammar?
8. **(Optional) `worker-architect` (One-Way Door check)** — Only if the proposed changes shift tier defaults in ways that create new one-way pressure. Probably not needed.

**Cross-model Codex pass**: off (per overlay). Reversible config change.

**Max concurrent workers**: 5 (Discover + Research parallel = 5). Well under the 8-worker cap.

## Artifacts I Would Produce

1. `.claude/artifacts/research_model_capability_matrix.md` — tech-axis findings (Opus/Sonnet/Haiku 4.x capability + cost + use-case fit)
2. `.claude/artifacts/research_multi_agent_model_routing.md` — patterns-axis findings (how other frameworks do cost-aware routing)
3. `.claude/artifacts/adr_tier_model_correlation.md` — ADR: proposed tier×worker model matrix + signal-propagation mechanism
4. `.claude/artifacts/plan_tier_model_correlation.md` — Executable phase plan for `/swarm-execute` (which config files to edit, in what order, with test expectations)

## Estimated Cost

- **Parallel worker peak**: 5 (Discover 2 + Research 3 overlap)
- **Heaviest single call**: architect (sonnet, design phase) — one ADR-scale artifact
- **Codex presence**: none
- **Total worker-minutes**: ~30-45 (research is the long tail; other phases are fast grep/read work)

## Not Doing

- No implementation of the config changes — this plan produces the design/plan artifacts; `/swarm-execute` does the edits.
- No new GitHub issue or PR creation.
- No changes to `workflow-swarm.md` or agent frontmatter during planning — those are /swarm-execute's job once the plan is approved.
- No cross-model Codex review on the plan artifact (Two-Way Door; not warranted).

## Open Questions for Approval

1. **Scope intent** — audit-only (produce research + ADR, stop there) OR full plan (research + ADR + executable plan for /swarm-execute)?
2. **Research breadth** — 3 axes (full) or narrower (e.g., tech-only, skip framework-pattern survey)?
3. **Architect model** — sonnet sufficient, or escalate to opus given this shapes all future swarm runs?
4. **Agent default changes in scope?** — e.g., should `worker-reviewer` consider haiku at tier=low, or is that out of scope and we only touch the tier-coupled overlays?

## Next Step If Approved

Proceed to Phase 1 (Discover + Research) with the worker assignment above. Re-emerge with artifacts listed. If `/swarm-execute` is requested after, hand off `plan_tier_model_correlation.md`.
