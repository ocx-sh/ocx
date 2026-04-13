# Research: CLI Exit Code Conventions (2026)

<!--
Research Artifact
Filename: artifacts/research_exit_codes.md
Scope: OCX exit-code taxonomy — canonical enum in `ocx_lib::cli` used by `ocx` and `ocx_mirror`
Related Plan: plan_config_review_cleanup.md
-->

## Status

- **Date:** 2026-04-13
- **Axis:** Technology + domain knowledge
- **Consumers:** `plan_config_review_cleanup.md`, future CLI work, any new OCX binary

## Direct Recommendation

OCX should own a `#[repr(u8)]` enum in `ocx_lib::cli::ExitCode` whose variant values align with **BSD `sysexits.h`** (codes 64–78) for semantic failures, plus a small private range 79–81 for OCX-specific categories (not-found, auth, offline-blocked). No external dependency — the values are stable POSIX conventions and owning the enum keeps both binaries decoupled from any third-party crate.

## Standards Survey

| Source | Prescription |
|---|---|
| [BSD `sysexits.h`][sysexits-bsd] | Codes 64–78. Formally deprecated as a C header for portability, but the numeric values remain canonical and scripts `case $?` on them |
| [Rust CLI Book — Exit Code][rust-cli-book] | Recommends `exitcode` crate (constants only) for sysexits-aligned codes |
| [`sysexits` crate][sysexits-crate] | Full enum, `Termination` impl, `#[repr(i32)]`, `no_std`, MSRV 1.85 — current idiomatic Rust choice if taking a dep |
| [clig.dev — Exit Codes][clig-dev] | "0 success, non-zero failure." No numeric prescription; defers to tool conventions |
| [`std::process::ExitCode`][std-exitcode] | Only `SUCCESS` and `FAILURE` predefined; `From<u8>` for custom codes |

## Real-World Tool Survey

| Tool | Exit code shape | Notes |
|---|---|---|
| cargo | 0, 101 only | No semantic differentiation; `101` is the panic convention |
| git | 0, 1, 128, 129+ | 128 = fatal "invocation itself broken"; 129+ = 128 + signal |
| npm | 0–7 | 1=generic, 2=missing, 3=permission, 4=network, 5=not found, 6=config, 7=dep conflict |
| curl | 1–99 | Every failure type gets a number; too much for most tools |
| docker | 125, 126, 127 + container code | 125=daemon, 126=not invocable, 127=not found |
| Helm, kubectl | 0, 1 | Widely criticized in GitOps tooling |
| uv (Astral) | 0, 1, 2 | Simple but coarse |
| jj (jujutsu) | 0, 1, 2 (usage), 255 | Distinguishes usage errors |

**Pattern the research landscape rewards:** backend-first tools distinguish failure modes with stable codes. OCX's target users (GitHub Actions, Bazel, Python scripts, devcontainer features) need programmatic discrimination of "auth failure, retry with new token" vs "package not found, abort."

## Why sysexits.h over npm-style 1–7

- **Reserved range hygiene** — codes 1 and 2 are shell-reserved (1 = generic error, 2 = misuse of shell builtins by convention from Bash). Using 64+ avoids collisions with shell-internal codes and signal-derived codes (128+).
- **Widely-known convention** — sysexits has been the POSIX reference since BSD 4.3. A Bash script author reading a `case $?` block expecting `78` to mean "config error" is looking at a 40-year-old convention.
- **Room to grow** — 64–127 leaves ~60 slots for future OCX-specific codes without collision.
- **Documented rationale in Rust CLI book** — when two idiomatic Rust references converge on the same recommendation, that's the consensus.

## Proposed Enum Shape

```rust
//! crates/ocx_lib/src/cli/exit_code.rs

/// Process exit codes used by all OCX binaries.
///
/// Numeric values align with BSD sysexits.h (EX__BASE = 64) to avoid collisions
/// with shell-reserved codes (1–2) and signal-derived codes (128+). Scripts can
/// `case $?` on these values for structured error handling.
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
    /// Input data malformed: bad identifier format, invalid digest.
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
    /// Configuration error: bad `config.toml`, missing required field, parse failure.
    /// Mirrors `EX_CONFIG` (78).
    ConfigError = 78,
    /// Resource not found: package 404, explicit config path absent.
    /// OCX-specific; first slot above `EX_CONFIG`.
    NotFound = 79,
    /// Authentication failure: registry 401, missing credentials.
    /// OCX-specific; reserved 80.
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

`#[non_exhaustive]` is required so adding a variant in v2 is not a semver break.

