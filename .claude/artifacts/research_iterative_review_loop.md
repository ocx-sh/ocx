# Research: Automated Iterative Review-Fix Loops

## Metadata

**Date:** 2026-03-22
**Domain:** quality, workflow
**Triggered by:** Automating the manual review-fix-review cycle in AI-assisted feature development
**Expires:** 2026-09-22

## Direct Answer

The pattern — implement, review, fix, re-review, repeat until clean, report deferred — is well-grounded. Three non-negotiable design decisions: (1) use a fresh-context reviewer, not the same session that wrote the code; (2) set a hard 3-round budget; (3) classify findings into actionable vs. deferred before the loop runs, so it never stalls on items requiring human judgment.

## Technology Landscape

### Trending

| Tool/Pattern | Adoption Signal | Key Benefit |
|---|---|---|
| Generator-Critic (Judge Agent) | HubSpot Sidekick, 80% approval, 90% faster feedback (March 2026) | Separates generation and evaluation, eliminates same-context bias |
| Severity-gated loop termination | LLMLOOP (ICSME 2025), Reflexion | Boolean stop conditions per failure mode converge faster |
| Claude Agent SDK `max_turns` + `max_budget_usd` | Anthropic official | Hard budget cap prevents runaway sessions |

### Established

| Tool/Pattern | Status | Notes |
|---|---|---|
| Self-Refine (Madaan et al., 2023) | Mature baseline | ~20% average improvement; diminishing returns after round 3 |
| Reflexion (Shinn et al., 2023) | Widely cited | Oscillation heuristic: same (action, observation) pair 3+ times = halt |
| Fresh-context reviewer | Anthropic best practice | "A fresh context improves code review since Claude won't be biased toward code it just wrote" |

### Declining

| Tool/Pattern | Signal | Avoid Because |
|---|---|---|
| Single-context self-review | Oct 2025 Dunning-Kruger research | Inherits same blind spots as generation |
| Unbounded loops | Documented cost-spike root cause | Cannot distinguish progress from being stuck |
| Prompt-level oscillation prevention | No evidence of effectiveness | Needs structural enforcement, not instructions |

## Key Findings

1. **Diminishing returns at round 3.** Self-Refine: quality flattens fast. Round 4+ is mostly noise or oscillation. Source: [arXiv:2303.17651](https://arxiv.org/abs/2303.17651)
2. **Fresh context is the most impactful choice.** LLMs show Dunning-Kruger-like overconfidence when self-reviewing. Source: [arXiv:2510.05457](https://arxiv.org/abs/2510.05457)
3. **Oscillation requires structural detection.** Hash (action, observation) pairs; halt if same pair appears 3+ times. Source: [arXiv:2303.11366](https://arxiv.org/abs/2303.11366)
4. **Type-specific sub-loops outperform monolithic loops.** LLMLOOP's five nested loops (one per failure mode) converge faster than one general "fix everything" loop. Source: [ICSME 2025](https://conf.researchr.org/details/icsme-2025/icsme-2025-tool-demonstration/8/LLMLOOP-Improving-LLM-Generated-Code-and-Tests-through-Automated-Iterative-Feedback-)
5. **LLM yes/no convergence signals are unreliable.** Structured output (JSON count of findings by severity) is more reliable. `task verify` is ground truth.
6. **Deferred summary is a first-class output.** Loop produces: (1) what was fixed, (2) what was deferred with rationale.

## Recommendation

3-round maximum, fresh-context reviewer, severity-gated termination (Block+Warn drive the loop, Suggest goes to deferred), `task verify` as exit gate. Do not extend beyond 3 rounds — remaining findings belong in the deferred summary for a human.

## Sources

| Source | Type | Date |
|---|---|---|
| [Self-Refine: arXiv:2303.17651](https://arxiv.org/abs/2303.17651) | Research | 2023 |
| [Reflexion: arXiv:2303.11366](https://arxiv.org/abs/2303.11366) | Research | 2023 |
| [LLMLOOP — ICSME 2025](https://conf.researchr.org/details/icsme-2025/icsme-2025-tool-demonstration/8/LLMLOOP-Improving-LLM-Generated-Code-and-Tests-through-Automated-Iterative-Feedback-) | Research | 2025 |
| [HubSpot Sidekick — InfoQ](https://www.infoq.com/news/2026/03/hubspot-ai-code-review-agent/) | Case study | March 2026 |
| [Claude Agent SDK — Agent Loop](https://platform.claude.com/docs/en/agent-sdk/agent-loop) | Docs | 2025 |
| [Claude Code Best Practices](https://code.claude.com/docs/en/best-practices) | Docs | 2025 |
| [Dunning-Kruger in Code Models: arXiv:2510.05457](https://arxiv.org/abs/2510.05457) | Research | Oct 2025 |
| [Designing Agentic Loops — Simon Willison](https://simonwillison.net/2025/Sep/30/designing-agentic-loops/) | Blog | Sep 2025 |
