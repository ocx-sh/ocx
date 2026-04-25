---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
  - "**/Cargo.lock"
---

# Rust Code Quality

Rust quality reference. Universal design (SOLID, DRY, YAGNI, anti-pattern severity, reusability, review checklist) in `quality-core.md` — this file = **Rust-specific** plus Tokio async + Rust 2024 edition.

Project-independent, shareable. Project-specific types/modules → subsystem rules.

**Sibling rules (deep dives):**

- [`quality-rust-errors.md`](./quality-rust-errors.md) — Rust error design: API Guidelines message rule, `thiserror`/`anyhow` conventions, three-layer chain patterns, library vs CLI boundary styles.
- [`quality-rust-exit_codes.md`](./quality-rust-exit_codes.md) — `ExitCode` enum shape for Rust CLI, sysexits.h alignment, error-to-exit-code classification, anti-patterns.

---

## Design Patterns

### Builder Pattern
Consuming builder (`self` not `&mut self`) for structs with 4+ optional fields. Return `Self` from setter for chaining. Required fields → typestate builder where `build()` only on state with all required set — missing = compile error.

### Newtype Pattern
Wrap primitives in single-field tuple structs for type safety. Zero runtime cost. Use for: invariants (`NonEmptyString`), type safety (`Digest` wrap hash string), bypass orphan rule. Always impl `Display`, `Debug`, `From`.

### RAII Guards
Acquire in constructor, release in `Drop`. File locks, temp dirs, lease borrows = natural RAII. Guard holds resource, drop = cleanup.

### Strategy via Traits
Behavior as trait, inject impl. Prefer static dispatch (`impl Trait` / `<T: Trait>`) for zero-cost monomorphization. `dyn Trait` only for runtime polymorphism (heterogeneous collections, plugins).

### Typestate Pattern
Encode valid states as distinct types. Transitions consume `self`, return new type → invalid transitions = compile error. Zero runtime cost via `PhantomData`. Use when protocol correctness matters (connection states, build phases).

### Version Enum via `serde_repr`
Versioned on-disk formats: encode versions as `#[repr(u8)]` enum with `serde_repr`. Deserialization rejects unknown automatically — no manual check. Beats raw `u32` field with `CURRENT_VERSION` constant + manual validation.

```rust
#[derive(Serialize_repr, Deserialize_repr, PartialEq)]
#[repr(u8)]
pub enum Version { V1 = 1 }
```

---

## Anti-Patterns (Tiered Severity)

### Block (must fix before merge)

- **`.unwrap()` / `.expect()` in library code** — panics cross API boundaries without caller consent. `clippy::unwrap_used` and `clippy::expect_used` in restriction group; enable both as `warn` in `[lints.rust]` for lib crates. `.expect("reason")` OK for invariants proven at compile time or by preceding logic (regex group guaranteed capture, length checked before `.next()`). Fallible ops → return `Result`. Tests may use `.unwrap()`.
- **`anyhow` / erased errors in library APIs** — `anyhow::Error` kills downstream `match`. Rule: libs use `thiserror`, binaries use `anyhow`. Both fine; mixing roles not. See [`quality-rust-errors.md`](./quality-rust-errors.md) for full boundary rule.
- **Sentence-case / trailing-punctuation `#[error("...")]` strings** — violates Rust API Guidelines `C-GOOD-ERR`, reads inconsistent in chained output. Full rule + normalization in [`quality-rust-errors.md`](./quality-rust-errors.md).
- **Magic numeric exit codes or `std::process::exit(N)` with bare literals** — CLI binaries own typed `ExitCode` enum aligned with `sysexits.h`. See [`quality-rust-exit_codes.md`](./quality-rust-exit_codes.md) for shape + classification.
- **Silent error swallowing** — `let _ = result` or `.ok()` without comment explaining why ignored.
- **`.to_string()` in `map_err()` erasing source errors** — never `map_err(|e| SomeError(e.to_string()))`. Carry source structurally via `#[source]` or `Box<dyn Error + Send + Sync>`.
- **`String` wrapping structured error's Display output** — if field holds `error.to_string()`, hold error itself.
- **`MutexGuard` across `.await`** — extract data, drop guard, then await. Deadlock if `Send`, compile error if not. `tokio::sync::Mutex` only when lock genuinely spans await.
- **`unsafe` without safety comment** — every `unsafe` block must document invariant in `// SAFETY:` comment.
- **Blocking I/O in async** — never `std::fs::*`, `std::net::*`, `std::thread::sleep`, or any blocking stdlib in async. Use `tokio::fs::*`, `tokio::time::sleep`, or `spawn_blocking`.
- **`From` impl hiding `.unwrap()`** — `From` must be infallible. `TryFrom` for fallible. Violates `?` operator contract.
- **`Box<dyn Error>` as function error return in lib code** — loses type info; use concrete enum via `thiserror`.
- **`clippy::correctness` group violations** — deny-by-default for reason; signal wrong code. Never suppress without comment.
- **`todo!()` / `unimplemented!()` in production paths** — OK in stub phases, block-tier if reachable in released build.
- **RPIT without `use<..>` bounds in public APIs (edition 2024)** — Rust 2024 implicitly captures all in-scope lifetimes in `impl Trait` returns. Public lib functions: add explicit `use<'a, T>` bounds to lock capture set, prevent API breakage on edition upgrade.

