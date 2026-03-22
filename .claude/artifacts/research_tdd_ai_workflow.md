# Research: TDD in AI-Assisted Workflows

## Metadata

**Date:** 2026-03-22
**Domain:** testing
**Triggered by:** Designing a stub-first, test-second, implement-third AI planning and execution workflow
**Expires:** 2026-09-22

## Direct Answer

The "stub-first, test-second, implement-third" pattern maps directly onto **London-school TDD (outside-in)**, enriched by ATDD, and independently rediscovered in 2025 as **Spec-Driven Development (SDD)**. The pattern is architecturally sound and empirically validated by 2026 research on AI coding agents. The single most important invariant: tests must be derived from the design record, not from the stubs. AI will naturally read stubs to generate tests unless explicitly prevented — producing circular, spec-decoupled tests that validate the stub's shape rather than the intended behavior.

## Technology Landscape

### Trending

| Tool/Pattern | Adoption Signal | Key Benefit | Relevance |
|---|---|---|---|
| Spec-Driven Development (SDD) | GitHub spec-kit OSS CLI (Sept 2025); Thoughtworks Tech Radar; Kiro, Tessl competing tools | Specs are executable validation gates — drift fails the build | Directly implements "design record drives tests" |
| ATDD with AI (ATDD-AI) | Paul Duvall's ATDD-AI workflow; Uncle Bob's empire-2025 produced Claude Code plugin (swingerman/atdd) | Tests become the primary AI steering mechanism | Acceptance tests constrain AI to observable contract |
| TDAD (Test-Driven Agentic Development) | arXiv:2603.17973 March 2026; 70% regression reduction; MIT licensed, pip installable | Graph-based test impact analysis prevents AI regressions | Empirically validates the "verify tests fail" gate |

### Established

| Tool/Pattern | Status | Notes |
|---|---|---|
| London-school TDD (outside-in, mockist) | Mature — GOOS book (Freeman/Pryce) canonical | Acceptance test first, unit test second, implement third |
| BDD / Gherkin | Mature — Cucumber, Behave, pytest-bdd | Living documentation prevents spec drift |
| Red-Green-Refactor | Standard TDD cycle | AI adaptation: write all tests, implement all at once |

### Declining

| Tool/Pattern | Signal | Avoid Because |
|---|---|---|
| Chicago-school (inside-out) TDD with AI | Eric Elliott argues breakdown at AI speed | State-based tests couple to implementation, not spec |
| Waterfall SDD (frozen specs) | Marmelab essay Nov 2025; practitioner postmortems | Specs drift faster than teams maintain them |
| AI-generated tests from stubs | Documented AI anti-pattern | Circular — validates stub shape, not intended behavior |

## Key Findings

1. **Circular test generation is the primary failure mode.** AI reads stubs and writes tests that validate the stub's shape. Fix: strict input sequencing — design record only. Source: [arXiv:2602.00180](https://arxiv.org/html/2602.00180v1)
2. **London-school TDD is the correct foundation.** Outside-in: acceptance test → unit test → implementation. Source: [TDD Wars](https://medium.com/@adrianbooth/test-driven-development-wars-detroit-vs-london-classicist-vs-mockist-9956c78ae95f)
3. **TDAD: TDD prompting alone increased regressions 9.94% in smaller models.** Contextual information (which tests to run) outperforms procedural instructions (how to do TDD). Source: [arXiv:2603.17973](https://arxiv.org/abs/2603.17973)
4. **"Verify tests fail" is not optional.** Gate must confirm: exit non-zero, expected test names present, no trivially-passing tests against stubs.
5. **BDD's living documentation prevents spec drift by construction.** Spec = executable test = same artifact. Source: [Cucumber BDD](https://cucumber.io/docs/bdd/)
6. **SDD tools reached critical mass in 2025.** Martin Fowler comparative analysis, Thoughtworks Tech Radar. Source: [Fowler on SDD](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html)

## Design Patterns

- **Walking Skeleton** — `unimplemented!()` stubs are exactly this pattern. Source: [Code Climate](https://codeclimate.com/blog/kickstart-your-next-project-with-a-walking-skeleton)
- **Acceptance Test as AI Steering** — Prompt: "make this test pass," not "implement this feature." Source: [paulmduvall.com](https://www.paulmduvall.com/atdd-driven-ai-development-how-prompting-and-tests-steer-the-code/)
- **Spec Gate** — Tests from design record become CI gates. If implementation diverges, build fails. Source: [InfoQ](https://www.infoq.com/articles/spec-driven-development/)
- **Two-Layer Testing (ATDD + TDD)** — Acceptance tests: observable contract. Unit tests: internal invariants. Both from the spec.

## Recommendation

Four enforcement mechanisms for the OCX swarm workflow:

1. `worker-tester` receives plan/ADR as primary input with explicit instruction not to read stub implementations
2. Stub writing and test writing are sequential, not concurrent — prevents context contamination
3. "Verify tests fail" is a mandatory gate: exit non-zero, test names present, no trivially-passing tests
4. Design records are living documents — update plan before writing tests for unspecified decisions

## Sources

| Source | Type | Date |
|---|---|---|
| [TDAD: arXiv:2603.17973](https://arxiv.org/abs/2603.17973) | Research | March 2026 |
| [TDFlow: arXiv:2510.23761](https://arxiv.org/html/2510.23761) | Research | Oct 2025 |
| [SDD: arXiv:2602.00180](https://arxiv.org/html/2602.00180v1) | Research | Feb 2026 |
| [Fowler on SDD Tools](https://martinfowler.com/articles/exploring-gen-ai/sdd-3-tools.html) | Analysis | Late 2025 |
| [ATDD-AI Workflow](https://www.paulmduvall.com/atdd-driven-ai-development-how-prompting-and-tests-steer-the-code/) | Blog | 2025 |
| [SDD Waterfall Strikes Back](https://marmelab.com/blog/2025/11/12/spec-driven-development-waterfall-strikes-back.html) | Blog | Nov 2025 |
| [TDD Wars: Detroit vs London](https://medium.com/@adrianbooth/test-driven-development-wars-detroit-vs-london-classicist-vs-mockist-9956c78ae95f) | Blog | — |
| [Walking Skeleton](https://codeclimate.com/blog/kickstart-your-next-project-with-a-walking-skeleton) | Blog | — |
| [SDD Thoughtworks](https://www.thoughtworks.com/en-us/insights/blog/agile-engineering-practices/spec-driven-development-unpacking-2025-new-engineering-practices) | Analysis | 2025 |
| [ATDD Claude Code Plugin](https://github.com/swingerman/atdd) | OSS | 2025 |
| [BDD Cucumber](https://cucumber.io/docs/bdd/) | Docs | Current |
