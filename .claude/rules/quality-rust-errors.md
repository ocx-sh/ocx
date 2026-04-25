---
paths:
  - "**/error.rs"
  - "**/errors.rs"
  - "**/*.rs"
---

# Rust Error Design

Rust-specific error-design reference. Shareable, project-independent — crate/module references belong in subsystem rules, not here. Auto-loads with `quality-rust.md` on any `.rs` edit; narrower globs above for search-by-name discovery.

Complements "Anti-Patterns" section in [`quality-rust.md`](./quality-rust.md) with detailed rules for error messages, chains, boundaries.

---

## The Canonical Rule

From [Rust API Guidelines `C-GOOD-ERR`](https://rust-lang.github.io/api-guidelines/interoperability.html#error-types-are-meaningful-and-well-behaved-c-good-err) and [`std::error::Error` docs](https://doc.rust-lang.org/std/error/trait.Error.html):

> Error messages are concise lowercase sentences without trailing punctuation.

Canonical: `"invalid digit found in string"`. Not `"Invalid digit found in string."`.

**Acronyms and proper nouns keep canonical case.** Rule applies to first *English word*, not initialisms: `JSON`, `TOML`, `HTTP`, `URL`, `I/O`, `SHA-256`, `TLS`, `CI` unchanged. `"I/O error for {path}"` compliant; `"io error for {path}"` not.

---

## Library vs CLI Boundary

Error messages live at two layers:

| Layer | Style | Rationale |
|---|---|---|
| Library (`thiserror` variants, `#[error("...")]`) | lowercase, no period, concise | Composes into `Display` chains via `source()`. Mixed-case chains read wrong: `"failed to install: Registry authentication failed."` |
| CLI binary (`anyhow::Context` strings) | Sentence-case acceptable | Terminal boundary; user reads outer string directly |

When binary prints errors with `anyhow::Error`'s `{:#}` alternate format, CLI context string (sentence-case) prefixes lowercase lib chain cleanly:

```
Context("Running install for cmake:3.28")
  → lib error "registry authentication failed"
     → lib error "invalid digit found in header"
```

Prints as:
```
Running install for cmake:3.28: registry authentication failed: invalid digit found in header
```

Never inline "Error:" / "error:" prefix at log site — `log::error!` / `tracing::error!` already categorize line.

---

## Block-tier Violations (must fix before merge)

- **`.to_string()` in `map_err()` erasing source errors**: `map_err(|e| MyError::X(e.to_string()))` destroys source chain. Use `#[source]` on structured field carrying inner error, or `Box<dyn Error + Send + Sync>`.
- **`String` wrapping structured error's Display output**: if field holds `error.to_string()`, should hold error itself.
- **Sentence-case or trailing-punctuation `#[error("...")]` strings** in library crates: violates `C-GOOD-ERR`, reads inconsistent in `{:#}` chains.
- **`"Error:" / "error:"` prefix in `#[error("...")]` strings**: `Error` trait itself represents error category; prefix redundant, breaks chain readability.
- **Missing `#[source]` on wrapping error variants**: every variant wrapping inner error must return it via `source()`. Without it, chain walking breaks for logging, diagnostics, downcasting.
- **`anyhow::Error` in library APIs**: libraries use `thiserror` for structured errors; `anyhow::Error` is binary/application-layer convenience, destroys `match`-ability for downstream callers.

## Warn-tier Violations (should fix)

- **Missing `#[non_exhaustive]` on public error enums**: adding variant becomes semver break without it.
- **Error types without `#[derive(thiserror::Error)]`**: manual `Display` impls OK only when format logic too complex for `#[error(...)]`; new types default to thiserror.
- **Bare re-raise without context**: `?` propagates, but adding `anyhow::Context` string at each semantic boundary in binary helps debugging.

---

## Structured Error Chain Pattern

Three-layer pattern for per-object error diagnosis in batch ops:

```rust
pub enum Error {
    #[error("{0}")]
    PackageManager(PackageError),
    // ... other top-level variants
}

pub struct PackageError {
    pub identifier: Identifier,
    pub kind: PackageErrorKind,
}

impl std::error::Error for PackageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.identifier, self.kind)
    }
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum PackageErrorKind {
    #[error("package not found")]
    NotFound,
    #[error("ambiguous selection: {candidates:?}")]
    Ambiguous { candidates: Vec<String> },
    // ...
}
```

Outer struct attaches per-object context (identifier); inner enum carries discriminant kind. Chain walking via `source()` surfaces inner kind for programmatic dispatch (e.g., exit-code classification — see [`quality-rust-exit_codes.md`](./quality-rust-exit_codes.md)).

---

## `thiserror` Conventions

- `#[derive(thiserror::Error, Debug)]` on every library error type.
- `#[error("...")]` messages follow lowercase/no-period rule.
- `#[source]` on wrapping variants. `#[from]` when conversion unambiguous and infallible.
- `#[error(transparent)]` when variant is pure pass-through to single inner error — don't add prefix when nothing to add.
- Library public API: always `#[non_exhaustive]`.
- One error enum per module. Avoid single workspace-wide god enum; each subsystem owns its taxonomy, composes via `#[from]`.

---

## `anyhow` Conventions

- `anyhow` belongs in binaries (`main.rs` and immediate call sites), not libraries.
- Use `.context("…")` / `.with_context(|| …)` at semantic boundaries, not every `?` site.
- Sentence-case context strings OK at CLI boundary where user reads them.
- Print errors with `{err:#}` (alternate format) to walk full `source()` chain — not `{err}` which only shows top message.
- Do NOT inline `"Error: "` prefix when logging; `log::error!` / `tracing::error!` already signal level.

---

## Normalization Examples

| Non-compliant | Compliant |
|---|---|
| `"Invalid manifest: {0}"` | `"invalid manifest: {0}"` |
| `"Failed to read config file {path}"` | `"failed to read config file {path}"` |
| `"Registry authentication failed: {0}"` | `"registry authentication failed: {0}"` |
| `"A network operation was attempted while in offline mode."` | `"network operation attempted in offline mode"` |
| `"JSON serialization error: {0}"` | `"JSON serialization error: {0}"` (compliant — `JSON` acronym) |
| `"I/O error for '{path}': {source}"` | `"I/O error for '{path}': {source}"` (compliant — `I/O` acronym) |
| `"CI environment variable is not set. Is this running inside CI?"` | `"CI environment variable is not set; is this running inside CI?"` (trailing `?` removed by joining sentences; `CI` stays canonical) |

---

## Sources

- [Rust API Guidelines — `C-GOOD-ERR`](https://rust-lang.github.io/api-guidelines/interoperability.html#error-types-are-meaningful-and-well-behaved-c-good-err) — canonical rule with examples
- [`std::error::Error` trait docs](https://doc.rust-lang.org/std/error/trait.Error.html) — reinforces lowercase/no-period convention
- [`thiserror` docs](https://docs.rs/thiserror/latest/thiserror/) — attribute reference + examples
- [`anyhow` docs](https://docs.rs/anyhow/latest/anyhow/) — Context pattern for CLI layers
- [cargo source — `util/context/mod.rs`](https://github.com/rust-lang/cargo/blob/master/src/cargo/util/context/mod.rs) — production-scale reference for lowercase error messages
- [jj source — `cli/src/ui.rs`](https://github.com/martinvonz/jj/blob/main/cli/src/ui.rs) — shows library/CLI boundary split (lib lowercase, UI sentence-case render)