### Warn (should fix)

- **`pub(crate)` / `pub(super)` as design smell** — control visibility through module nesting, not path qualifiers. Use `mod` (private) vs `pub mod` on declaration to gate. Items inside: `pub` (visible to whoever sees module) or private. If you want `pub(crate)`/`pub(super)`, reconsider hierarchy.
- **Error types without `#[derive(thiserror::Error)]`** — all error types should use thiserror for `Display`/`source()` (manual `Display` OK when format too complex for `#[error]`).
- **Public error enums without `#[non_exhaustive]`** — adding variant = semver-breaking without it.
- **Missing `#[source]` on inner error fields** — wrapping variant must return inner from `source()`. Without it, chain walking breaks for logging/diagnostics.
- **Unnecessary `.clone()`** — clone to silence borrow checker masks design problem. Restructure ownership, pass refs, or use indices.
- **`Box<dyn Trait>` where `impl Trait` suffices** — vtable + heap overhead. Single impl or compile-time-known set → use generics.
- **`PathBuf` parameter where `&Path` suffices** — accept `impl AsRef<Path>` at API boundaries, `&Path` internally.
- **`String` parameter where `&str` / `impl AsRef<str>` suffices** — forces alloc at every call site. Lint: `clippy::needless_pass_by_value`.
- **Stringly-typed APIs** — `String` where enum prevents typos at compile time. Includes errors: `String` errors block programmatic matching.
- **Boolean parameters** — `fn sort(ascending: bool)` less clear than `fn sort(order: SortOrder)`. Enums for two-state flags.
- **Missing `From`/`Into`** — if callers often write `String::from(x)` or `.into()`, add `impl From<T>`. `?` operator needs `From`.
- **Unbounded channels** — `mpsc::channel()` (unbounded) = latent OOM. Prefer `mpsc::channel(N)` with documented bound.
- **God structs** — 15+ fields spanning unrelated concerns. Decompose.

### Suggest (improvement)

- **`Cow<'_, str>`** for functions usually returning borrowed but sometimes allocating. Common in serialisation, path normalisation.
- **`#[must_use]`** on returns callers might discard.
- **Iterator chains** over materializing intermediate `Vec` — `.iter().map().filter().collect()` not building `Vec` then iterating.
- **`impl Into<T>` parameters** — `fn process(name: impl Into<String>)` accepts both `&str` and `String` without forcing alloc.
- **Early returns over nesting** — prefer `if condition { continue; }` or `if condition { return; }` to cut indent. Flatten `if !x { ... }` by inverting.
- **`clippy::pedantic` cherry-picks** — don't enable whole group, pick: `clippy::semicolon_if_nothing_returned`, `clippy::match_wildcard_for_single_variants`, `clippy::inefficient_to_string`.

---

## SOLID in Rust

See `quality-core.md` for universal SOLID. Rust mechanisms:

| Principle | Rust Mechanism |
|-----------|---------------|
| **SRP** | One struct per concern; split `impl` blocks by role |
| **OCP** | New `impl Trait` instead of new match arms |
| **LSP** | Every trait `impl` honors documented contract — no `panic!` where trait promises `Result` |
| **ISP** | Narrowest bounds: `impl Write` not `impl Read + Write + Seek`; `&[T]` not `Vec<T>` when reading |
| **DIP** | Depend on `impl Trait` / `dyn Trait`, not concrete; constructor takes `impl Client` not `HttpClient` |

