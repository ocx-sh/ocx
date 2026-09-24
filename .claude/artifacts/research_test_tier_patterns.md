# Research: Test-tier patterns — tiered gates, agent inner loops, pyramid migrations, structural lints

## Metadata

**Date:** 2026-09-22
**Domain:** testing
**Triggered by:** hex-architect ADR on test-suite speed tiers (axis: design-pattern precedent)
**Expires:** 2027-03-22

## Direct Answer

### 1. Tiered verification in practice

Google's established split runs only fast, reliable tests at presubmit (~11 min budget, mostly unit-scope). Slower or less stable tests go to postsubmit, which accepts some coverage loss at presubmit in exchange for velocity — [SWE at Google ch23](https://abseil.io/resources/swe-book/html/ch23.html). The split rests on the Test Sizes taxonomy:

- small = single-process, no I/O;
- medium = multi-process, localhost only;
- large = multi-machine, hours.

The target mix is ~80/15/5 small:medium:large — [Test Sizes](https://testing.googleblog.com/2010/12/test-sizes.html), [SWE at Google ch14](https://abseil.io/resources/swe-book/html/ch14.html).

Documented failure mode: detection lag. TAP postsubmit takes ~1–2 h to start failing on a broken change, and finding the culprit plus notifying adds up to 4 h+ before the author learns of it. Google responded with ML-driven speculative postsubmit scheduling, which cut median detection time by ~65% (107 → 37 min) — [ICST 2025](https://hackthology.com/speculative-testing-at-google-with-transition-prediction.html).

Chromium's commit queue (CQ) shows the tier boundary decaying at scale:

- It tolerates ~20,000 unique flaky tests.
- A 1-in-1000 flake across 100 builders gives only ~90.47% presubmit pass probability.
- 15–25% of CQ jobs were wrongly rejected in a sampled week — [Findit](https://sites.google.com/chromium.org/cat/findit).
- Builders marked "informational" or "post-submit only" are skipped in the CQ, and that is how bad CLs slip through. Marking a flaky test informational is a last resort because "it allows new breakages to slip in" — [Chromium Breakage and Flake Policy](https://www.chromium.org/chromium-os/developer-library/guides/testing/breakages-and-flakes/).

Rust precedent:

- bors/homu builds a scratch integration branch (`auto`) from the PR plus the base and runs full CI there. Verification always runs against the prospective merge — [Rust Forge, Bors](https://forge.rust-lang.org/infra/docs/bors.html).
- The rewritten rust-lang/bors separates a `try` tier from an `auto` merge tier — [rust-lang/bors](https://github.com/rust-lang/bors/blob/main/README.md).
- Cargo moved to GitHub merge queues (the `merge_group` event: Tier 1 on every push, Tier 2 at merge against the tentative merge commit) — [cargo#14718](https://github.com/rust-lang/cargo/pull/14718).

### 2. Verification for AI coding agents

The one concrete framing draws the inner/outer-loop boundary by dependence on external resources:

- Unit tests with no external resources belong to the inner loop.
- Anything that touches real dependencies (network, DB, containers) belongs to the outer loop.

It names Claude Code as spanning both loops; outer-loop agents (Devin, the Copilot coding agent) return a PR that goes through CI — [Augment Code](https://www.augmentcode.com/guides/inner-loop-vs-outer-loop-ai-agents).

**Evidence gap:** no source gives quantitative escape rates comparing narrow and broad per-change gates for diffs written by AI agents. DORA reports that ~30% of developers have little or no trust in AI code, and teams compensate with slower human review, not wider automated gates (same source). No agent vendor publishes a documented test-scope policy per tier — [awesome-harness-engineering](https://github.com/ai-boost/awesome-harness-engineering). Any tiering specific to agents should therefore be treated as novel and unvalidated.

### 3. Test-pyramid migrations at scale

Airbnb migrated ~3,500 test files from Enzyme to React Testing Library with LLM assistance: 6 weeks for 6 engineers, against an 18-month manual estimate. Three points matter here:

- Airbnb could **not delete the old suite first**; a coverage analysis showed gaps. The pattern was migrate, prove equivalence, then delete.
- ~3% of the files were finished by hand after 50–100 automated retries each.
- The method used to prove equivalence is **undisclosed** — [Airbnb Engineering](https://airbnb.tech/infrastructure/accelerating-large-scale-test-migration-with-llms/).

Mutation testing is the established tool for showing that a replacement suite is not weaker: coverage only proves that code ran, not that an assertion would catch a fault — [Codecov](https://about.codecov.io/blog/mutation-testing-how-to-ensure-code-coverage-isnt-a-vanity-metric/), [JAVAPRO PIT](https://javapro.io/2026/01/21/test-your-tests-in-java-with-pit/). No public case study uses mutation testing to sign off an e2e→unit migration; the academic work on unittest→pytest measures migration correctness, not mutation-kill parity — [TestMigrationsInPy](https://arxiv.org/pdf/2602.05122), [ACM AST 2026](https://arxiv.org/html/2602.02964). Mutation-proofing ported tests is sound but emerging.

### 4. Structural and architecture tests

ArchUnit frames itself as "a linter for your architecture" but runs as ordinary tests. It needed a dedicated class-import cache to stay cheap — [Cloudflight](https://engineering.cloudflight.io/en/archunit-linting-your-achitecture-with-unit-tests/), [ArchUnit guide](https://www.archunit.org/userguide/html/000_Index.html). No case study of moving such checks into a lint phase, and no guidance specific to Bazel, was found.

Three Bazel facts apply:

1. Tests get their own caching semantics, which is why a `genrule` should not be used as a test — [Bazel General Rules](https://bazel.build/reference/be/general).
2. The test-result cache re-runs a target only when its transitive inputs change — [How Bazel Caching Works](https://sluongng.hashnode.dev/bazel-caching-explained-pt-1-how-bazel-works).
3. An action that calls a binary or script it does not declare as an input reuses stale cached output — [bazel#3856](https://github.com/bazelbuild/bazel/issues/3856).

Net: the distinction that matters is caching and invalidation semantics with fully declared inputs, not the "test vs lint" label. OCX's own separately cached `bazel:lint`/`bazel:tag:guard` gates are the most current precedent found.

## Technology Landscape

| | Tiered CI gates | Agent verification scope | Pyramid migration | Structural invariants |
|---|---|---|---|---|
| **Trending** | GitHub-native merge queues (cargo#14718); gating on the tentative merge commit | Inner/outer loop split by resource dependence (Augment) | LLM-assisted bulk migration with a manual tail (Airbnb) | Separately labelled, cached Bazel lint/test targets (OCX) |
| **Established** | Presubmit/postsubmit split; Test Sizes 80/15/5; bors integration branch | — (no metric-backed policy found) | Mutation testing as proof of suite equivalence | ArchUnit in the suite with caching; Bazel test vs genrule semantics |
| **Emerging** | ML-driven speculative test scheduling (ICST 2025) | "Evals ≠ unit tests" at harness level | Mutation-proofing ported tests before deletion | Grep checks as input-tracked Bazel actions |
| **Declining** | Bespoke merge bots; "more e2e for confidence" | — | Unassisted manual migration at scale | Raw shell greps outside the build graph (bazel#3856) |

## Key Findings

- Google's lag numbers (1–2 h to fail, 4 h+ to notify) argue for a fast inner tier **plus** a slow tier that someone actively monitors.
- Chromium documents "skip in CQ, run only post-submit" as a known escape path, not a safe design.
- rust-lang/bors `try` vs `auto` is the cleanest precedent for a cheap try tier plus a gating merge tier.
- Airbnb validates the "port, then delete after proof" order; the equivalence mechanism is a gap.
- Narrow gates specific to agents have no measured escape-rate evidence; flag them as novel.
- For the lint phase, declared inputs and caching semantics matter more than categorisation.

## Sources

See the inline links above. All retrieved 2026-09-22.
