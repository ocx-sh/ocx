---
paths:
  - "**/main.rs"
  - "**/exit_code.rs"
  - "**/*.rs"
---

# Rust CLI Exit Code Design

Shareable, project-independent guide to designing exit codes for Rust CLI tools. Auto-loads on `main.rs` or `exit_code.rs` edits; the `**/*.rs` glob is for search-by-name discovery across broad Rust work.

Complements [`quality-rust.md`](./quality-rust.md) and [`quality-rust-errors.md`](./quality-rust-errors.md) — the error-message rule and the exit-code taxonomy are co-design concerns.

---

## The Canonical Reference

**BSD `sysexits.h`** (codes 64–78) is the de-facto standard for CLI exit codes on Unix-family systems. Though formally deprecated as a C header for portability reasons, its numeric values remain canonical and the Rust CLI Book endorses them via the `exitcode`/`sysexits` crates.

Values 1 and 2 are shell-reserved (1 = generic error, 2 = misuse of builtins by Bash convention). Values 128+ are signal-derived (`128 + N` where N is the signal number). Using 64+ for semantic codes avoids collision with both.

---

## Design Principles

- **Own the enum** — define a `#[repr(u8)]` enum in the library crate's `cli` submodule (`<lib>::cli::ExitCode`) rather than depending on `sysexits` or `exitcode` crates. The values are stable POSIX conventions; owning them decouples your binaries from an external dependency.
- **Align with `sysexits.h`** — 64 for usage errors, 65 for data errors, 69 for unavailable services, 74 for I/O errors, 77 for permission, 78 for config. This is the established convention backend tools and shell scripts expect.
- **Reserve a private range above 78** — 79–127 is free (below shell-reserved 128+ and above `EX__MAX = 78`). Use it for tool-specific codes that sysexits doesn't cover (e.g., "auth failure", "offline-blocked").
- **`#[non_exhaustive]` required** — adding a variant must not be a semver break.
- **`From<ExitCode> for std::process::ExitCode`** — lets `main()` return the code directly without explicit casting at every call site.
- **One enum per workspace, shared by all binaries** — both the primary CLI and sibling tools (e.g., a mirror/publisher tool) consume the same enum. Prevents drift.

---

## Canonical Shape

```rust
/// Process exit codes used by all binaries in this workspace.
///
/// Numeric values align with BSD sysexits.h (EX__BASE = 64) to avoid collisions
/// with shell-reserved codes (1–2) and signal-derived codes (128+).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    /// Successful completion.
    Success = 0,
    /// Generic failure — use only when no specific code applies.
    Failure = 1,
    /// Bad CLI invocation: unknown flag, wrong argument count, invalid syntax.
    /// Mirrors `EX_USAGE` (64).
    UsageError = 64,
    /// Input data malformed: bad identifier format, invalid digest, corrupted manifest.
    /// Mirrors `EX_DATAERR` (65).
    DataError = 65,
    /// Required resource unavailable: network down, registry unreachable.
    /// Mirrors `EX_UNAVAILABLE` (69).
    Unavailable = 69,
    /// I/O error: filesystem permission denied, disk full, read/write failure.
    /// Mirrors `EX_IOERR` (74).
    IoError = 74,
    /// Temporary failure that may succeed on retry: rate limit, transient network.
    /// Mirrors `EX_TEMPFAIL` (75).
    TempFail = 75,
    /// Insufficient permissions: registry 403, filesystem `EPERM`.
    /// Mirrors `EX_NOPERM` (77).
    PermissionDenied = 77,
    /// Configuration error: bad config file, missing required field, parse failure.
    /// Mirrors `EX_CONFIG` (78).
    ConfigError = 78,
    /// Resource not found: package 404, explicit config path absent.
    /// Tool-specific; first slot above `EX_CONFIG`.
    NotFound = 79,
    /// Authentication failure: registry 401, missing credentials.
    /// Tool-specific.
    AuthError = 80,
    /// Offline mode blocked a network operation.
    /// Distinct from `Unavailable`: the failure is deliberate policy, not a fault.
    OfflineBlocked = 81,
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        std::process::ExitCode::from(code as u8)
    }
}
```

---

## Error → Exit Code Classification

Use a free function, not a trait method. Trait methods couple every error type to the exit-code taxonomy, creating a circular dependency (errors → `ExitCode` → `main.rs` → errors). A free function that walks the `anyhow::Error::chain()` and downcasts to each known subtree keeps the dependency direction clean.