---

## DRY in Rust

See `quality-core.md` for universal DRY. Rust mechanisms:

- **Generics** (`<T: Trait>`): zero-cost DRY — same algorithm, multiple types
- **Derive macros**: kill boilerplate — `Debug, Clone, PartialEq, Serialize, Deserialize`
- **`macro_rules!`**: structural duplication generics can't express (multiple types, different field names)
- **Extract trait**: only when 2+ genuinely different impls exist

---

## YAGNI in Rust

See `quality-core.md` for universal YAGNI. Rust applications:

- **Prefer `impl Trait` over `dyn Trait`** unless runtime polymorphism truly needed
- **Start concrete.** Extract trait only when second different impl appears
- **Don't over-engineer error enums.** Callers distinguish 2 cases → don't make 20-variant enum
- **No premature generics.** Function only handles `String` → no `<T: AsRef<str>>` until called with something else

---

## Async Patterns (Tokio)

### Structured Concurrency
- **`JoinSet`** for bounded parallel work needing all results. Drop aborts all.
- **Always join** — never fire-and-forget spawned tasks. Observe `JoinHandle` or use `JoinSet`.
- **Deterministic output**: `JoinSet::join_next()` returns in **completion order**, non-deterministic. Every `JoinSet` consumer **must** ensure deterministic output — sort by stable key (path, index, ID) before returning. No exceptions.
- **Preserving input order** in parallel batch: spawn with index, collect, sort by index. Standard Tokio pattern for order-preserving parallel work.
- **`.expect()` on `JoinHandle` / `join_next()`**: OK for swallowing task panics at join boundary — message describes panicking context. But **inner `Result`** from task always propagated via `?` — never silently dropped.
- **`spawn_blocking`** for sync I/O and CPU-bound (>100μs between awaits). Use `rayon` for heavy compute with `oneshot` channel bridge.
- **`spawn_blocking` result must be awaited** — `JoinHandle` awaited or blocking thread panic silently dropped.

### Cancel Safety
- `recv()` cancel-safe, `send()` NOT — use `reserve().await` + `permit.send()` in `select!`
- Pin futures outside `select!` loops (`tokio::pin!`) to resume; don't recreate each iteration
- `JoinSet::join_next()` cancel-safe

### Async Anti-Patterns
- **NEVER** hold `std::sync::MutexGuard` across `.await` — extract, drop, then await
- **NEVER** `std::fs::*`, `std::net::*`, or `std::thread::sleep` in async — use tokio
- **NEVER** `runtime.block_on()` from tokio thread (deadlock)
- **NEVER** `mpsc::unbounded_channel()` without justification — bounded for backpressure
- **NEVER** drop `JoinHandle` without observing — panics silently disappear

### Error Handling in Async
- `JoinError`: distinguish panics (`.is_panic()`) from cancellation (`.is_cancelled()`)
- Re-panic: `std::panic::resume_unwind(e.into_panic())` to propagate task panics
- Fail fast: `set.abort_all()` on first error when right

### Async I/O Conventions
- **Tokio I/O**: `tokio::fs::*` not `std::fs::*`; `tokio::net::*` not `std::net::*`
- **Channels**: bounded by default (`mpsc::channel(N)`), unbounded only with justification
- **Heavy/sync workloads**: `spawn_blocking` for sync I/O, `rayon` for CPU-bound

---

## Testing Conventions

- **Test-only methods**: prefer separate `#[cfg(test)] impl Foo { ... }` block before `mod tests`, not scattered `#[cfg(test)]` on individual methods mixed into production `impl`. Keeps production surface clear, makes test scaffolding explicit.

---

## Refactoring Tooling (Rust-Specific)

LSP tool available → use rust-analyzer for symbol ops — `findReferences`, `goToDefinition`, `workspaceSymbol` give semantically precise results. See `quality-core.md` for general principle.

---

## Code Review Checklist (Rust-Specific)

See `quality-core.md` for universal checklist. Rust additions:

