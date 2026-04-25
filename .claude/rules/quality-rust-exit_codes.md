---
paths:
  - "**/main.rs"
  - "**/exit_code.rs"
  - "**/*.rs"
---

# Rust CLI Exit Code Design

Shareable, project-independent guide for Rust CLI exit codes. Auto-loads on `main.rs` or `exit_code.rs` edits; `**/*.rs` glob for search-by-name discovery across broad Rust work.

Complements [`quality-rust.md`](./quality-rust.md) and [`quality-rust-errors.md`](./quality-rust-errors.md) — error-message rule and exit-code taxonomy co-design.

---

## The Canonical Reference

**BSD `sysexits.h`** (codes 64–78) = de-facto standard for CLI exit codes on Unix. Formally deprecated as C header for portability, but numeric values stay canonical. Rust CLI Book endorses via `exitcode`/`sysexits` crates.

Values 1, 2 shell-reserved (1 = generic error, 2 = Bash builtin misuse). 128+ signal-derived (`128 + N` where N = signal number). 64+ for semantic codes avoids both collisions.

---

## Design Principles

- **Own the enum** — define `#[repr(u8)]` enum in library crate's `cli` submodule (`<lib>::cli::ExitCode`) instead of depending on `sysexits` or `exitcode` crates. Values = stable POSIX conventions; ownership decouples binaries from external dep.
- **Align with `sysexits.h`** — 64 usage, 65 data, 69 unavailable, 74 I/O, 77 permission, 78 config. Convention backend tools and shell scripts expect.
- **Reserve private range above 78** — 79–127 free (below shell-reserved 128+, above `EX__MAX = 78`). Use for tool-specific codes sysexits skip (e.g., "auth failure", "offline-blocked").
- **`#[non_exhaustive]` required** — adding variant must not break semver.
- **`From<ExitCode> for std::process::ExitCode`** — lets `main()` return code directly, no explicit cast at call sites.
- **One enum per workspace, shared by all binaries** — primary CLI and sibling tools (e.g., mirror/publisher) consume same enum. Prevents drift.

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

Use free function, not trait method. Trait methods couple every error type to exit-code taxonomy → circular dep (errors → `ExitCode` → `main.rs` → errors). Free function walks `anyhow::Error::chain()` and downcasts each known subtree, keeping dep direction clean.

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

**Three-layer error pattern.** If library uses `Error → PackageError → PackageErrorKind` pattern (outer enum, context-bearing middle struct, discriminant-only inner enum), `classify_error` downcasts *outermost* `Error` first, then pattern-matches to inner `kind`. Cannot `downcast_ref::<PackageErrorKind>()` directly unless kind attached as own `anyhow::context` — unusual.

**Default fall-through.** Any subtree not classified falls through to `ExitCode::Failure`. Acceptable v1 behavior if test locks in fall-through so it cannot silently change later.

---

## Anti-Patterns

### Block

- **Single-digit numeric codes for semantic categories** (e.g., `exit 3` for "network error"). Collides with shell-reserved 1/2, no discoverable meaning.
- **Bash `exit $?` chains with magic numbers** inside the CLI itself — use enum, not literals.
- **Different binaries in same workspace using different exit-code taxonomies** — blocks shared error handling in CI scripts. One enum, shared.
- **Trait-based error-to-exit-code mapping per error type** — circular dep lib → cli → lib. Use free function walking error chain.
- **`std::process::exit(N)` from inside library code** — libraries never exit; return `Result`. Exits at `main.rs`.

### Warn

- **Hard-coded `ExitCode::from(N)` at call sites** — route through typed enum so numeric value = single source of truth.
- **More than one canonical success code** (e.g., `0` for "installed", `99` for "already installed"). Use `Success = 0`; communicate "already installed" via stdout/stderr, not exit code.
- **Missing `#[non_exhaustive]`** — adding variant silently breaks semver.

### Suggest

- **`match` with wildcard arm `_ => Failure`** — prefer exhaustive matches so new error variants compile-error until classified, then explicitly map unclassified to `Failure` if intended. Locks choice.

---

## Wiring the Enum into `main()`

End-state `main.rs` short:

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
- Success path: app's own `ExitCode` (e.g., `ExitCode::Success` or `ExitCode::NotFound` for "nothing matched" query).
- Error path: log full chain with `{err:#}`, classify via free function, return numeric code.
- Never prefix error log with `"Error: "` — `tracing`/`log` level already categorizes line.

---

## Scripts Consuming the Exit Codes

Scripts `case $?` on stable numeric values:

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

Primary value of enum for backend/automation tools: programmatic failure discrimination without parsing stderr.

---

## Sources

- [FreeBSD `sysexits.h` manpage](https://man.freebsd.org/cgi/man.cgi?sysexits) — canonical numeric table
- [Rust CLI Book — Exit Codes](https://rust-cli.github.io/book/in-depth/exit-code.html) — endorses sysexits-aligned codes
- [`sysexits` crate](https://crates.io/crates/sysexits) — Rust enum with `Termination` impl; reference shape if prefer dep over owning enum
- [`std::process::ExitCode` docs](https://doc.rust-lang.org/stable/std/process/struct.ExitCode.html) — `From<u8>` contract
- [clig.dev — Exit Codes](https://clig.dev/#exit-codes) — "0 success, non-zero failure" (no numeric prescription; defers to tool conventions)
- [npm exit codes](https://docs.npmjs.com/cli/v10/using-npm/scripts#exit-codes) — semantic differentiation example in practice