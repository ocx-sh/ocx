# ocx_exit

Process-outcome vocabulary shared by every OCX binary and by a future SDK: `ExitCode`, `ErrorCategory`, the classification traits and the types they answer with, the `families!` registry macro, and the `Classify` derive tier crates use to declare their own classification.

**Tier:** interface

**May depend on:** `ocx_exit_derive` (and `serde`; no `indicatif`, `console`, `tracing`, `clap`)

**Named dependency exceptions:** none

## What it holds

| Type | Contract |
|---|---|
| `ExitCode` | The process status every OCX binary exits with. Numeric values align with BSD `sysexits.h` (`EX__BASE = 64`) so they collide with neither the shell-reserved codes (1–2) nor the signal-derived ones (128+). Scripts `case $?` on them. One macro row per variant declares the value, `summary()` and `category()`, and `ALL` lists the rows, so a new code cannot exist without its category or its table entry. |
| `ErrorCategory` | The frozen `error.kind` vocabulary of the `--format json` error envelope, with `ALL` and `summary()` for the published table. |
| `ClassifyExitCode`, `ClassifyErrorKind` | The two traits an error type implements to name its exit code and its `error.detail` slug. `#[derive(Classify)]` writes both from `#[exit(...)]` attributes; a hand impl is the exception, not the pattern. `DETAILS` lists every row a type declares. |
| `Detail`, `DetailEntry`, `Row`, `Pick`, `Decision` | What the traits answer with: a slug row, and where the slug comes from (a fixed row, or a source chain only the binary can walk). `Row` and `Pick` let a `with` function answer only with rows its arm declared. |
| `families!` | The binary's one list of classifiable types. It generates the downcast ladder and the detail registry, and refuses at compile time a listed type that lacks either trait. |
| `Classify` | The derive, re-exported from `ocx_exit_derive`, so a tier crate names `ocx_exit` alone. |

`ExitCode` and `ErrorCategory` are wire contracts, not internal types: a published script reads them. A
value changes only as an announced break, never as a refactor.

## Why it is the seam

An SDK that drives `ocx` as a subprocess needs the two vocabularies, and a tier crate needs the traits to
declare its own errors. So this crate depends on no workspace crate but its derive (a build-time proc-macro)
and pulls no terminal, logging or argument-parsing machinery into a caller's graph. Everything is
re-exported at the crate root, one path each; the modules behind it are private.

Libraries declare their classification; only the binary resolves it. A foreign cause such as
`std::io::Error` can carry no impl here, so the chain walk that reaches it lives in `ocx`.