```rust
pub fn classify_error(err: &anyhow::Error) -> ExitCode {
    for cause in err.chain() {
        // Match the outer library error type first (three-layer pattern).
        if let Some(e) = cause.downcast_ref::<MyLibError>() {
            return match e {
                MyLibError::OfflineMode        => ExitCode::OfflineBlocked,
                MyLibError::Io { .. }          => ExitCode::IoError,
                MyLibError::Config(ce)         => classify_config(ce),
                MyLibError::PackageManager(pe) => match pe.kind() {
                    PackageErrorKind::NotFound  => ExitCode::NotFound,
                    PackageErrorKind::Ambiguous => ExitCode::DataError,
                    _                           => ExitCode::Failure,
                },
                _ => continue,
            };
        }
        // Subsystem types that may surface standalone via `.context()`.
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::PermissionDenied
        {
            return ExitCode::PermissionDenied;
        }
    }
    ExitCode::Failure
}
```

**Three-layer error pattern.** If your library uses the `Error → PackageError → PackageErrorKind` pattern (outer enum, context-bearing middle struct, discriminant-only inner enum), the `classify_error` function downcasts the *outermost* `Error` first, then pattern-matches through to the inner `kind`. You cannot `downcast_ref::<PackageErrorKind>()` directly unless the kind was attached as its own `anyhow::context` — which is unusual.

**Default fall-through.** Any subtree not explicitly classified falls through to `ExitCode::Failure`. This is acceptable v1 behavior as long as a test locks in the fall-through so it cannot silently change later.

---

## Anti-Patterns

### Block

- **Single-digit numeric codes for semantic categories** (e.g., `exit 3` for "network error"). Collides with shell-reserved 1/2 and has no discoverable meaning.
- **Bash `exit $?` chains with magic numbers** inside the CLI itself — use the enum, not literals.
- **Different binaries in the same workspace using different exit-code taxonomies** — prevents shared error handling in CI scripts. One enum, shared.
- **Trait-based error-to-exit-code mapping on each error type** — creates circular dependency lib → cli → lib. Use a free function that walks the error chain.
- **`std::process::exit(N)` from inside library code** — libraries never exit; they return `Result`. Exits happen at `main.rs`.

### Warn

- **Hard-coded `ExitCode::from(N)` at call sites** — route through the typed enum so the numeric value is a single source of truth.
- **More than one canonical success code** (e.g., `0` for "installed" and `99` for "already installed"). Use `Success = 0` and communicate the "already installed" status via stdout or stderr, not exit code.
- **Missing `#[non_exhaustive]`** — adding a variant silently becomes a semver break.

### Suggest

- **`match` with a wildcard arm `_ => Failure`** — prefer exhaustive matches so new error variants compile-error until classified, then explicitly map the unclassified to `Failure` if that's the intended behavior. Locks in the choice.

---

## Wiring the Enum into `main()`

The end-state `main.rs` is short:

```rust
use <lib>::cli::{classify_error, ExitCode};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match app::run().await {
        Ok(code) => code.into(),
        Err(err) => {
            tracing::error!("{err:#}");
            classify_error(&err).into()
        }
    }
}
```

- `app::run()` returns `anyhow::Result<ExitCode>`.
- Success path: the app's own `ExitCode` (e.g., `ExitCode::Success` or `ExitCode::NotFound` for a "nothing matched" query).
- Error path: log the full chain with `{err:#}`, classify via the free function, return the numeric code.
- Never prefix the error log with `"Error: "` — the `tracing`/`log` level already categorizes the line.

---

## Scripts Consuming the Exit Codes

Scripts can `case $?` on the stable numeric values:

```sh
mytool install foo:1.0
case $? in
    0)  echo "installed" ;;
    64) echo "usage error; check flags" ;;
    69) echo "registry unreachable; retry with backoff" ;;
    78) echo "bad config; fix and retry" ;;
    79) echo "not found; pin a different version" ;;
    80) echo "auth failed; refresh credentials" ;;
    81) echo "offline mode; run online" ;;
    *)  echo "unknown failure ($?)"; exit 1 ;;
esac
```

This is the primary value proposition of the enum for backend/automation tools: programmatic failure discrimination without parsing stderr.

---

## Sources

- [FreeBSD `sysexits.h` manpage](https://man.freebsd.org/cgi/man.cgi?sysexits) — canonical numeric table
- [Rust CLI Book — Exit Codes](https://rust-cli.github.io/book/in-depth/exit-code.html) — endorses sysexits-aligned codes
- [`sysexits` crate](https://crates.io/crates/sysexits) — Rust enum with `Termination` impl; reference shape if you prefer a dependency over owning the enum
- [`std::process::ExitCode` docs](https://doc.rust-lang.org/stable/std/process/struct.ExitCode.html) — `From<u8>` contract
- [clig.dev — Exit Codes](https://clig.dev/#exit-codes) — "0 success, non-zero failure" (no numeric prescription; defers to tool conventions)
- [npm exit codes](https://docs.npmjs.com/cli/v10/using-npm/scripts#exit-codes) — example of semantic differentiation in practice
