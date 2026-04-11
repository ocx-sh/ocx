# Code Quality Standards

Canonical design principles applicable to **all languages**. This is the shareable,
project-independent root rule. For language-specific applications, see the leaf rules
listed in **See Also** at the bottom.

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

- Extract shared logic only when **2+ genuinely different callers** exist — incidental similarity is not duplication
- Prefer the language's zero-cost abstraction mechanism (generics, protocols, type parameters) over runtime indirection
- Single source of truth for business logic — if a rule exists in two places, one will go stale
- **When NOT to DRY**: test code (explicit > clever), similar error handling representing distinct situations, coupling risk outweighs deduplication

### YAGNI

- **Start concrete.** Extract abstractions only when a second genuinely different use case appears
- **No premature generics.** A function that only handles one type doesn't need type parameters until called with something else
- **Don't over-engineer error types.** If callers distinguish 2 cases, don't create 20 variants/subclasses
- **No feature flags or compatibility shims** when you can just change the code
- Three similar lines of code beat a premature abstraction

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
- Catching/swallowing errors silently (no logging, no re-raise)
- God objects/modules with 15+ fields/methods spanning unrelated concerns

### Universal Warn-tier Anti-Patterns
- Boolean parameters where an enum/literal type would be clearer
- Stringly-typed APIs where structured types prevent typos at compile/type-check time
- Unnecessary copies/clones in hot paths
- Missing error context (bare re-raise without adding information)

---

## Reusability Assessment

Before writing new code, ask:
- "Could a second caller use this, or would it need to copy-paste?"
- "Is this in the right layer?" (generic utility vs. domain logic vs. command-specific glue)
- "Is this a generic capability dressed up as a specific feature?"

**Signals of misplaced code:**
- Cross-cutting concern implemented inline (progress, retry, rate-limiting, path sanitization)
- Platform-specific logic in a library instead of the application layer
- Generic utility mixed into command-specific code

---

## Code Review Checklist (All Languages)

- [ ] Errors propagated with context, not swallowed; logged once at boundary
- [ ] No god objects — each module/class has a single responsibility
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

## Refactoring Discipline

**Two Hats Rule**: Never mix refactoring and optimization in the same session.

- **Hat 1: Refactoring** — Change structure, NOT behavior. Tests must pass unchanged.
- **Hat 2: Optimization** — Improve performance, NOT behavior. Benchmarks required.

When switching hats, commit first, then switch context.

---

## See Also — Language-Specific Quality Rules

Each of these is path-scoped — it loads automatically when you edit files of that language:

- `quality-rust.md` — Rust-specific quality (ownership, async/Tokio, error handling, edition 2024)
- `quality-python.md` — Python-specific quality (type hints, async, modern tooling)
- `quality-typescript.md` — TypeScript-specific quality (strict mode, ESM, narrowing)
- `quality-bash.md` — Bash script safety (`set -euo pipefail`, quoting, shellcheck)
- `quality-vite.md` — Vite build-tool opinions (Vite 7/8, VitePress, Rolldown)