- [ ] No `.unwrap()` in library code; no `MutexGuard` across `.await`; no blocking I/O in async
- [ ] `thiserror` in libs, `anyhow` only in binaries — no `anyhow` in library APIs
- [ ] Every `.clone()` intentional; prefer `&[T]`/`&str`/`&Path` over owned
- [ ] `Result` propagated via `?` with `From` impls; errors logged once at boundary
- [ ] `#[non_exhaustive]` on public enums; `#[source]` on wrapping error variants
- [ ] Builder for 4+ optional fields; no boolean flags where enum clearer
- [ ] `JoinSet` consumers sort results by stable key; `spawn_blocking` handles awaited
- [ ] Bounded channels; tasks observed; no `MutexGuard` across `.await`
- [ ] Public APIs use `use<..>` bounds for RPIT in edition 2024
- [ ] `cargo clippy --workspace` passes; `clippy::correctness` never suppressed

---

## 2026 Update Notes

- **Edition 2024 stable.** Migrate with `cargo fix --edition`. Key impact: RPIT now captures all lifetimes — `Captures` trick and outlives trick = dead weight, remove. Reserve `gen` identifier (keyword even without stable generators).
- **`thiserror` 2.x** = current line. New projects pin `>=2`. `#[error(transparent)]` improvements + better `source` chaining worth upgrade.
- **`clippy::pedantic` cherry-picking** — don't enable whole group, pick: `clippy::semicolon_if_nothing_returned`, `clippy::match_wildcard_for_single_variants`, `clippy::inefficient_to_string`.
- **`snafu`** — subsystems with many error sites needing rich context (file paths, HTTP status codes), `snafu`'s context selector pattern gaining adoption over `thiserror` + manual `map_err`. Worth evaluating for large internal subsystems, not blanket replacement.

---

## Comment Quality

Comments communicate what code cannot. Code = *what*; comments = *why*.

### The Ousterhout Test

> "If someone unfamiliar with the code could write your comment just by reading the code, it adds no value."

Before adding comment, three substitution tests: (1) Better name eliminate need? (2) Extraction into named function eliminate need? (3) Type (enum, newtype) eliminate need? Add comment only if all three fail.

### Two-Register Model

| Register | Syntax | Audience | Content |
|----------|--------|----------|---------|
| **Doc comments** | `///` / `//!` | API consumers (rustdoc) | Contract: what it does, when fails, invariants |
| **Inline comments** | `//` | Maintainers reading impl | Rationale: why this approach, non-obvious constraints |

Never mix: don't explain impl mechanics in `///`, don't explain API contracts in `//`.

### Block-tier (must fix)

- **Commented-out code** — delete; version control preserves history
- **`unsafe` without `// SAFETY:`** — required above, re-confirmed

### Warn-tier (should fix)

- **Narration comments** — comments restating next line (`// Create a new vector` above `let v = Vec::new()`)
- **Tautological doc comments** — `///` restating function name without info (`/// Returns the path` on `fn path()`)
- **Closing brace comments** — `} // end if`, `} // end for`

### Positive Requirements

- Public items: `///` summary adding info beyond name
- Functions returning `Result`: `# Errors` section
- Modules: `//!` inner doc comment at top
- `unsafe` blocks: `// SAFETY:` explaining relied-upon invariant

### Patterns to Preserve

- `// ── Section ──` dividers in long files (aids scanning)
- Phase/step comments in multi-step orchestration (`// Phase 1:`, `// Step 1:`)
- Parenthetical qualifications explaining *why* (`// tolerate failure (stale ref or GC'd object)`)
- Issue references (`// NOTE: issue #23`)
- Comments explaining non-obvious constraints or "why this looks wrong but is correct"
- External references (RFCs, specs, algorithm citations)

---

## Sources

Authoritative references:

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/about.html)
- [Rust 2024 Edition Guide — RPIT Lifetime Capture](https://doc.rust-lang.org/edition-guide/rust-2024/rpit-lifetime-capture.html)
- [Clippy Lints Reference](https://rust-lang.github.io/rust-clippy/master/index.html)
- [Tokio JoinSet docs](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html)
- [Tokio shared-state tutorial](https://tokio.rs/tokio/tutorial/shared-state)
- [Effective Rust — Item 22: Minimize visibility](https://effective-rust.com/visibility.html)
- [Effective Rust — Item 29: Listen to Clippy](https://effective-rust.com/clippy.html)