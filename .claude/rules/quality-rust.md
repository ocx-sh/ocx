---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
  - "**/Cargo.lock"
---

# Rust Code Quality

Rust-specific quality reference. Universal design principles (SOLID, DRY, YAGNI,
anti-pattern severity, reusability, review checklist) live in `quality-core.md` —
this file covers **Rust-specific applications** plus Tokio async conventions and
Rust 2024 edition guidance.

Project-independent and shareable: references to project-specific types or
modules belong in subsystem rules, not here.

---

## Design Patterns

### Builder Pattern
Use a consuming builder (`self` not `&mut self`) for structs with 4+ optional fields. Return `Self` from each setter for method chaining. For required fields, consider a typestate builder where `build()` only exists on the state with all required fields set — missing fields become compile errors.

### Newtype Pattern
Wrap primitives in single-field tuple structs for type safety. Zero runtime cost. Use when: enforcing invariants (e.g., `NonEmptyString`), adding type safety (e.g., `Digest` wrapping a hash string), or bypassing the orphan rule. Always implement `Display`, `Debug`, `From` on newtypes.

### RAII Guards
Acquire resources in constructors, release in `Drop`. File locks, temp directories, and lease-style borrows are natural RAII candidates — the guard type holds the resource, and dropping it performs cleanup.

### Strategy via Traits
Define behavior as a trait, inject the implementation. Prefer static dispatch (`impl Trait` / `<T: Trait>`) for zero-cost monomorphization. Use `dyn Trait` only when you genuinely need runtime polymorphism (heterogeneous collections, plugin systems).

### Typestate Pattern
Encode valid states as distinct types. State transitions consume `self` and return a new type, making invalid transitions compile errors. Zero runtime overhead via `PhantomData`. Use when protocol correctness matters (connection states, build phases).

### Version Enum via `serde_repr`
For versioned on-disk formats, encode known versions as a `#[repr(u8)]` enum with `serde_repr`. Deserialization rejects unknown versions automatically — no manual version check needed. Prefer this over a raw `u32` field with a `CURRENT_VERSION` constant and manual validation.

```rust
#[derive(Serialize_repr, Deserialize_repr, PartialEq)]
#[repr(u8)]
pub enum Version { V1 = 1 }
```

---

## Anti-Patterns (Tiered Severity)

### Block (must fix before merge)

- **`.unwrap()` / `.expect()` in library code** — panics cross API boundaries without the caller's consent. `clippy::unwrap_used` and `clippy::expect_used` are in the restriction group; enable both as `warn` in `[lints.rust]` for lib crates. `.expect("reason")` is tolerable for invariants proven at compile time or by preceding logic (e.g., regex group guaranteed to capture, length checked before `.next()`). For fallible operations, return `Result`. Tests may use `.unwrap()`.
- **`anyhow` / erased errors in library APIs** — `anyhow::Error` destroys downstream `match`-ability. Rule: libraries use `thiserror`, binaries use `anyhow`. Using both is fine; mixing the roles is not.
- **Silent error swallowing** — `let _ = result` or `.ok()` without a comment explaining why the error is deliberately ignored.
- **`.to_string()` in `map_err()` erasing source errors** — never `map_err(|e| SomeError(e.to_string()))`. Carry the source error structurally via `#[source]` or `Box<dyn Error + Send + Sync>`.
- **`String` wrapping a structured error's Display output** — if a field holds `error.to_string()`, it should hold the error itself instead.
- **`MutexGuard` across `.await`** — extract data, drop guard, then await. Causes deadlock if `Send`, compile error if not. Use `tokio::sync::Mutex` only when the lock genuinely spans an await point.
- **`unsafe` without safety comment** — every `unsafe` block must document the invariant it relies on in a `// SAFETY:` comment.
- **Blocking I/O in async** — never `std::fs::*`, `std::net::*`, `std::thread::sleep`, or any blocking stdlib call in async context. Use `tokio::fs::*`, `tokio::time::sleep`, or `spawn_blocking`.
- **`From` impl hiding `.unwrap()`** — `From` must be infallible. Use `TryFrom` for fallible conversions. Violating this breaks the `?` operator's contract.
- **`Box<dyn Error>` as a function's error return type in lib code** — loses type information; use a concrete enum via `thiserror`.
- **`clippy::correctness` group violations** — these are deny-by-default for a reason; they signal outright wrong code. Never suppress without a comment.
- **`todo!()` / `unimplemented!()` in production paths** — acceptable in stub phases, but block-tier if they can be reached at runtime in a released build.
- **RPIT without `use<..>` bounds in public APIs (edition 2024)** — Rust 2024 implicitly captures all in-scope lifetimes in `impl Trait` return types. For public library functions, add explicit `use<'a, T>` bounds to document and lock the capture set, preventing API breakage when callers upgrade editions.

