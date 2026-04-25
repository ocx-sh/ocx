# Code Quality Standards

Canonical design principles for **all languages**. Shareable, project-independent root rule. Language-specific applications: see leaf rules in **See Also** below.

---

## Design Principles

### SOLID

| Principle | Meaning | Violation Signal |
|-----------|---------|------------------|
| **SRP** | One responsibility per module/class/struct | Module with methods spanning unrelated concerns |
| **OCP** | Extend behavior without modifying existing code | Editing existing branches to add new cases |
| **LSP** | Subtypes/implementations honor the parent contract | Implementation that panics/throws where contract promises success |
| **ISP** | Depend on the narrowest interface needed | Requiring capabilities the consumer never uses |
| **DIP** | High-level modules depend on abstractions, not concretions | Constructor takes a concrete implementation instead of an interface/trait/protocol |

### DRY

- Extract shared logic only when **2+ genuinely different callers** exist — incidental similarity not duplication
- Prefer language's zero-cost abstraction (generics, protocols, type parameters) over runtime indirection
- Single source of truth for business logic — rule in two places, one go stale
- **When NOT to DRY** (prefer DAMP — Descriptive And Meaningful Phrases): test code self-contained, readable in isolation, even at cost of repetition. Also: similar error handling for distinct situations, coupling risk outweighs dedup

### KISS (Keep It Simple, Stupid)

Simplicity = prerequisite for reliability. Every line, abstraction, indirection = liability until proven otherwise.

- Prefer straightforward code newcomer can read over clever code that impresses peers
- Design needs diagram to explain → too complex for problem
- Complexity not badge of thoroughness — cost that compounds
- Doubt? Write naive solution first; optimize only when measurement demands

### Choose Boring Technology

Teams have finite "innovation tokens" (Dan McKinley, 2015). Boring tech mature, battle-tested, *known* failure modes — novel tech introduces unknown unknowns that compound operational cost.

- Default to established option; save novelty for genuine differentiation
- "Best tool for job" = local optimization — actual job is keeping system running
- Each novel dependency/framework/language spends innovation token; budget ~3 total
- Evaluate alternatives by operational maturity and team familiarity, not just features

### YAGNI

- **Start concrete.** Extract abstractions only when second genuinely different use case appears
- **No premature generics.** Function handling one type needs no type parameters until called with another
- **Don't over-engineer error types.** Callers distinguish 2 cases? Don't create 20 variants/subclasses
- **No feature flags or compatibility shims** when you can just change the code
- Three similar lines beat premature abstraction

---

## Anti-Pattern Severity

| Tier | Meaning | Action |
|------|---------|--------|
| **Block** | Correctness or security risk | Must fix before merge |
| **Warn** | Design smell, performance issue, maintainability risk | Should fix, can negotiate |
| **Suggest** | Improvement opportunity | Could fix, optional |

### Universal Block-tier Anti-Patterns
- Hardcoded secrets or credentials
- Unvalidated external input at system boundaries
- Catching/swallowing errors silently (no log, no re-raise)
- God objects/modules with 15+ fields/methods spanning unrelated concerns

### Universal Warn-tier Anti-Patterns
- Boolean parameters where enum/literal type clearer
- Stringly-typed APIs where structured types prevent typos at compile/type-check time
- Unnecessary copies/clones in hot paths
- Missing error context (bare re-raise without adding info)

---

## Reusability Assessment

Before writing new code, ask:
- "Could second caller use this, or copy-paste?"
- "Right layer?" (generic utility vs. domain logic vs. command-specific glue)
- "Generic capability dressed up as specific feature?"

**Signals of misplaced code:**
- Cross-cutting concern inline (progress, retry, rate-limiting, path sanitization)
- Platform-specific logic in library instead of application layer
- Generic utility mixed into command-specific code

---

## Code Review Checklist (All Languages)

- [ ] Errors propagated with context, not swallowed; logged once at boundary
- [ ] No god objects — each module/class single responsibility
- [ ] Follows existing codebase patterns (grep before inventing)
- [ ] Generic logic in library layer, command-specific in application layer
- [ ] No premature abstractions — extraction justified by real duplication
- [ ] External input validated at system boundaries

---

## Performance Checklist

- N+1 query patterns (loops with DB calls)
- Blocking I/O in async paths (e.g., `std::fs::*` in async Rust, sync HTTP clients in async handlers)
- Excessive memory allocations (clones in hot loops, intermediate collections)
- Missing pagination
- Inefficient algorithms (O(n²) when O(n) possible)
- Cache opportunities missed
- Unbounded channels without backpressure
- Locks held across suspension points (e.g., `MutexGuard` across `.await` in Rust)

---

## Refactoring Tooling

Before refactoring, check available tools via `ToolSearch` — capabilities like LSP may be deferred tools not loaded by default. Prefer semantic tooling (LSP `findReferences`, `workspaceSymbol`, `goToDefinition`) over text search (Grep) for symbol-level ops like renames and reference lookups. Fall back to Grep for non-code searches (comments, docs, config).

---

## Refactoring Discipline

**Two Hats Rule**: Never mix refactoring and optimization in same session.

- **Hat 1: Refactoring** — Change structure, NOT behavior. Tests pass unchanged.
- **Hat 2: Optimization** — Improve performance, NOT behavior. Benchmarks required.

Switching hats? Commit first, then switch context.

---

## Red Flags — Two Hats Rationalizations

| Rationalization | Red flag | Correct action |
|---|---|---|
| "I'll optimize this loop while I refactor it" | Hat 1 and Hat 2 mixed in one pass | Commit the refactor first. Then put on Hat 2. |
| "The benchmark runs fine locally, I'll skip it" | No benchmark output in the commit | Put on Hat 2 = benchmarks are required. Record the before/after. |
| "This refactor is small, I don't need a test" | No tests cover the changed code | If no safety net exists, write characterization tests first — that's Hat 1 prep, not optional. |

---

## Verification Honesty

Verification claims must be evidence-backed. Hedging in review verdicts, commit messages, completion reports masks uncertainty, degrades trust in quality gates.

### Banned Phrases

| Phrase | Replace With |
|--------|-------------|
| "should work" | "verified by [test name / command output]" |
| "probably", "likely" | state what was checked and what the result was |
| "seems to" | "confirmed that [X] by [method]" |
| "Great!", "Perfect!", "Done!" | evidence of completion (test pass, clean diff, gate output) |

### Classification

- Hedging in review verdict or completion report: **Warn-tier**
- Premature celebration before verification evidence: **Warn-tier**
- Stating "verified" without citing evidence: **Block-tier** (false verification)

---

## See Also — Language-Specific Quality Rules

Each path-scoped — loads automatically when editing files of that language:

- `quality-rust.md` — Rust-specific quality (ownership, async/Tokio, error handling, edition 2024)
- `quality-python.md` — Python-specific quality (type hints, async, modern tooling)
- `quality-typescript.md` — TypeScript-specific quality (strict mode, ESM, narrowing)
- `quality-bash.md` — Bash script safety (`set -euo pipefail`, quoting, shellcheck)
- `quality-vite.md` — Vite build-tool opinions (Vite 7/8, VitePress, Rolldown)