## Error → Exit Code Mapping (OCX)

| OCX error category | Source type | Exit code |
|---|---|---|
| TOML parse failure in config file | `config::Error::Parse` | `ConfigError = 78` |
| Config file too large | `config::Error::FileTooLarge` | `ConfigError = 78` |
| Config file not found (explicit path) | `config::Error::FileNotFound` | `NotFound = 79` |
| Config file I/O failure | `config::Error::Io` | `IoError = 74` |
| Registry auth failure (401/403) | `oci::client::error::Authentication` | `AuthError = 80` |
| Package not found | `package_manager::error::NotFound` | `NotFound = 79` |
| Bad identifier format | `oci::identifier::error::*` | `DataError = 65` |
| Bad digest format | `oci::digest::error::Invalid` | `DataError = 65` |
| Network failure during fetch | `oci::client::error::Registry` (connect/timeout) | `Unavailable = 69` |
| Transient registry error (429/503) | `oci::client::error::Registry` with HTTP status | `TempFail = 75` |
| Filesystem permission denied | `std::io::ErrorKind::PermissionDenied` | `PermissionDenied = 77` |
| Filesystem I/O error (disk full, etc.) | `file_structure::Error`, `archive::Error::Io` | `IoError = 74` |
| Usage error (bad CLI args) | clap parse failure | `UsageError = 64` |
| Offline mode blocked operation | `ocx_lib::Error::OfflineMode` | `OfflineBlocked = 81` |
| Panic / task join failure | panic in task | `Failure = 1` (fall through) |

## Implementation Shape

The dispatch logic in `main.rs` downcasts the anyhow chain to determine the right code:

```rust
// crates/ocx_cli/src/main.rs (sketch)
use ocx_lib::cli::ExitCode;

fn classify(err: &anyhow::Error) -> ExitCode {
    for cause in err.chain() {
        if let Some(e) = cause.downcast_ref::<ocx_lib::config::error::Error>() {
            return match e {
                ocx_lib::config::error::Error::FileNotFound { .. } => ExitCode::NotFound,
                ocx_lib::config::error::Error::Io { .. } => ExitCode::IoError,
                _ => ExitCode::ConfigError,
            };
        }
        if let Some(e) = cause.downcast_ref::<ocx_lib::oci::client::error::ClientError>() {
            return match e {
                ocx_lib::oci::client::error::ClientError::Authentication(_) => ExitCode::AuthError,
                // ...
                _ => ExitCode::Unavailable,
            };
        }
        // ... other downcasts
    }
    ExitCode::Failure
}
```

`ocx_mirror` already has `MirrorError::exit_code()` returning `std::process::ExitCode::from(N)` — replace with matching `ExitCode` variants (SpecInvalid → `DataError`, SpecNotFound → `NotFound`, ExecutionFailed → `Failure`/`Unavailable` based on cause, SourceError → `Unavailable`/`TempFail`).

## Sources

[sysexits-bsd]: https://man.freebsd.org/cgi/man.cgi?sysexits
[rust-cli-book]: https://rust-cli.github.io/book/in-depth/exit-code.html
[sysexits-crate]: https://crates.io/crates/sysexits
[clig-dev]: https://clig.dev/#exit-codes
[std-exitcode]: https://doc.rust-lang.org/stable/std/process/struct.ExitCode.html

- [FreeBSD sysexits.h manpage][sysexits-bsd] — canonical numeric table
- [Rust CLI Book — Exit Codes][rust-cli-book] — endorses sysexits alignment
- [`sysexits` crate (crates.io)][sysexits-crate] — idiomatic Rust enum if taking a dep
- [clig.dev — Exit Codes][clig-dev] — "0 success, non-zero failure" (no numeric prescription)
- [`std::process::ExitCode`][std-exitcode] — `From<u8>` contract
- [Cargo exit codes](https://doc.rust-lang.org/cargo/commands/cargo.html) — reference (0 and 101 only)
- [npm exit codes](https://docs.npmjs.com/cli/v10/using-npm/scripts#exit-codes) — reference (0–7 semantic split)
- [Docker exit codes](https://docs.docker.com/engine/containers/run/) — 125/126/127 pattern
