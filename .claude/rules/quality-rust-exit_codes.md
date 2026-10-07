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
- **Reserve the range above 78 for next-action codes** — 79–127 sits below shell-reserved 128+ and above `EX__MAX = 78`. Spend it only under [Choosing a code](#choosing-a-code): each number there names an action no sysexits code offers ("not found", "auth failure", "policy refused", "unsupported"), never a feature.
- **`#[non_exhaustive]` required** — adding variant must not break semver.
- **`From<ExitCode> for std::process::ExitCode`** — lets `main()` return code directly, no explicit cast at call sites.
- **One enum per workspace, shared by all binaries** — primary CLI and sibling tools (e.g., mirror/publisher) consume same enum. Prevents drift.

---

## Canonical Shape

```rust
/// Process exit codes shared by every binary in this workspace. Values align with
/// BSD sysexits.h (EX__BASE = 64), clear of shell-reserved 1-2 and signal-derived 128+.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    Success = 0,          // successful completion
    Failure = 1,          // generic failure; only when no specific code applies
    UsageError = 64,      // bad invocation (EX_USAGE): fix the command line
    DataError = 65,       // malformed input (EX_DATAERR): fix the data
    Unavailable = 69,     // service unreachable (EX_UNAVAILABLE): restore it
    IoError = 74,         // local I/O fault (EX_IOERR): fix the filesystem
    TempFail = 75,        // may succeed on retry (EX_TEMPFAIL): the only retry code
    PermissionDenied = 77, // refused (EX_NOPERM): obtain the permission
    ConfigError = 78,     // bad configuration (EX_CONFIG): fix the config
    NotFound = 79,        // the named thing does not exist: name something that does
    AuthError = 80,       // missing or rejected credentials: supply or refresh them
    PolicyBlocked = 81,   // a local policy or safeguard refused: loosen it or force past it
    Unsupported = 82,     // valid request the service or build lacks; never retry (gRPC UNIMPLEMENTED)
}
```

---

## Choosing a code

A code is a promise about what the caller does next, so a script can `case $?` with no per-feature table.

- **A code names the caller's next action**: fix the command line, fix the data, retry, obtain a permission, supply credentials, loosen a policy, use another service. It never names the feature that failed.
- **Feature identity goes in a detail field**: a stable slug in the machine-readable error document (the RFC 9457 `type`, the gRPC error details). A new feature adds a slug under an existing code, not a number.
- **A new code needs a next action no existing code has.** Ask what the caller does differently; "this failure is new" is not an answer. A code with fewer than three distinct slugs under it is a feature code in disguise.
- **A retired number is never reused.** Record it in a `RETIRED` list and test that no variant takes it. Reuse silently changes the meaning for every script keyed on it; if one unavoidable reuse happens, name it once, before the contract is frozen.
- **Retry is one code.** Exactly one code means "the same command may succeed later"; no feature-specific transient code beside it.

Prior art: [gRPC status codes](https://grpc.io/docs/guides/status-codes/) (few codes chosen by caller action, specifics in error details), [sysexits.h](https://man7.org/linux/man-pages/man3/sysexits.h.3head.html) (generic codes, none tied to a feature), [RFC 9457](https://datatracker.ietf.org/doc/html/rfc9457) (coarse status plus a fine `type`).

---

## Error → Exit Code Classification

**The type that owns an error declares its own code.** Put the classification trait and `ExitCode` in a leaf crate that every library may depend on and that depends on none of them. Dependency direction stays acyclic (libraries → leaf ← binary), so each library implements the trait for its own error types, in the crate that declares them.

```rust
// leaf crate: `trait ClassifyExitCode { fn classify(&self) -> Option<ExitCode>; }`
// (`None` defers to the next cause). Library crate, next to the error:
#[derive(Debug, thiserror::Error, Classify)]
pub enum InstallError {
    #[error("offline")]
    #[exit(PolicyBlocked, slug = "install_offline", summary = "An install was refused offline")]
    Offline,
    #[error(transparent)]
    #[exit(delegate)] // the inner type classifies itself
    Package(#[from] PackageError),
}
```

- **Prefer a derive over a hand-written `impl`.** The derive makes a variant with no declared code a compile error, which a hand-written exhaustive `match` only does until someone adds `_ =>`. Each row carries the code, the error's stable machine-readable slug and a one-line summary, so all three are declared once, side by side.
- **The binary keeps one free function** that walks `anyhow::Error::chain()`, asks each cause to classify itself, and falls back to `ExitCode::Failure`. It owns what no library can: foreign error types (`std::io::Error`, a dependency's error) and the process boundary.
- **One registry list in the binary** names every classifiable type and generates the downcast ladder, so a type is added in one place. A hand-written `downcast_ref` chain drifts: a new type compiles, never matches, and exits with the fall-through code.
- **A type whose code is chosen at run time** is the one reason for a hand-written `impl`.

**Layered errors** (outer enum → context struct → discriminant enum): each layer delegates to the next; the innermost kind owns the code.

**Default fall-through.** A chain with no classified cause exits `ExitCode::Failure`. Lock it with a test so it cannot silently change.

```rust
pub fn classify_error(err: &anyhow::Error) -> ExitCode {
    for cause in err.chain() {
        if let Some(code) = try_classify(cause) {   // registry-generated ladder
            return code;
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::PermissionDenied
        {
            return ExitCode::PermissionDenied;
        }
    }
    ExitCode::Failure
}
```

---

## Anti-Patterns

### Block

- **A code minted for one feature's failure** (`ExitCode::FooFeatureUnavailable`), or one reused after retirement. See [Choosing a code](#choosing-a-code).
- **Single-digit numeric codes for semantic categories** (e.g., `exit 3` for "network error"). Collides with shell-reserved 1/2, no discoverable meaning.
- **Bash `exit $?` chains with magic numbers** inside the CLI itself — use enum, not literals.
- **Different binaries in same workspace using different exit-code taxonomies** — blocks shared error handling in CI scripts. One enum, shared.
- **Classification of an error type you own defined outside the crate that owns it**, or a **hand-maintained downcast list that can drift** from the set of types. The first forces the owner to export internals; the second silently exits with the fall-through code for any type added after it. Declare the code beside the type (derive) and generate the ladder from one registry list.
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
    75) echo "temporary failure; retry with backoff" ;;
    78) echo "bad config; fix and retry" ;;
    79) echo "not found; pin a different version" ;;
    80) echo "auth failed; supply or refresh credentials" ;;
    81) echo "policy blocked; loosen the flag or force past the safeguard" ;;
    82) echo "capability unsupported; use another service or build; never retry" ;;
    *)  echo "unknown failure ($?)"; exit 1 ;;
esac
```

---

## Sources

- [FreeBSD `sysexits.h` manpage](https://man.freebsd.org/cgi/man.cgi?sysexits) — canonical numeric table
- [Rust CLI Book — Exit Codes](https://rust-cli.github.io/book/in-depth/exit-code.html) — endorses sysexits-aligned codes
- [`sysexits` crate](https://crates.io/crates/sysexits) — Rust enum with `Termination` impl; reference shape if prefer dep over owning enum
- [`std::process::ExitCode` docs](https://doc.rust-lang.org/stable/std/process/struct.ExitCode.html) — `From<u8>` contract
- [clig.dev — Exit Codes](https://clig.dev/#exit-codes) — "0 success, non-zero failure" (no numeric prescription; defers to tool conventions)
- [npm exit codes](https://docs.npmjs.com/cli/v10/using-npm/scripts#exit-codes) — semantic differentiation example in practice