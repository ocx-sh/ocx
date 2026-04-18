# Model Capability + Pricing Matrix (as of 2026-04-19)

**Research date**: 2026-04-19
**Purpose**: Tech-axis research for tier→model correlation audit in the OCX swarm system. Validates or refutes claims in `workflow-swarm.md`; informs architect's model-routing redesign.

## Quick-look table

| Model | Input $/1M | Output $/1M | Cache write (5m/1h) $/1M | Cache read $/1M | Context | Max output | Speed |
|---|---|---|---|---|---|---|---|
| **Opus 4.7** | $5.00 | $25.00 | $6.25 / $10.00 | $0.50 | 1M tokens | 128k | Moderate |
| **Opus 4.6** | $5.00 | $25.00 | $6.25 / $10.00 | $0.50 | 1M tokens | 128k | Moderate |
| **Sonnet 4.6** | $3.00 | $15.00 | $3.75 / $6.00 | $0.30 | 1M tokens | 64k | Fast (~55 tok/s, 0.73s TTFT) |
| **Sonnet 4.5** | $3.00 | $15.00 | $3.75 / $6.00 | $0.30 | 200k tokens | 64k | Fast |
| **Haiku 4.5** | $1.00 | $5.00 | $1.25 / $2.00 | $0.10 | 200k tokens | 64k | Fastest (4-5× Sonnet) |

Batch API (50% discount): Opus 4.7/4.6 = $2.50/$12.50; Sonnet 4.6 = $1.50/$7.50; Haiku 4.5 = $0.50/$2.50 per MTok.

Source: Anthropic pricing page and models overview (accessed 2026-04-19).

## Cost ratios

**Input token ratios (standard):**
- Opus 4.7 : Sonnet 4.6 : Haiku 4.5 = **5 : 3 : 1** (absolute $/MTok)
- Opus vs Haiku: **5×**; Sonnet vs Haiku: **3×**; Opus vs Sonnet: **1.67×**
- Note: `workflow-swarm.md` says "Sonnet at 5× lower cost than Opus" — this is **wrong**. Opus is 1.67× Sonnet. The 5× ratio is Opus vs Haiku.

