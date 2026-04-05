# Code Quality Standards

Canonical design principles, quality gates, and refactoring discipline. Applies to **all languages** (Rust, Python, TypeScript, Bash). For Rust-specific patterns and async conventions, see `.claude/rules/rust-quality.md`.

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
- Blocking I/O in async paths (`std::fs::*` in async context, `std::thread::sleep`)
- Excessive memory allocations (`.clone()` in hot loops, intermediate `Vec`)
- Missing pagination
- Inefficient algorithms (O(n²) when O(n) possible)
- Cache opportunities missed
- Unbounded channels without backpressure
- `MutexGuard` held across `.await`

## Quality Gates

**Run `task verify`** — this executes all gates in order. Do not run gates individually unless debugging a specific failure. Run `task --list` to discover all available task commands.

### Prerequisites

`task verify` requires these tools installed and available:

| Tool | Install | Used by |
|------|---------|---------|
| `cargo-nextest` | Auto-installed by task | `test:unit` |
| `hawkeye` | Auto-installed by task | `license:check` |
| `cargo-deny` | Auto-installed by task | `license:deps` |
| `ocx.sh/shellcheck:0.11` | `ocx install --select shellcheck:0.11` | `lint:shell` |
| `ocx.sh/shfmt:3` | `ocx install --select shfmt:3` | `lint:shell` |
| Docker (registry:2) | System install | `test:parallel` |
| `uv` | System install | `lint:ai-config`, `test:parallel` |

Shell linting tools (`shellcheck`, `shfmt`) are managed by OCX itself and must be installed in the global OCX store (`~/.ocx`). The project uses a pinned local index at `.ocx/index/` (set via `OCX_INDEX` in the root taskfile) so versions are locked. First-time setup:

```sh
ocx index update shellcheck shfmt     # fetch tag metadata
ocx install --select shellcheck:0.11  # install shellcheck
ocx install --select shfmt:3          # install shfmt
```

### Gates (in order)

`task verify` runs:
1. `format:check` — `cargo fmt --check`
2. `clippy:check` — `cargo clippy --workspace --locked`
3. `lint:shell` — shellcheck + shfmt on `.sh` scripts (via `ocx exec`)
4. `lint:ai-config` — AI config structural tests (`.claude/tests/`)
5. `license:check` — SPDX headers (hawkeye)
6. `license:deps` — dependency license audit (cargo-deny)
7. `build` — `cargo build --release -p ocx --locked`
8. `test:unit` — `cargo nextest run --workspace --release --locked`
9. `test:parallel` — acceptance tests with pytest-xdist

## Taskfile Conventions

- **Composite tasks aggregate subtasks.** A quality-gate task (e.g., `lint:shell`) should call individual tool tasks (`shell:lint`, `shell:format:check`), not run multiple tools inline.
- **Individual tools get their own subtask.** Each linter/formatter is a separate task so it can be run independently for debugging or one-off use.
- **`task verify` references composite tasks only.** Keep the verify pipeline simple — one entry per concern, not one per tool.

## Refactoring Discipline

**Two Hats Rule**: Never mix refactoring and optimization in the same session.

- **Hat 1: Refactoring** - Change structure, NOT behavior. Tests must pass unchanged.
- **Hat 2: Optimization** - Improve performance, NOT behavior. Benchmarks required.

When switching hats, commit first, then switch context.
