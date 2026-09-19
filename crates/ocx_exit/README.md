# ocx_exit

Process-outcome vocabulary shared by every OCX binary and by a future SDK: `ExitCode`, `ErrorCategory`.

**Tier:** interface

**May depend on:** none (`serde` is its only dependency; no `indicatif`, `console`, `tracing`, `clap`)

**Named dependency exceptions:** none

## What it holds

| Type | Contract |
|---|---|
| `ExitCode` | The process status every OCX binary exits with. Numeric values align with BSD `sysexits.h` (`EX__BASE = 64`) so they collide with neither the shell-reserved codes (1–2) nor the signal-derived ones (128+). Scripts `case $?` on them. |
| `ErrorCategory` | The frozen `error.kind` vocabulary of the `--format json` error envelope. `from_exit_code` is total over `ExitCode` by a **wildcard-free** match, so a new exit code is a compile error until it is classified. |

Both are wire contracts, not internal types: a published script reads them. A
value changes only as an announced break, never as a refactor.

## Why it is the seam

An SDK that drives `ocx` as a subprocess needs exactly these two vocabularies
and nothing else — so this crate depends on no other crate in the workspace and
pulls no terminal, logging or argument-parsing machinery into a caller's graph.
The public surface is two types at the crate root, one path each; the modules
behind them are private.