**Generation shift 4.6 → 4.7 (Opus):**
- List price: unchanged ($5/$25 MTok)
- Effective cost: up **0–35%** due to new tokenizer producing more tokens for the same text (worst case on code + structured data — OCX's primary workload). Anthropic acknowledges this explicitly in pricing docs.
- Practical estimate: coding-agent workloads likely see +20–35% per-request cost vs Opus 4.6.

**Batch API cross-tier opportunity:**
- Batch Opus 4.7 input ($2.50/MTok) < Standard Sonnet 4.6 input ($3.00/MTok)
- For offline / non-latency-sensitive workers, batch Opus delivers higher capability at lower unit cost than real-time Sonnet.

## Capability benchmarks

### SWE-bench (software engineering — primary signal for coding workers)

| Model | SWE-bench Verified | SWE-bench Pro |
|---|---|---|
| Opus 4.7 | **87.6%** | 64.3% |
| Opus 4.6 | 80.8% | 53.4% |
| Sonnet 4.6 | 79.6% | ~48% est. |
| Haiku 4.5 | 73.3% | 39.5% |

**Delta analysis:**
- Opus 4.7 vs Sonnet 4.6: **8.0pp gap** — the "within 1.2pp" claim in `workflow-swarm.md` is stale.
- Opus 4.6 vs Sonnet 4.6: 1.2pp — the original claim was accurate for that generation only.
- Sonnet 4.6 vs Haiku 4.5: 6.3pp — Haiku is competitive with Sonnet 4.0 but behind 4.6.

### Reasoning benchmarks

| Model | GPQA Diamond | ARC-AGI-2 | Notes |
|---|---|---|---|
| Opus 4.7 | **94.2%** | — | Adaptive thinking; Jan 2026 knowledge |
| Opus 4.6 | 91.3% | **68.8%** | Extended thinking |
| Sonnet 4.6 | — | 58.3% | Extended + adaptive thinking |

- ARC-AGI-2 Opus 4.6 vs Sonnet 4.6: **10.5pp gap** — largest measured Opus premium; novel / distribution-shifted reasoning.
- GPQA: 94.2% (Opus 4.7) vs 91.3% (Opus 4.6) — 2.9pp generational gain.

### Code review — single-pass reasoning (Qodo study, 400 real PRs)

- Haiku 4.5 (thinking mode) wins **58%** of comparisons vs Sonnet 4.5 (thinking mode); quality 7.29 vs 6.60.
- Haiku 4.5 standard mode: 6.55 vs Sonnet 4.0: 6.20 — Haiku has caught up to prior-gen Sonnet.
- Caveat: single-pass only; excludes tool-calling, multi-step agentic behavior, code execution.

## Where Opus earns its premium (largest benchmark gaps)

1. **SWE-bench Verified (Opus 4.7 vs Sonnet 4.6)**: 8.0pp — multi-step agentic coding chains
2. **ARC-AGI-2 (Opus 4.6 vs Sonnet 4.6)**: 10.5pp — novel reasoning, distribution shift
3. **GPQA Diamond**: Opus 4.7 leads at 94.2% — PhD-level reasoning
4. **Terminal-Bench 2.0**: Opus 4.7 scores 69.4% — agentic CLI tasks (no Sonnet comparison published yet)
5. **Multi-turn agentic chains**: Anthropic's primary Opus 4.7 positioning — gap is most pronounced in long tool-use sequences, not single-pass generation.

## Context window + speed

| Model | Context | TTFT | Output speed | Notes |
|---|---|---|---|---|
| Opus 4.7 | 1M tokens | Moderate | Not published | New tokenizer; 300k output via Batch API beta |
| Opus 4.6 | 1M tokens | Moderate | Not published | Fast mode: $30/$150/MTok (6× premium) |
| Sonnet 4.6 | 1M tokens | 0.73s | ~55 tok/s | 300k output via Batch API beta |
| Haiku 4.5 | **200k tokens** | Fastest | 4–5× Sonnet | **Context cap is a hard blocker** for codebase-wide tasks |

Critical: Haiku 4.5's 200k context limit is a hard constraint for workers needing to load full codebases, long web pages, or multiple large files simultaneously. Sonnet 4.6 and Opus 4.7 both have 1M windows.

## Use-case fit (Anthropic's published guidance)

| Task type | Recommended model | Rationale |
|---|---|---|
| Complex agentic coding, multi-step tool use | Opus 4.7 | "Step-change improvement in agentic coding"; 87.6% SWE-bench |
| Standard coding, implementation, review | Sonnet 4.6 | "Best combination of speed and intelligence"; 79.6%, 55 tok/s |
| Real-time / low-latency pair programming | Haiku 4.5 | "Near-frontier coding quality with blazing speed" |
| High-throughput parallel subtasks | Haiku 4.5 | "Sonnet orchestrates a team of Haiku 4.5s for subtasks" (Anthropic) |
| PhD-level reasoning, novel problems | Opus 4.7 | 94.2% GPQA Diamond; deepest reasoning |
| Architecture / One-Way Door design | Opus 4.7 | Jan 2026 knowledge cutoff; largest reasoning budget |
| Offline batch (non-latency-sensitive) | Batch Opus or Batch Haiku | Batch Opus ($2.50 input) cheaper than standard Sonnet ($3.00) |

## Implications for OCX tier→model mapping

1. **The "within 1.2pp of Opus" rationale in `workflow-swarm.md` is stale and must be updated.** Opus 4.7 vs Sonnet 4.6 is 8.0pp on SWE-bench Verified — a meaningful gap. Sonnet for reviewer/tester/builder remains cost-defensible, but the justification string should change to reflect the actual trade-off ("8pp quality gap at 1.67× lower cost and 2× speed").

2. **Opus 4.7's tokenizer inflation (0–35% on code) erodes the apparent price parity with Opus 4.6.** The `worker-architect` (fixed opus) and tier=max builder will see real cost increases. This strengthens the case for reserving Opus strictly for high-ambiguity architectural decisions and not using it for mechanical work.

3. **Haiku 4.5 is viable for `worker-explorer` and narrow-scope workers — but the 200k context cap is a hard blocker for codebase-wide workers.** `worker-architecture-explorer` and `worker-researcher` (which load long web content) should stay on Sonnet 4.6 for the 1M context window. Haiku is safe for targeted single-file searches and simple test generation.

4. **`worker-doc-reviewer` is a strong Haiku candidate at tier=low.** Single-pass document consistency review maps well to Haiku's strengths (fast, near-Sonnet-4.0 quality). Context limit of 200k may suffice for individual doc files but could bind on full user-guide audits.

5. **Batch API creates a new cost-routing dimension the current swarm system doesn't model.** For non-interactive workers (post-session doc audits, offline review passes), batch Opus ($2.50/MTok) costs less than real-time Sonnet ($3.00/MTok) while delivering materially higher quality. The architect should consider a `--batch` overlay or tier-specific batch routing for workers where latency is irrelevant.

## Sources

- Anthropic Models Overview — platform.claude.com/docs/en/docs/about-claude/models/overview (accessed 2026-04-19)
- Anthropic Pricing Page — platform.claude.com/docs/en/about-claude/pricing (accessed 2026-04-19)
- Claude Opus 4.7 Benchmarks Review — buildfastwithai.com/blogs/claude-opus-4-7-review-benchmarks-2026
- Claude Benchmarks 2026 — morphllm.com/claude-benchmarks (Opus 4.6, Sonnet 4.6, Haiku 4.5 SWE-bench + ARC-AGI-2 side-by-side)
- Opus 4.7 Tokenizer Cost Analysis — finout.io/blog/claude-opus-4.7-pricing-the-real-cost-story-behind-the-unchanged-price-tag
- Vellum LLM Leaderboard 2026 — vellum.ai/llm-leaderboard
- Anthropic: Introducing Claude Haiku 4.5 — anthropic.com/news/claude-haiku-4-5
- Qodo: Haiku 4.5 vs Sonnet 4.5 on 400 Real PRs — qodo.ai/blog/thinking-vs-thinking-benchmarking-claude-haiku-4-5-and-sonnet-4-5-on-400-real-prs
- LLM Stats: Claude Opus 4.7 Launch — llm-stats.com/blog/research/claude-opus-4-7-launch
- BenchLM: Claude API Pricing April 2026 — benchlm.ai/blog/posts/claude-api-pricing
