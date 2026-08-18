---
paths:
  - "**/*.rs"
summary: The Rust quality index — non-negotiables, the verification gate, the pinned exit-code contract, and where the depth lives
keywords: rust,quality,standards,review,async,errors,exit-codes,cli,security,testing,architecture,idioms
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# Rust Quality

Traps, not maps. Everything here names a mistake that gets made without
it; the architecture of any particular codebase is discoverable by
reading the code, so it is not in this file.

Contents: [The Gate](#the-gate) · [Non-Negotiables](#non-negotiables) ·
[Where the Depth Is](#where-the-depth-is) · [Severity](#severity) ·
[Siblings](#siblings)

**Ending a process, choosing a status, or writing to stdout? Read
[rust-quality/cli-contract.md](rust-quality/cli-contract.md) first.** The
exit-code table is pinned, already scripted against, and locked by tests —
a number invented locally is a shipped contract break.

## The Gate

Run it after every change, in this order — each stage costs more than the
last, so the common case never reaches the slow ones. Narrowest scope
first; widen only when the narrow one is green.

```bash
cargo check -p <crate> --all-targets --locked
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --locked
cargo test --doc
cargo deny check
```

A task is done when a command, its exit code, and the tree it ran against
are all named. Narration is not evidence. `--locked` is not optional: a
lockfile change outside the task's declared scope means the build that
went green is not the code that was reviewed.

## Non-Negotiables

Every line below blocks a merge. IDs resolve to the depth files in the
next section, where each rule carries its rationale and verification.

| # | Rule | ID |
|---|---|---|
| 1 | No `unwrap`/`expect` on a fallible path outside tests. An `expect` message states the invariant, never the failure. | ERR-09, ERR-10 |
| 2 | Subsystems return concrete `thiserror` enums; `anyhow` appears only at the binary boundary. | ERR-01 |
| 3 | A function returning `Result` never also logs that error. Never swallow one with `let _ =`, `.ok()`, or `unwrap_or_default()` without a stated reason. | ERR-18, ERR-19 |
| 4 | No `std::sync` guard held across an `.await`. No blocking work — `std::fs`, `Command`, hashing, decompression — inside an `async fn` without `spawn_blocking`. | ASYNC-01, ASYNC-02 |
| 5 | Every network or subprocess await carries a deadline. Every fan-out whose width is caller- or wire-controlled is bounded. No unbounded channels. | ASYNC-04, ASYNC-05, ASYNC-06 |
| 6 | Every spawned task's handle is awaited, held in a `JoinSet`, or explicitly detached with a reason — a dropped handle silently discards the panic. | ASYNC-09 |
| 7 | Exit values come from the shared `ExitCode` enum; `std::process::exit` appears nowhere but `main`'s return path. | EXIT-01, EXIT-02 |
| 8 | stdout carries the result; logs, progress, prompts and errors go to stderr. Under a machine-output flag, stdout is the payload and nothing else. | CLI-01, CLI-02 |
| 9 | Untrusted input is validated before use, not after: archive entry paths after normalisation, digests before the bytes are used or executed, sizes and entry counts bounded. | SEC family |
| 10 | No secret reaches a log line, an error message, a `Debug` impl, `argv`, or a span field. | ERR-17 |
| 11 | `unsafe` is forbidden. Where a crate carries a documented exemption, every block has a `// SAFETY:` comment stating the invariant it relies on. | LINT-07 |
| 12 | Never `#[allow(...)]`. Suppress with `#[expect(lint, reason = "…")]`, which fails the build once the lint stops firing. | LINT-08 |
| 13 | No `todo!()` or `unimplemented!()` reachable on a production path outside a labelled stub commit — both compile clean and panic only at runtime, sailing past the fast gate. | LINT-05 |
| 14 | Never weaken an assertion, `#[ignore]` a test, hand-edit a snapshot, or touch the gate's own config to turn a check green. Prove a check can go red before trusting it green. | TEST-12 |
| 15 | Private by default. `pub(crate)` for cross-module use; bare `pub` only for a genuine external contract. | ARCH-15 |
| 16 | Three functions threading the same leading parameter tuple means a missing type — and the type they move onto is capped at 2 inherent `impl` blocks and 25 methods. | ARCH-01, ARCH-03 |
| 17 | Anything hash- or filesystem-ordered is sorted before it is emitted, written, or asserted on. | DATA-DET |
| 18 | A write to a cache, lockfile, or install tree goes through the durable-write helper — temp file in the *target's* parent, sync, rename. Never truncate in place, and never treat a failed `fsync` as retryable. | STATE-1, STATE-4 |
| 19 | Nothing that can panic goes in a `Drop` body, and `process::exit` is never reachable while a guard is alive — `exit` runs no destructor. | STATE-11, STATE-15 |
| 20 | Do not reach for the crate or API you remember. Version-blind recall is the highest-frequency failure mode in agent-written Rust — check the current-APIs table first. | EVO |

## Where the Depth Is

Read the file for the work you are about to do, not for the topic it is
filed under. One level deep; these files do not point at each other.

| Doing… | Read |
|---|---|
| Adding a type, trait, or module; moving code between them; touching crate boundaries | [rust-quality/architecture.md](rust-quality/architecture.md) |
| Defining or changing an error type; deciding what a failure returns or prints | [rust-quality/errors.md](rust-quality/errors.md) |
| Anything that ends a process, picks an exit status, parses argv, or writes to stdout | [rust-quality/cli-contract.md](rust-quality/cli-contract.md) |
| Anything `async`, spawned, locked, timed out, retried, or cancelled | [rust-quality/async.md](rust-quality/async.md) |
| Handling registry content, archives, paths from outside, subprocesses, credentials, TLS | [rust-quality/security.md](rust-quality/security.md) |
| Writing or changing a test, a fixture, a snapshot, or a test seam | [rust-quality/testing.md](rust-quality/testing.md) |
| A change made for speed, or one that could plausibly cost it | [rust-quality/performance.md](rust-quality/performance.md) |
| Writing a doc comment, a log line, a span, or user-facing diagnostic output | [rust-quality/docs-and-tracing.md](rust-quality/docs-and-tracing.md) |
| Designing a public function signature, a derive set, or a conversion | [rust-quality/api-and-idioms.md](rust-quality/api-and-idioms.md) |
| Choosing a crate, or writing code against an API you have not checked this year | [rust-quality/current-apis.md](rust-quality/current-apis.md) |
| Serializing anything, changing an on-disk or wire format, or emitting output another tool parses | [rust-quality/data-and-formats.md](rust-quality/data-and-formats.md) |
| Touching a path, a filename, an archive entry, a `cfg(target_os)` branch, or the clock | [rust-quality/platform-and-paths.md](rust-quality/platform-and-paths.md) |
| Writing state that must survive a crash — atomic writes, fsync, locks, guards, `Drop` | [rust-quality/durable-state.md](rust-quality/durable-state.md) |
| Registry resilience, retries against a remote, partial failure across a batch of packages | [rust-quality/package-manager-domain.md](rust-quality/package-manager-domain.md) |
| Anything in the terminal UI — rendering, events, keybindings, terminal state | [rust-quality/tui.md](rust-quality/tui.md) |
| Reviewing a diff someone else — or something else — wrote | [rust-quality/reviewing-a-diff.md](rust-quality/reviewing-a-diff.md) |
| Checking whether a change turned the gate green by weakening it | [rust-quality/diff-integrity.md](rust-quality/diff-integrity.md) |
| Moving code at scale — extracting a type, splitting a crate, a codemod across many files | [rust-quality/restructuring.md](rust-quality/restructuring.md) |

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not
in the commit body. CONSIDER = Suggest: never blocks, never re-raised
after a decline.

Keep the Block list short enough that a blocked change is unusual. A rule
set where everything blocks teaches the reader to negotiate with all of it.

## Siblings

- **`rust-cargo`** — lint policy, toolchain pinning, dependency gates, CI
  job design and release settings. Loads on `Cargo.toml` and the tool
  configs beside it. **Read it when writing or changing a Rust CI workflow
  or a release profile** — it does not glob `.github/workflows/`, because a
  workflow filename says nothing about its language and the directory holds
  every other job the repository has.
