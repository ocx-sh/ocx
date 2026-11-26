# Research: launcher size with a JSON sidecar parser

Date: 2026-11-26 · Expires: 2027-02-26 · Triggered by: hex-architect high, rules_ocx under remote execution (review finding on Choice 2) · Domain: launcher

## Question

Does a launcher that parses its sidecar with `serde_json` stay inside the 512 KiB shim budget (`crates/ocx_store/src/shim.rs:14`)? A JSON sidecar would avoid a hand-rolled grammar.

## What was run

Two throwaway binaries were built in a scratch directory with rustc's host toolchain, target `x86_64-unknown-linux-gnu`. This is the only target installed on this host. Both use the shim's profile: `opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`.

- `plain`: std only. It splits a NUL-separated sidecar, prepends `PATH`-like values and `exec`s.
- `json`: `serde` (derive) plus `serde_json`. It deserialises a struct with a version, a target, args, env records, unset and set lists, then `exec`s.

## Results

| Binary | Bytes |
|---|---|
| plain (std only) | 323392 (316 KiB) |
| json (serde + serde_json) | 374064 (365 KiB) |

`serde_json` adds about 50 KiB. Both are under 512 KiB. The committed Windows blobs are 239 KiB and 333 KiB (review seat, read from the repository).

## Not run and why

- The musl static, macOS and Windows targets are not installed here. Static musl links its libc into the binary, so it is expected to be larger. The CI size gate measures every target.

## Answer

A JSON sidecar parsed by `serde_json` fits the budget with about 147 KiB to spare on this target. Size alone is no reason for a hand-rolled format.
