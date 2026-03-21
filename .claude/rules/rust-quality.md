---
paths:
  - crates/**/*.rs
  - external/**/*.rs
---

# Rust Code Quality

Rust-specific quality reference for the OCX project. Universal design principles (SOLID, DRY, YAGNI, anti-pattern severity, reusability, review checklist) live in `code-quality.md` — this file covers **Rust-specific applications** of those principles plus OCX async/pattern conventions.

---

## Design Patterns

### Builder Pattern
Use a consuming builder (`self` not `&mut self`) for structs with 4+ optional fields. Return `Self` from each setter for method chaining. For required fields, consider a typestate builder where `build()` only exists on the state with all required fields set — missing fields become compile errors. OCX example: `ClientBuilder`, `BundleBuilder`.

### Newtype Pattern
Wrap primitives in single-field tuple structs for type safety. Zero runtime cost. Use when: enforcing invariants (e.g., `NonEmptyString`), adding type safety (e.g., `Digest` wrapping a hash string), or bypassing the orphan rule. Always implement `Display`, `Debug`, `From` on newtypes.

### RAII Guards
Acquire resources in constructors, release in `Drop`. OCX example: `TempAcquireResult` holds a `FileLock` released on drop. `ReferenceManager` patterns use RAII for back-reference cleanup.

### Strategy via Traits
Define behavior as a trait, inject the implementation. Prefer static dispatch (`impl Trait` / `<T: Trait>`) for zero-cost monomorphization. Use `dyn Trait` only when you genuinely need runtime polymorphism (heterogeneous collections, plugin systems). OCX example: `IndexImpl` trait with `LocalIndex` and `RemoteIndex` implementations.

### Typestate Pattern
Encode valid states as distinct types. State transitions consume `self` and return a new type, making invalid transitions compile errors. Zero runtime overhead via `PhantomData`. Use when protocol correctness matters (connection states, build phases).

---

## Anti-Patterns (Tiered Severity)

### Block (must fix before merge)
- **`.unwrap()` in library code** — `crates/ocx_lib/` and `crates/ocx_mirror/` must return `Result`. `.expect("reason")` acceptable only for invariants proven at compile time. Tests may use `.unwrap()`.
- **`MutexGuard` across `.await`** — extract data, drop guard, then await. Use `tokio::sync::Mutex` only when lock genuinely spans an await.
- **`unsafe` without safety comment** — every `unsafe` block must document the invariant it relies on.
- **Blocking I/O in async** — never `std::fs::*`, `std::thread::sleep`, or any blocking stdlib call in async context. Use `tokio::fs::*`, `tokio::time::sleep`, or `spawn_blocking`.
- **`From` impl hiding `.unwrap()`** — `From` must be infallible. Use `TryFrom` for fallible conversions.

### Warn (should fix)
- **Unnecessary `.clone()`** — cloning to silence the borrow checker masks a design problem. Restructure ownership, pass references, or use indices instead.
- **`Box<dyn Trait>` where `impl Trait` suffices** — vtable + heap allocation overhead. If only one implementation exists or the set is known at compile time, use generics.
- **Stringly-typed APIs** — `String` where an enum would prevent typos at compile time. Applies to error types too: `String` errors prevent programmatic matching.
- **Boolean parameters** — `fn sort(ascending: bool)` is less clear than `fn sort(order: SortOrder)`. Use enums for two-state flags.
- **Missing `From`/`Into`** — if callers frequently write `String::from(x)` or `.into()`, add `impl From<T>` on your type. The `?` operator relies on `From`.
- **God structs** — 15+ fields spanning unrelated concerns. Decompose into focused structs.

### Suggest (improvement)
- **`Cow<str>`** for functions that usually return borrowed data but sometimes need to allocate.
- **`#[must_use]`** on return types callers might accidentally discard.
- **`#[non_exhaustive]`** on public enums whose variant set may grow.
- **Iterator chains** over materializing intermediate `Vec` — `.iter().map().filter().collect()` instead of building a `Vec` then iterating it.
- **`impl Into<T>` parameters** — `fn process(name: impl Into<String>)` accepts both `&str` and `String` without forcing callers to allocate.

---

## SOLID in Rust

See `code-quality.md` for universal SOLID definitions. Rust-specific applications:

| Principle | Rust Mechanism | Example |
|-----------|---------------|---------|
| **SRP** | One struct per concern; split `impl` blocks by role | `PackageManager` facade delegates to focused task methods |
| **OCP** | New `impl Trait` instead of new match arms | `IndexImpl` trait with `LocalIndex`/`RemoteIndex` |
| **LSP** | Every `impl Trait` honors documented contract | Trait impl must not panic where contract promises `Result` |
| **ISP** | Narrowest trait bounds: `impl Write` not `impl Read + Write + Seek` | Accept `&[T]` not `Vec<T>` when only reading |
| **DIP** | Depend on `impl Trait`/`dyn Trait`, not concrete types | Constructor takes `impl Client` not `HttpClient` |

---

## DRY in Rust

See `code-quality.md` for universal DRY rules. Rust-specific mechanisms:

- **Generics** (`<T: Trait>`): zero-cost DRY — same algorithm over multiple types
- **Derive macros**: eliminate boilerplate — `Debug, Clone, PartialEq, Serialize, Deserialize`
- **`macro_rules!`**: for structural duplication generics can't express (multiple types with different field names)
- **Extract trait**: only when 2+ genuinely different implementations exist

---