### Warn (should fix)

- **`pub(crate)` / `pub(super)` as a design smell** — control visibility through module nesting, not path qualifiers. Use `mod` (private) vs `pub mod` on the module declaration to gate what outsiders can see. Items inside should be either `pub` (visible to whoever can see the module) or private (module-internal). If you feel the need for `pub(crate)` or `pub(super)`, reconsider the module hierarchy first.
- **Error types without `#[derive(thiserror::Error)]`** — all error types should use thiserror for `Display`/`source()` generation (manual `Display` acceptable when format logic is too complex for `#[error]`).
- **Public error enums without `#[non_exhaustive]`** — adding a variant is a semver-breaking change without it.
- **Missing `#[source]` on inner error fields** — every wrapping variant must return its inner error from `source()`. Without it, error chain walking breaks for logging and diagnostics.
- **Unnecessary `.clone()`** — cloning to silence the borrow checker masks a design problem. Restructure ownership, pass references, or use indices instead.
- **`Box<dyn Trait>` where `impl Trait` suffices** — vtable + heap allocation overhead. If only one implementation exists or the set is known at compile time, use generics.
- **`PathBuf` parameter where `&Path` suffices** — accept `impl AsRef<Path>` at API boundaries, `&Path` internally.
- **`String` parameter where `&str` / `impl AsRef<str>` suffices** — forces unnecessary allocation at every call site. Lint: `clippy::needless_pass_by_value`.
- **Stringly-typed APIs** — `String` where an enum would prevent typos at compile time. Applies to error types too: `String` errors prevent programmatic matching.
- **Boolean parameters** — `fn sort(ascending: bool)` is less clear than `fn sort(order: SortOrder)`. Use enums for two-state flags.
- **Missing `From`/`Into`** — if callers frequently write `String::from(x)` or `.into()`, add `impl From<T>` on your type. The `?` operator relies on `From`.
- **Unbounded channels** — `mpsc::channel()` (unbounded) is a latent OOM. Prefer `mpsc::channel(N)` with a documented bound.
- **God structs** — 15+ fields spanning unrelated concerns. Decompose into focused structs.

### Suggest (improvement)

- **`Cow<'_, str>`** for functions that usually return borrowed data but sometimes need to allocate. Common in serialisation and path normalisation code.
- **`#[must_use]`** on return types callers might accidentally discard.
- **Iterator chains** over materializing intermediate `Vec` — `.iter().map().filter().collect()` instead of building a `Vec` then iterating it.
- **`impl Into<T>` parameters** — `fn process(name: impl Into<String>)` accepts both `&str` and `String` without forcing callers to allocate.
- **Early returns over nesting** — prefer `if condition { continue; }` or `if condition { return; }` to reduce indentation depth. Flatten `if !x { ... }` blocks by inverting the condition.
- **`clippy::pedantic` cherry-picks** — don't enable the whole group, but pick: `clippy::semicolon_if_nothing_returned`, `clippy::match_wildcard_for_single_variants`, `clippy::inefficient_to_string`.

---

## SOLID in Rust

See `quality-core.md` for universal SOLID definitions. Rust-specific mechanisms:

| Principle | Rust Mechanism |
|-----------|---------------|
| **SRP** | One struct per concern; split `impl` blocks by role |
| **OCP** | New `impl Trait` instead of new match arms |
| **LSP** | Every trait `impl` honors the documented contract — no `panic!` where the trait promises `Result` |
| **ISP** | Narrowest trait bounds: `impl Write` not `impl Read + Write + Seek`; accept `&[T]` not `Vec<T>` when only reading |
| **DIP** | Depend on `impl Trait` / `dyn Trait`, not concrete types; constructor takes `impl Client` not `HttpClient` |

---

## DRY in Rust

See `quality-core.md` for universal DRY rules. Rust-specific mechanisms:

- **Generics** (`<T: Trait>`): zero-cost DRY — same algorithm over multiple types
- **Derive macros**: eliminate boilerplate — `Debug, Clone, PartialEq, Serialize, Deserialize`
- **`macro_rules!`**: for structural duplication generics can't express (multiple types with different field names)
- **Extract trait**: only when 2+ genuinely different implementations exist

---

## YAGNI in Rust

See `quality-core.md` for universal YAGNI rules. Rust-specific applications:

- **Prefer `impl Trait` over `dyn Trait`** unless runtime polymorphism is genuinely required
- **Start with concrete types.** Extract a trait only when a second genuinely different implementation appears
- **Don't over-engineer error enums.** If callers only distinguish 2 cases, don't create a 20-variant enum
- **No premature generics.** A function that only handles `String` doesn't need `<T: AsRef<str>>` until called with something else

---

## Async Patterns (Tokio)

### Structured Concurrency
- **`JoinSet`** for bounded parallel work where you need all results. Drop aborts all tasks.
- **Always join** — never fire-and-forget spawned tasks. Observe `JoinHandle` or use `JoinSet`.
- **Deterministic output**: `JoinSet::join_next()` returns results in **completion order**, which is non-deterministic. Every `JoinSet` consumer **must** ensure deterministic output — sort results by a stable key (path, index, ID) before returning. No exceptions.
- **Preserving input order** in parallel batch methods: spawn with an index, collect results, sort by index to preserve input order. This is the standard Tokio pattern for order-preserving parallel work.
- **`.expect()` on `JoinHandle` / `join_next()`**: Acceptable for swallowing task panics at the join boundary — the `.expect()` message should describe the panicking context. However, the **inner `Result`** from the task must always be propagated via `?` — never silently dropped.
- **`spawn_blocking`** for sync I/O and CPU-bound work (>100μs between await points). Use `rayon` for heavy compute with `oneshot` channel bridge.
- **`spawn_blocking` result must be awaited** — the `JoinHandle` must be awaited or the blocking thread's panic is silently dropped.

### Cancel Safety
- `recv()` is cancel-safe, `send()` is NOT — use `reserve().await` + `permit.send()` in `select!`
- Pin futures outside `select!` loops (`tokio::pin!`) to resume them; don't recreate each iteration
- `JoinSet::join_next()` is cancel-safe

### Async Anti-Patterns
- **NEVER** hold `std::sync::MutexGuard` across `.await` — extract data, drop guard, then await
- **NEVER** call `std::fs::*`, `std::net::*`, or `std::thread::sleep` in async — use tokio equivalents
- **NEVER** `runtime.block_on()` from within a tokio thread (deadlock)
- **NEVER** `mpsc::unbounded_channel()` without explicit justification — always bounded for backpressure
- **NEVER** drop `JoinHandle` without observing result — panics silently disappear

### Error Handling in Async
- `JoinError`: distinguish panics (`.is_panic()`) from cancellation (`.is_cancelled()`)
- Re-panic: `std::panic::resume_unwind(e.into_panic())` to propagate task panics
- Fail fast: `set.abort_all()` on first error when appropriate

### Async I/O Conventions
- **Tokio I/O**: `tokio::fs::*` not `std::fs::*`; `tokio::net::*` not `std::net::*`
- **Channels**: bounded by default (`mpsc::channel(N)`), unbounded only with explicit justification
- **Heavy/sync workloads**: `spawn_blocking` for sync I/O, `rayon` for CPU-bound

---

## Testing Conventions

- **Test-only methods**: prefer a separate `#[cfg(test)] impl Foo { ... }` block before `mod tests`, not scattered `#[cfg(test)]` on individual methods mixed into the production `impl`. This keeps the production surface clear and makes test scaffolding explicit.

---

## Refactoring Tooling (Rust-Specific)

When the LSP tool is available, use rust-analyzer for symbol operations — `findReferences`, `goToDefinition`, `workspaceSymbol` give semantically precise results. See `quality-core.md` for the general principle.

---

## Code Review Checklist (Rust-Specific)

See `quality-core.md` for the universal review checklist. Rust-specific additions:

- [ ] No `.unwrap()` in library code; no `MutexGuard` across `.await`; no blocking I/O in async
- [ ] `thiserror` in libs, `anyhow` only in binaries — no `anyhow` in library APIs
- [ ] Every `.clone()` intentional; prefer `&[T]`/`&str`/`&Path` params over owned types
- [ ] `Result` propagated via `?` with `From` impls; errors logged once at boundary
- [ ] `#[non_exhaustive]` on public enums; `#[source]` on wrapping error variants
- [ ] Builder for 4+ optional fields; no boolean flags where enum is clearer
- [ ] `JoinSet` consumers sort results by stable key; `spawn_blocking` handles awaited
- [ ] Bounded channels; tasks observed; no `MutexGuard` across `.await`
- [ ] Public APIs use `use<..>` bounds for RPIT in edition 2024
- [ ] `cargo clippy --workspace` passes; `clippy::correctness` never suppressed

---

## 2026 Update Notes

- **Edition 2024 is stable.** Migrate with `cargo fix --edition`. Key impact: RPIT now captures all lifetimes — the `Captures` trick and outlives trick are dead weight, remove them. Reserve the `gen` identifier (it's a keyword even without stable generators).
- **`thiserror` 2.x** is the current line. New projects should pin `>=2`. `#[error(transparent)]` improvements and better `source` chaining are worth the upgrade.
- **`clippy::pedantic` cherry-picking** — don't enable the whole group, but pick: `clippy::semicolon_if_nothing_returned`, `clippy::match_wildcard_for_single_variants`, `clippy::inefficient_to_string`.
- **`snafu`** — for subsystems with many error sites requiring rich context (file paths, HTTP status codes), `snafu`'s context selector pattern is gaining adoption over `thiserror` + manual `map_err`. Worth evaluating for large internal subsystems, not a blanket replacement.

---

## Comment Quality

Comments communicate what the code cannot. The code communicates *what*; comments communicate *why*.

### The Ousterhout Test

> "If someone unfamiliar with the code could write your comment just by reading the code, it adds no value."

Before adding a comment, apply three substitution tests: (1) Can a better name eliminate the need? (2) Can extraction into a named function eliminate the need? (3) Can a type (enum, newtype) eliminate the need? Only add the comment if all three fail.

### Two-Register Model

| Register | Syntax | Audience | Content |
|----------|--------|----------|---------|
| **Doc comments** | `///` / `//!` | API consumers (rustdoc) | Contract: what it does, when it fails, invariants |
| **Inline comments** | `//` | Maintainers reading the implementation | Rationale: why this approach, non-obvious constraints |

Never mix: don't explain implementation mechanics in `///`, don't explain API contracts in `//`.

### Block-tier (must fix)

- **Commented-out code** — delete it; version control preserves history
- **`unsafe` without `// SAFETY:`** — already required above, re-confirmed

### Warn-tier (should fix)

- **Narration comments** — comments restating what the next line does (`// Create a new vector` above `let v = Vec::new()`)
- **Tautological doc comments** — `///` that restates the function name without adding information (`/// Returns the path` on `fn path()`)
- **Closing brace comments** — `} // end if`, `} // end for`

### Positive Requirements

- Public items: `///` with a summary that adds information beyond the name
- Functions returning `Result`: `# Errors` section
- Modules: `//!` inner doc comment at top
- `unsafe` blocks: `// SAFETY:` explaining the relied-upon invariant

### Patterns to Preserve

- `// ── Section ──` dividers in long files (aids scanning)
- Phase/step comments in multi-step orchestration (`// Phase 1:`, `// Step 1:`)
- Parenthetical qualifications explaining *why* (`// tolerate failure (stale ref or GC'd object)`)
- Issue references (`// NOTE: issue #23`)
- Comments explaining non-obvious constraints or "why this looks wrong but is correct"
- External references (RFCs, specs, algorithm citations)

---

## Sources

Authoritative references used in this rule:

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/about.html)
- [Rust 2024 Edition Guide — RPIT Lifetime Capture](https://doc.rust-lang.org/edition-guide/rust-2024/rpit-lifetime-capture.html)
- [Clippy Lints Reference](https://rust-lang.github.io/rust-clippy/master/index.html)
- [Tokio JoinSet docs](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html)
- [Tokio shared-state tutorial](https://tokio.rs/tokio/tutorial/shared-state)
- [Effective Rust — Item 22: Minimize visibility](https://effective-rust.com/visibility.html)
- [Effective Rust — Item 29: Listen to Clippy](https://effective-rust.com/clippy.html)
