# Rust CLI Contract

The exit codes, streams, and interface behaviour every OCX-family binary
(`ocx`, `grim`, `ocx-mirror`) honours. Read this before changing anything
that can end a process, choose a status, or write to stdout — the file
that does it is not reliably named `main.rs` or `exit_code.rs`, which is
why this is routed to by subject rather than matched by path.

Contents: [The Exit-Code Table](#the-exit-code-table-pinned) ·
[Exit-Code Rules](#exit-code-rules) · [Streams and Output](#streams-and-output) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers, and the difference matters when adopting this elsewhere:

- **The mechanism** — own a typed enum, align it with `sysexits.h`, never
  let a bare integer reach a process exit, keep stdout for results — is
  general Rust CLI practice.
- **The table** — the specific numbers below — is a *pinned decision*.
  It is already shipped, scripted against, and locked by tests. It is not
  re-derivable and not open for re-litigation. Another project adopting
  this rule keeps the mechanism and assigns its own numbers above 78.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## The Exit-Code Table (pinned)

Every binary returns exactly one of these, as a `#[repr(u8)]` variant of
one `ExitCode` enum per workspace.

| Code | Name | Meaning |
|---|---|---|
| 0 | `Success` | Success |
| 1 | `Failure` | Unclassified failure — classification fall-through only |
| 64 | `UsageError` | Bad invocation, including every clap parse failure |
| 65 | `DataError` | Malformed input data, manifest, or lockfile |
| 69 | `Unavailable` | Registry or resource unreachable, non-retryable |
| 74 | `IoError` | Filesystem I/O fault |
| 75 | `TempFail` | Retryable transient failure |
| 77 | `PermissionDenied` | `EPERM` / insufficient permissions |
| 78 | `ConfigError` | Bad config file or missing required field |
| 79 | `NotFound` | Resource or explicit config path not found |
| 80 | `AuthError` | Authentication failure |
| 81 | `PolicyBlocked` | A deliberate `--offline` / `--frozen` / verify-offline refusal — policy, not fault |
| 82 | `DirtyRcBlock` | Refused to rewrite a shell-RC block carrying user edits |
| 83–99 | *(unassigned)* | Next free slots; allocate upward from 83 |
| 128+N | *(not ours)* | Forwarded signal status of a **child** process only |

64–78 mirror BSD `sysexits.h`; 79+ is the private range above `EX__MAX`.
Never claimed and never reused: 2–63, 66–68, 70–73, 76, 100–255. An
unused sysexits value stays unclaimed rather than being repurposed with a
different meaning.

## Exit-Code Rules

| ID | Rule | Verification | Severity |
|---|---|---|---|
| EXIT-01 | Every process exit value comes from the shared `ExitCode` enum. No bare integer literal reaches a process exit. | `rg -n --type rust --glob '!external/**' -e 'ExitCode::from\(' -e 'process::exit\(' -e 'use std::process::exit' .` — every hit must be the enum's own `From` impl or `main`; the import pattern flags files using the bare `exit(…)` form | MUST |
| EXIT-02 | `main` returns `std::process::ExitCode`. `std::process::exit` is forbidden outside `main`'s return path and a documented signal re-raise — it skips every destructor on every thread, corrupting mid-write lockfiles and leaking temp dirs. | `rg -n --type rust --glob '!external/**' -e 'process::exit\(' -e 'use std::process::exit' .` — anchored on the call, not the name in a doc comment; every hit outside `main`'s return path is a finding | MUST |
| EXIT-03 | Parse with `try_get_matches`/`try_parse`: `--help`/`--version` → 0, every other clap error → 64. clap's default exit 2 never escapes. | Integration test: `ocx --bogus` and `grim --bogus` each exit 64. `rg -n --type rust --glob '!external/**' -e '\.get_matches\(\)' -e '::parse\(\)' .` finds un-intercepted sites — the intercepted `try_` spellings do not match, so expect zero hits and treat any as a finding | MUST |
| EXIT-04 | Assign no application meaning to 1, 2, or anything ≥ 100. 1 is reachable only as the fall-through. | Enumerate the discriminants; anything outside 0/1/64–99 is a finding | MUST |
| EXIT-05 | Never `.unwrap()` `ExitStatus::code()`. On unix fall back to `128 + status.signal()`. A signal-killed child never maps to success. | `rg -nU --type rust --glob '!external/**' '\.code\(\)\s*\.unwrap' .` — `-U` and `\s*` catch the rustfmt-broken chain, which is the common spelling. This also matches `.unwrap_or(n)`, equally a finding: it maps a signal kill to a fixed code | MUST |
| EXIT-06 | New codes are append-only. A shipped number and its meaning are never reassigned, even after the feature is removed. | Review: any diff changing an existing discriminant is a finding | MUST |
| EXIT-07 | The fall-through to `Failure` is locked by a test, and classification matches are exhaustive — no `_ =>` wildcard over a local error enum, so a new variant compile-errors until it is classified. | Each error enum has a fall-through test | MUST |
| EXIT-08 | One exit-code taxonomy per workspace. A binary needing its own carries an ADR and a doc note naming the carve-out. | `rg -n --type rust --glob '!external/**' 'enum ExitCode' .` — each hit outside the shared enum needs its ADR link | MUST |
| EXIT-09 | Classification may be a free function **or** a trait implemented per error type. Pick one shape per workspace; do not mix. | Review: a workspace with both shapes is a finding | SHOULD |
| EXIT-10 | Every code has a doc comment on the variant, a row in the tool's public docs, and a test asserting a real invocation produces it. | Public doc row count == variant count | MUST |
| EXIT-11 | A signal handler that exists for cleanup derives its status from the signal received — restore `SIG_DFL` and re-raise. Never hardcode 130. | The exit/raise call references the matched signal | SHOULD |

`ExitCode` lives in the library crate, not the binary, so every sibling
binary and every test can name it.

```rust
/// Process exit codes for every binary in this workspace.
///
/// 64–78 mirror BSD `sysexits.h`; 79+ is the private range above `EX__MAX`.
/// Values are a public contract: append only, never reassign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    Success = 0,
    Failure = 1,
    UsageError = 64,
    // ...
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        std::process::ExitCode::from(code as u8)
    }
}

fn main() -> std::process::ExitCode {
    match app::run() {
        Ok(code) => code.into(),
        Err(error) => {
            report(&error); // sanitized, stderr, exactly once
            classify(&error).into()
        }
    }
}
```

## Streams and Output

| ID | Rule | Verification | Severity |
|---|---|---|---|
| CLI-01 | The result goes to stdout; logs, progress, warnings, prompts and errors go to stderr, unconditionally. | `rg -n --type rust --glob '!external/**' -e '\bprintln!' -e '\bprint!' .` — the `\b` keeps the correct `eprint` forms out; a hit outside result formatting is a finding; restrict to added lines on a diff | MUST |
| CLI-02 | Under `--format json`, stdout carries **only** the payload — no banner, no progress, no trailing "done". | Per JSON subcommand, an integration test parses the whole captured stdout | MUST |
| CLI-03 | Render the error chain exactly once, at the single `main.rs` boundary, and sanitize it for control and bidi characters first — chains quote names read off wire documents (CWE-150 terminal injection). | The boundary write routes through the sanitizer; pin it with a structural test | MUST |
| CLI-04 | Structured errors use the pinned envelope on stdout: `{"error": {"code": <slug>, "exit": <int>, "message": …, "reason"?, "retryable"?, "forceable"?}}`. Slugs are stable; changing one is breaking. | Snapshot test per exit code | MUST |
| CLI-05 | A closed downstream stdout pipe is a clean exit 0, handled once centrally — never a panic, never a propagated error. Rust sets `SIGPIPE` to `SIG_IGN` before `main`, so `println!` panics once a reader like `head` closes it. | Integration test: a reader that closes stdout after one line — exit 0, no panic, no error line | MUST |
| CLI-06 | Any command that can emit more than a handful of lines writes through a locked `BufWriter` and flushes explicitly — `io::Stdout` is line-buffered even when piped to a file. | `println!` inside an unbounded loop is a finding | SHOULD |
| CLI-07 | Colour comes from `anstream`/`colorchoice` plus an explicit `--color` taking `auto`, `always` or `never`, decided per destination stream. Never hand-roll the env check. | `rg -n --type rust --glob '!external/**' -e 'env::var\("NO_COLOR' -e 'env::var\("CLICOLOR' .` — anchored on the read rather than the word in a comment; discard hits inside the one colour module, any hit outside it is a partial reimplementation | MUST |
| CLI-08 | Progress bars draw to stderr and are suppressed when it is not a TTY, when `CI` is set, or when a machine-output flag is active. | `rg -n --type rust --glob '!external/**' 'ProgressDrawTarget::stdout' .` — any hit is a finding | MUST |
| CLI-09 | Never prompt unless `stdin().is_terminal()`, and always ship a non-interactive bypass (`--yes`/`--no-input`). Truthy `CI` counts as non-interactive even on a pseudo-TTY. | Every `read_line`/prompt call site has a preceding TTY gate | MUST |
| CLI-10 | Global flags (`--color`, `--quiet`, `--log-level`, `--format`) live in one `#[derive(Args)]` struct flattened into every subcommand, identical across the family. | `rg -n --type rust --glob '!external/**' 'pub color:' .` shows exactly one definition site — `long = "color"` finds nothing, because the idiomatic `#[arg(long)]` takes the flag name from the field | MUST |
| CLI-11 | Never accept a secret through a flag value or a plain env var — use `--password-file`, stdin, or the credential store. Flag values land in `ps` and shell history; env vars leak via `/proc` and CI log dumps. | `rg -n --type rust --glob '!external/**' -e '"password"' -e '"token"' -e '"secret"' .` — only a hit in a clap arg definition or an `#[arg(env = …)]` is a finding, test fixtures and auth-type enums are not; restrict to added lines on a diff | MUST |
| CLI-12 | A `///` on a clap-facing surface states the user contract and nothing else: ASCII only, short help ≤ ~70 chars, no ADR/section/code-path references, no dates. | Help-text gates in `task verify`: ASCII, no internal references, valid definition | MUST |
| CLI-13 | Config, cache and data paths come from `directories::ProjectDirs`. Every tool env var is prefixed (`OCX_`/`GRIM_`) and documented next to its flag. | `rg -n --type rust --glob '!external/**' -e '"HOME"' -e '"USERPROFILE"' -e 'dirs::home_dir' -e 'env::home_dir' .` — the quoted keys catch the `cfg!(windows)` hand-roll that `env::var\("HOME"\)` misses, and the quotes keep `OCX_HOME` out; discard hits inside the one platform-conventions module, any hit outside it is a finding. Then `rg -n --type rust --glob '!external/**' 'env::var\("[A-Z]' .` — every key must be `OCX_`/`GRIM_`-prefixed or a documented standard var | MUST |
| CLI-14 | Completions and man pages are generated from the same `clap::Command` used for parsing, via `clap_complete`/`clap_mangen` in an xtask. | A checked-in completion script with no generator is a finding | SHOULD |
| CLI-15 | Layer config-file values in **before** clap parses, so precedence is flags > env > project config > user config > system config. `#[arg(env = …)]` alone silently drops two tiers. | Test: env beats project config; a flag beats both | SHOULD |
| CLI-16 | Print something within ~100 ms for any command doing network I/O, and say so when disk state changes. | Behavioural: a status line before the first network await | CONSIDER |

## What Agents Get Wrong Here

1. `std::process::exit` sprinkled mid-function because it is the shortest
   way to stop — silently skipping destructors and unflushed writes.
2. Letting clap's default `exit(2)` escape by calling `parse()` instead of
   `try_parse()`.
3. `eprintln!`-ing the error at the point of failure *and* returning it —
   the same failure reported twice, once unsanitized.
4. Writing progress or a "done" line to stdout under `--format json`.
5. Hand-rolling `NO_COLOR` handling and missing `CLICOLOR_FORCE`,
   `TERM=dumb`, or per-stream TTY detection.
6. `.code().unwrap()` on a child's `ExitStatus` — panics exactly when the
   child crashed hardest.
7. Inventing a new small-integer code (3, 4, 5) for a new failure class
   instead of allocating from 83 upward.

## Sources

- [FreeBSD `sysexits.h`](https://man.freebsd.org/cgi/man.cgi?sysexits) — the canonical numeric table
- [`std::process::ExitCode`](https://doc.rust-lang.org/std/process/struct.ExitCode.html) and [`exit`](https://doc.rust-lang.org/std/process/fn.exit.html) — the `From<u8>` contract and the destructor warning
- [Rust CLI Book: exit codes](https://rust-cli.github.io/book/in-depth/exit-code.html)
- [clig.dev](https://clig.dev/) — streams, machine output, colour, interactivity
- [no-color.org](https://no-color.org/) — the `NO_COLOR` convention
- [curl exit codes](https://everything.curl.dev/cmdline/exitcode.html) — a 50-code scheme that works because retired numbers stay retired
- [rust-lang/rust#97889](https://github.com/rust-lang/rust/issues/97889) — `SIGPIPE` is `SIG_IGN` before `main`
