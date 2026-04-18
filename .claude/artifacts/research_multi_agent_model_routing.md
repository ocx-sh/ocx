# Multi-Agent Framework Model Routing Patterns

**Research date**: 2026-04-19
**Domain**: multi-agent, cli, devops
**Purpose**: Patterns-axis research for OCX tier→model correlation audit. Surveys how modern multi-agent frameworks handle cost-aware model routing.
**Expires**: 2026-10-19 (model landscape evolves fast — re-verify in 6 months)

## Framework-by-framework findings

### Aider

Two-model pipeline called **architect mode**. The architect model proposes a solution in natural language; a separate **editor model** converts the proposal into precise file-edit instructions. Designed because some LLMs reason well but struggle with edit-format compliance.

```bash
aider --architect --model o1-preview --editor-model claude-3-5-sonnet-20241022
# or in-session:
/chat-mode architect
```

Built-in defaults map main model → editor model automatically. Edit formats: `editor-diff` (fast), `editor-whole` (accurate, higher cost).

**Routing mechanism:** Static per-session mode selection. No dynamic promotion. User chooses the mode; no auto-escalation.

**Benchmarks:** R1 (architect) + Sonnet 4.5 (editor) = 64% polyglot at **14× less cost than o1 alone**. o1-preview + Sonnet in diff format = 82.7% (practical interactive best).

### Cline

VS Code extension with **Plan mode** (read-only, no file writes) and **Act mode** (execution). Default: same model both modes. Opt-in setting enables per-mode model assignment.

**Usage telemetry (7-day window, 2025):**
- Plan: Sonnet 4 = 42.6%, Gemini 2.5 Pro = 15.3%
- Act: Sonnet 4 = 46.6%
- Most popular cross-mode: **Opus 4.1 (Plan) → Sonnet 4 (Act)** = 25.3% of cross-mode usage

Practitioners treat planning as higher-reasoning (premium model) and execution as capable-but-cheaper. Same pattern OCX uses at tier=max.

**Routing mechanism:** Static user config, mode-triggered. No runtime complexity routing.

### Claude Code

Per-subagent model via YAML frontmatter `model:` field: `haiku | sonnet | opus | inherit | <full-id>`. Default `inherit`. `CLAUDE_CODE_SUBAGENT_MODEL` env var overrides.

| Subagent | Model | Rationale |
|---|---|---|
| Explore | Haiku | Fast, read-only codebase search |
| Plan | Inherit | Research during plan mode; full capability |
| General-purpose | Inherit | Complex multi-step tasks |
| statusline-setup | Sonnet | Moderate task complexity |
| Claude Code Guide | Haiku | Simple documentation lookups |

**Routing mechanism:** Description-based dispatch. Parent LLM evaluates task against each subagent's `description:` and delegates; model is then fixed by the subagent's definition. No runtime complexity scoring.

### CrewAI

Per-agent LLM assignment at instantiation, plus separate `function_calling_llm` for tool-invocation calls — enabling cheaper model for tool dispatch and premium for reasoning within a single agent.

```python
agent = Agent(
    llm="claude-opus-4-7",                # primary reasoning
    function_calling_llm="gpt-4o-mini",   # cheaper tool dispatch
)
crew = Crew(process=Process.hierarchical, manager_llm="claude-opus-4-7", agents=[...])
```

| Role | Recommended tier |
|---|---|
| Strategic / planning | Reasoning models (Opus, GPT-4o) |
| Content / writing | Creative models |
| Data processing | Efficient / fast |
| Tool-heavy | Function-calling optimized |

**80/20 heuristic:** Premium models for ~20% of agents handling ~80% of complex reasoning.

**Routing mechanism:** Per-agent static at initialization. `function_calling_llm` split is the one intra-agent model switch.

### AutoGen (Microsoft)

Routes through **agent type registration** — each type bound to one model client at deploy time. Using different models requires registering multiple types.

```python
await runtime.register("triage", lambda: AssistantAgent("triage", model_client=haiku))
await runtime.register("specialist", lambda: AssistantAgent("specialist", model_client=sonnet))
await runtime.register("architect", lambda: AssistantAgent("architect", model_client=opus))
```

**Swarm pattern:** Agents emit `HandoffMessage` to transfer control. A cheap triage agent routes to specialized agents with appropriate model tiers — cascade implemented entirely through agent behavior.

**Routing mechanism:** Structural at deploy time (type registration) + runtime routing via handoffs. No dynamic complexity scoring in base framework.

Note: Microsoft merged AutoGen with Semantic Kernel; GA expected Q1 2026 with production cost/latency threshold support.

### LangGraph

Routing as graph topology. Each node instantiates its own LLM client. **Conditional edges** inspect state and return a routing key selecting the next node (which may be a different model).

```python
def route_complexity(state) -> Literal["simple_node", "advanced_node"]:
    result = classifier_llm.invoke(f"complexity score: {state['query']}")
    return "advanced_node" if result.score > 0.7 else "simple_node"

builder.add_conditional_edges("router", route_complexity, {...})
```

