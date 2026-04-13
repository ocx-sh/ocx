---
paths:
  - "**/error.rs"
  - "**/errors.rs"
  - "**/*.rs"
---

# Rust Error Design

Rust-specific error-design reference. Shareable and project-independent — references to specific crates or modules belong in subsystem rules, not here. Auto-loads alongside `quality-rust.md` on any `.rs` edit; the narrower globs above are for search-by-name discovery.

Complements the "Anti-Patterns" section in [`quality-rust.md`](./quality-rust.md) with the detailed rules for error messages, chains, and boundaries.

---

## The Canonical Rule

From the [Rust API Guidelines `C-GOOD-ERR`](https://rust-lang.github.io/api-guidelines/interoperability.html#error-types-are-meaningful-and-well-behaved-c-good-err) and the [`std::error::Error` docs](https://doc.rust-lang.org/std/error/trait.Error.html):

> Error messages are concise lowercase sentences without trailing punctuation.

Canonical example: `"invalid digit found in string"`. Not `"Invalid digit found in string."`.

**Acronyms and proper nouns retain canonical case.** The rule applies to the first *English word*, not to initialisms: `JSON`, `TOML`, `HTTP`, `URL`, `I/O`, `SHA-256`, `TLS`, `CI` are unchanged. `"I/O error for {path}"` is compliant; `"io error for {path}"` is not.

---

## Library vs CLI Boundary

Error messages live at two distinct layers:

| Layer | Style | Rationale |
|---|---|---|
| Library (`thiserror` variants, `#[error("...")]`) | lowercase, no period, concise | Composes into `Display` chains via `source()`. Mixed-case chains read wrong: `"failed to install: Registry authentication failed."` |
| CLI binary (`anyhow::Context` strings) | Sentence-case acceptable | Terminal boundary; user reads the outer string directly |

When the binary prints errors with `anyhow::Error`'s `{:#}` alternate format, the CLI context string (sentence-case) prefixes the lowercase lib chain cleanly:

```
Context("Running install for cmake:3.28")
  → lib error "registry authentication failed"
     → lib error "invalid digit found in header"
```

Prints as:
```
Running install for cmake:3.28: registry authentication failed: invalid digit found in header
```

Never inline the "Error:" / "error:" prefix at the log site — `log::error!` / `tracing::error!` already categorize the line.

---

## Block-tier Violations (must fix before merge)

- **`.to_string()` in `map_err()` erasing source errors**: `map_err(|e| MyError::X(e.to_string()))` destroys the source chain. Use `#[source]` on a structured field carrying the inner error, or `Box<dyn Error + Send + Sync>`.
- **`String` wrapping a structured error's Display output**: if a field holds `error.to_string()`, it should hold the error itself.
- **Sentence-case or trailing-punctuation `#[error("...")]` strings** in library crates: violates `C-GOOD-ERR`, reads inconsistent in `{:#}` chains.
- **`"Error:" / "error:"` prefix in `#[error("...")]` strings**: the `Error` trait itself represents the error category; the prefix is redundant and breaks chain readability.
- **Missing `#[source]` on wrapping error variants**: every variant that wraps an inner error must return it via `source()`. Without it, error chain walking breaks for logging, diagnostics, and downcasting.
- **`anyhow::Error` in library APIs**: libraries use `thiserror` for structured errors; `anyhow::Error` is a binary/application-layer convenience and destroys `match`-ability for downstream callers.

## Warn-tier Violations (should fix)

- **Missing `#[non_exhaustive]` on public error enums**: adding a variant becomes a semver break without it.
- **Error types without `#[derive(thiserror::Error)]`**: manual `Display` impls are acceptable only when the format logic is too complex for `#[error(...)]`, but new error types should default to thiserror.
- **Bare re-raise without adding context**: `?` propagates, but adding an `anyhow::Context` string at each semantic boundary in the binary helps debugging.

---

## Structured Error Chain Pattern

The three-layer pattern for per-object error diagnosis in batch operations:

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

The outer struct attaches per-object context (the identifier), the inner enum carries the discriminant kind. Chain walking via `source()` surfaces the inner kind for programmatic dispatch (e.g., exit-code classification — see [`quality-rust-exit_codes.md`](./quality-rust-exit_codes.md)).

---

## `thiserror` Conventions

- `#[derive(thiserror::Error, Debug)]` on every library error type.
- `#[error("...")]` messages follow the lowercase/no-period rule.
- `#[source]` on wrapping variants. `#[from]` when the conversion is unambiguous and infallible.
- `#[error(transparent)]` when the variant is a pure pass-through to a single inner error — don't add a prefix string when there's nothing to add.
- Library public API: always `#[non_exhaustive]`.
- One error enum per module. Avoid a single workspace-wide god enum; each subsystem owns its taxonomy and composes through `#[from]`.

---

## `anyhow` Conventions

- `anyhow` belongs in binaries (`main.rs` and its immediate call sites), not in libraries.
- Use `.context("…")` / `.with_context(|| …)` at semantic boundaries, not at every `?` site.
- Sentence-case context strings are acceptable at the CLI boundary where the user reads them.
- Print errors with `{err:#}` (alternate format) to walk the full `source()` chain — not `{err}` which only shows the top message.
- Do NOT inline an `"Error: "` prefix when logging; `log::error!` / `tracing::error!` already signal the level.

---

## Normalization Examples

| Non-compliant | Compliant |
|---|---|
| `"Invalid manifest: {0}"` | `"invalid manifest: {0}"` |
| `"Failed to read config file {path}"` | `"failed to read config file {path}"` |
| `"Registry authentication failed: {0}"` | `"registry authentication failed: {0}"` |
| `"A network operation was attempted while in offline mode."` | `"network operation attempted in offline mode"` |
| `"JSON serialization error: {0}"` | `"JSON serialization error: {0}"` (compliant — `JSON` is an acronym) |
| `"I/O error for '{path}': {source}"` | `"I/O error for '{path}': {source}"` (compliant — `I/O` is an acronym) |
| `"CI environment variable is not set. Is this running inside CI?"` | `"CI environment variable is not set; is this running inside CI?"` (trailing `?` removed by joining sentences; `CI` stays canonical) |

---

## Sources

- [Rust API Guidelines — `C-GOOD-ERR`](https://rust-lang.github.io/api-guidelines/interoperability.html#error-types-are-meaningful-and-well-behaved-c-good-err) — the canonical rule with examples
- [`std::error::Error` trait docs](https://doc.rust-lang.org/std/error/trait.Error.html) — reinforces lowercase/no-period convention
- [`thiserror` docs](https://docs.rs/thiserror/latest/thiserror/) — attribute reference + examples
- [`anyhow` docs](https://docs.rs/anyhow/latest/anyhow/) — Context pattern for CLI layers
- [cargo source — `util/context/mod.rs`](https://github.com/rust-lang/cargo/blob/master/src/cargo/util/context/mod.rs) — production-scale reference for lowercase error messages
- [jj source — `cli/src/ui.rs`](https://github.com/martinvonz/jj/blob/main/cli/src/ui.rs) — demonstrates the library/CLI boundary split (lib lowercase, UI sentence-case render)