## YAGNI in Rust

See `code-quality.md` for universal YAGNI rules. Rust-specific applications:

- **Prefer `impl Trait` over `dyn Trait`** unless runtime polymorphism is genuinely required
- **Start with concrete types.** Extract a trait only when a second genuinely different implementation appears
- **Don't over-engineer error enums.** If callers only distinguish 2 cases, don't create a 20-variant enum
- **No premature generics.** A function that only handles `String` doesn't need `<T: AsRef<str>>` until called with something else

---

## Async Patterns (Tokio)

### Structured Concurrency
- **`JoinSet`** for bounded parallel work where you need all results. Drop aborts all tasks.
- **Always join** — never fire-and-forget spawned tasks. Observe `JoinHandle` or use `JoinSet`.
- **OCX convention**: `_all` methods use `JoinSet` but **preserve input order** — spawn with index, collect results, sort by index.
- **`spawn_blocking`** for sync I/O and CPU-bound work (>100μs between await points). Use `rayon` for heavy compute with `oneshot` channel bridge.

### Cancel Safety
- `recv()` is cancel-safe, `send()` is NOT — use `reserve().await` + `permit.send()` in `select!`
- Pin futures outside `select!` loops (`tokio::pin!`) to resume them; don't recreate each iteration
- `JoinSet::join_next()` is cancel-safe

### Async Anti-Patterns
- **NEVER** hold `std::sync::MutexGuard` across `.await` — extract data, drop guard, then await
- **NEVER** call `std::fs::*` or `std::thread::sleep` in async — use tokio equivalents
- **NEVER** `runtime.block_on()` from within a tokio thread (deadlock)
- **NEVER** `mpsc::unbounded_channel()` without explicit justification — always bounded for backpressure
- **NEVER** drop `JoinHandle` without observing result — panics silently disappear

### Error Handling in Async
- `JoinError`: distinguish panics (`.is_panic()`) from cancellation (`.is_cancelled()`)
- Re-panic: `std::panic::resume_unwind(e.into_panic())` to propagate task panics
- Fail fast: `set.abort_all()` on first error when appropriate

### OCX Async Conventions
- Progress: `tracing::info_span!` + `.instrument()` for JoinSet tasks, `.entered()` for sequential loops
- No custom progress abstraction — `tracing-indicatif` handles visualization
- Parallel `_all` methods: spawn into JoinSet with index, collect, sort by index to preserve input order
- Sequential `_all` methods (uninstall, deselect): iterate with entered guard, no JoinSet
- Semaphore-based concurrency limiting for fan-out work (e.g., parallel downloads)

---

## Project Pattern Consistency

New code must follow established OCX patterns. The reviewer enforces this:

| Pattern | Convention | Deviation = Bug |
|---------|-----------|----------------|
| Error model | `PackageErrorKind` → `PackageError` → `Error` | Ad-hoc error types |
| Progress | `tracing::info_span!` + `tracing-indicatif` | Custom progress bars, `println!` |
| Symlinks | `ReferenceManager::link(forward, content)` | Raw `symlink::update/create` |
| CLI flow | args → `transform_all()` → `manager.task_all()` → report from results → `api.report()` | Report from CLI args |
| API output | `Printable` trait, single `print_table()`, static headers, typed enum statuses | Multiple tables, dynamic headers, string statuses |
| Batch order | `_all()` preserves input order (JoinSet with index tracking) | Unordered results |
| Paths | `to_relaxed_slug()`, `repository_path()` splits on `/` | Manual path joining |
| Async I/O | `tokio::fs::*`, bounded channels, `spawn_blocking` for CPU | `std::fs::*`, unbounded channels |

**Consistency review questions:**
- "Does this follow the same pattern as similar existing code?"
- "Is there an established way to do this?" (grep before inventing)
- "Was this pattern reused or reinvented?"

---

## Reusability Assessment

See `code-quality.md` for universal reusability questions. OCX-specific signals of misplaced code:

- Platform-specific logic in a CLI command instead of the library
- Compression/archive handling added in one place but needed by others
- Env var parsing that doesn't use existing truthy-value logic

**OCX layer guide:**

| Logic type | Belongs in | Example |
|---|---|---|
| Generic utility | `ocx_lib` root or `utility/` | slug, symlink, file_lock |
| Domain operation | `ocx_lib/package_manager/tasks/` | find, install, clean |
| Progress/tracing | `ocx_lib/cli/` | progress spans, logging |
| Command-specific glue | `ocx_cli/command/` | arg parsing, report assembly |
| Output formatting | `ocx_cli/api/data/` | Printable types |

---

## Code Review Checklist (Rust-Specific)

See `code-quality.md` for the universal review checklist. Rust-specific additions:

- [ ] No `.unwrap()` in library code; no `MutexGuard` across `.await`; no blocking I/O in async
- [ ] Every `.clone()` intentional; prefer `&[T]`/`&str` params over owned types
- [ ] `Result` propagated via `?` with `From` impls; errors logged once at boundary
- [ ] Builder for 4+ optional fields; no boolean flags where enum is clearer
- [ ] JoinSet `_all` methods preserve input order; bounded channels; tasks observed
- [ ] Follows OCX conventions (error model, symlinks, CLI flow, progress — see table above)
- [ ] Generic logic in `ocx_lib`, command-specific in `ocx_cli`; patterns reused not reinvented
- [ ] `cargo clippy --workspace` passes; `duplo` for structural duplication (Rust-aware)