Router node typically uses a cheap model or rule-based logic; edge selects appropriate model node.

**Routing mechanism:** Graph topology + conditional edges. Most composable pattern surveyed — routing logic is explicit code. Supports both static (per-node LLM object) and dynamic (router selects among model nodes) routing.

### OpenHands (formerly OpenDevin)

TOML routing with **two reserved LLM names**. A **judge model** evaluates task complexity at runtime.

```toml
[llm]
model = "claude-sonnet-4-6"

[llm.reasoning_model]       # reserved — used for complex tasks
model = "o1"

[llm.judge_model]           # reserved — evaluates routing decision
model = "claude-haiku-4-5"  # cheap judge recommended
```

**Judge criteria:** Scores task complexity per arxiv 2409.19924; considers trajectory (last 5–10 action/observation pairs). Cheaper models explicitly recommended for the judge phase itself.

**Routing mechanism:** Clearest example of **dynamic per-task routing at runtime** in the survey. Judge → threshold → reasoning vs. default model.

### Cursor / Windsurf

"Auto mode" — server-side routing with four documented signals:

1. **Query complexity** — syntax fix vs. architectural decision
2. **Context requirements** — large codebase → wider context-window model
3. **Current availability** — fallback on latency
4. **Performance patterns** — learns over time

**Routing outcome:** ~90% → efficient/cheap models; ~10% → premium escalation. Fast-pool / slow-pool quota for rate limiting separate from capability routing.

**Routing mechanism:** Fully opaque, server-side, not developer-configurable. Only surveyed system where routing is platform-owned.

## Routing mechanism taxonomy

| Mechanism | Description | Frameworks | OCX Today |
|---|---|---|---|
| **Per-agent fixed** | Model set at definition time; immutable at runtime | CrewAI, AutoGen, LangGraph nodes, Claude Code subagents | All workers except architect/builder |
| **Mode toggle** | User switches mode; model follows mode | Aider, Cline, Claude Code plan mode | Tier flag at CLI invocation |
| **Tier/scale escalation** | Discrete model set per tier; higher tier = more capable | OCX (architect + builder overlays) | architect + builder only |
| **Dynamic LLM-judge** | Cheap model scores complexity at runtime; routes to reasoning model | OpenHands, LangGraph router node | Not implemented |
| **Structural handoff** | Typed agent-to-agent transfer; receiving agent has different model | AutoGen swarm | Codex adversary pass (cross-family, not cost-based) |
| **Intra-agent split** | Reasoning call vs. tool-invocation call use different models | CrewAI `function_calling_llm` | Not implemented |
| **Platform-opaque** | Server-side routing, developer-invisible | Cursor auto mode | Not applicable |
| **Budget-aware cap** | Stop escalating once cost threshold hit | OpenHands (configurable), Cursor fast quota | Not implemented |

OCX sits between "per-agent fixed" and "tier escalation". Architect and builder scale with tier; all other workers are fixed regardless of tier.

## Signal inputs used across frameworks

| Signal | Used by | OCX computes it? | Propagates to workers? |
|---|---|---|---|
| Agent role / task category | All (role = task type) | Yes (worker type) | Yes — worker type selects model |
| User-defined tier / mode | Aider, Cline, OCX | Yes (tier=low/high/max) | Partial — architect/builder only |
| LLM-scored task complexity | OpenHands, LangGraph, Cursor | No | No |
| File count / diff size | Cursor, OCX classify | Partial (classify.md) | No |
| Subsystem count | OCX (≥2 → `--builder=opus`) | Yes | As overlay trigger |
| One-Way Door / reversibility | OCX (codex overlay) | Yes | As codex overlay only |
| Trajectory / history depth | OpenHands | No | No |
| Context window demand | Cursor, Claude Code (Explore vs General) | No | No |
| Task type within a phase | Claude Code (Explore=Haiku narrow; Plan=inherit synthesis) | No | No |
| Model availability / latency | Cursor | No | No |

**Key gap:** OCX computes file count and subsystem count in `classify.md` but uses these only for tier selection and `--builder=opus`. Not passed to individual workers as task metadata.

## Cost/quality heuristics

| Pattern | Evidence | Source |
|---|---|---|
| Architect/editor split | R1+Sonnet = 64% polyglot at 14× less cost than o1 | aider.chat/2025/01/24/r1-sonnet |
| Premium reasoning + cheap editor | o1+Sonnet diff = 82.7% interactive best | aider.chat/2024/09/26/architect |
| Frontier orchestrator + cheap subagents | 40–60% cost reduction without meaningful quality loss | mindstudio.ai |
| Cascade (start cheap, escalate) | Easy cases = 70–80% of volume handled by cheap model | mindstudio.ai |
| Cross-mode pairing | 25.3% of Cline users: Opus Plan → Sonnet Act | cline.bot |
| 80/20 allocation | 20% premium agents handle 80% of complex reasoning | docs.crewai.com/en/learn/llm-selection-guide |
| Multi-agent skepticism | Single agent matched multi-agent on 64% of tasks | towardsdatascience.com/the-multi-agent-trap |

**Role-specific synthesis:**
- **Exploration / read-only search**: Haiku — universal
- **Tool invocation / function calling**: Can be cheaper than reasoning model (CrewAI pattern)
- **Review / judgment**: Sonnet minimum — no framework uses Haiku for judgment
- **Planning / architecture**: Premium model (Opus, o1, Gemini 2.5 Pro) — consistent
- **Implementation**: Sonnet default; Opus for cross-subsystem or novel algorithm

## Anti-patterns

1. **Model thrash within iterative loops.** Switching per micro-task creates context reinit overhead and stylistic divergence. OCX already handles correctly (Codex passes one-shot after Claude loop converges).

2. **Haiku for judgment / review tasks.** Frameworks keep Haiku only for read-only exploration. Using for spec-compliance or quality review risks silent quality regression.

3. **Context-window mismatch on downgrade.** At tier=high/max, OCX worker prompts carry substantial context. Downgrading to Haiku (200k cap) without checking effective prompt size risks silent truncation.

4. **Tool-call accuracy gaps.** CrewAI's `function_calling_llm` split exists because tool-call accuracy varies by model. Workers making many structured tool calls (builder, tester) should stay on Sonnet minimum.

5. **Opaque routing degrades trust.** Developers cannot tell if optimization was legitimate efficiency gain or hidden quality regression. OCX's meta-plan gate announcement (printing resolved tier + overlays) is good practice.

6. **Routing overhead exceeding savings.** For small tier=low changes, routing coordination can exceed savings. Tier=low already minimizes worker count.

7. **Cascade without confidence thresholds.** Start-cheap-escalate-on-failure needs a clear failure signal. Without one, either escalates too eagerly or never escalates.

## Implications for OCX

1. **Reviewer and tester models should stay at Sonnet across all tiers.** No framework in this survey downgrades judgment/review workers to Haiku. Current OCX behavior is correct.

2. **worker-researcher is the best candidate for tier-based model variation.** At tier=low, researcher tasks are typically narrow factual lookups (Claude Code Explore=Haiku pattern); at tier=high/max they involve synthesis across multiple sources (Plan=inherit pattern). A `--researcher=haiku|sonnet` overlay at tier=low is supportable.

3. **The OpenHands judge pattern is the most transferable dynamic mechanism.** A cheap model scoring implementation complexity before spawning `worker-builder` is additive to the existing signal-based overlay system — would make `--builder=sonnet|opus` responsive to actual task difficulty, not just structural signals.

4. **The CrewAI `function_calling_llm` pattern suggests intra-worker model split.** Workers making many tool calls could use a cheaper model for tool dispatch and Sonnet for reasoning. Framework-level change, not config change.

5. **Existing tier→architect and tier→builder overlays match industry best practice.** Premium models for architecture (Aider architect, Cline Plan=Opus, OCX `--architect=opus`), capable-but-cheaper for execution (Aider editor=Sonnet, Cline Act=Sonnet, OCX builder=Sonnet default). Improvement surface is propagating more signals downstream, not redesigning tier structure.

## Sources

- Aider architect mode — aider.chat/docs/usage/modes.html
- Aider architect/editor benchmark — aider.chat/2024/09/26/architect.html
- R1+Sonnet SOTA — aider.chat/2025/01/24/r1-sonnet.html
- Cline plan/act usage patterns — cline.bot/blog/plan-act-model-usage-patterns-in-cline
- Cline plan/act docs — docs.cline.bot/core-workflows/plan-and-act
- Claude Code subagents — code.claude.com/docs/en/sub-agents
- CrewAI agents — docs.crewai.com/concepts/agents
- CrewAI LLM selection guide — docs.crewai.com/en/learn/llm-selection-guide
- AutoGen swarm — microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/swarm.html
- AutoGen mixture-of-agents — microsoft.github.io/autogen/stable/user-guide/core-user-guide/design-patterns/mixture-of-agents.html
- LangGraph model router — github.com/johnsosoka/langgraph-model-router
- OpenHands routing PR — github.com/All-Hands-AI/OpenHands/pull/6189
- OpenHands LLM docs — docs.all-hands.dev/usage/llms/llms
- Explainable routing (Topaz) — arxiv.org/abs/2604.03527
- Routing criteria paper — arxiv.org/abs/2409.19924
- Cursor auto mode signals — surajgaud.com/blog/auto-model-selection-cost-optimizer
- Multi-model cost patterns — mindstudio.ai/blog/ai-agent-token-cost-optimization-multi-model-routing
- The Multi-Agent Trap — towardsdatascience.com/the-multi-agent-trap
