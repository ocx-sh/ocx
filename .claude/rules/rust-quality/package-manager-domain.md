# Package-Manager Domain Rules

Rules for a tool that fetches, verifies, unpacks and installs remote
artifacts: bounded ingestion of untrusted bytes and numbers, registry
resilience, and batch partial-failure reporting. Domain-shaped, not
language-shaped — if the code you are editing never touches a registry,
an archive or an N-item batch, skip this file.

Contents: [Bounded Ingestion](#bounded-ingestion) · [Registry Resilience](#registry-resilience) ·
[Batch and Partial Failure](#batch-and-partial-failure) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Three subareas, one pipeline: `N items → registry HTTP → untrusted bytes →
local state → one exit code`. A defect anywhere in it surfaces the same way —
the tool reports success it did not achieve, hangs forever, or eats the machine.

Neighbouring rules own the adjacent halves, and this file does not restate them:

- Fan-out shape, `tokio::time::timeout` placement and cancel-safety: [async.md](async.md).
- Extraction-path safety, signature and digest verification, terminal sanitization: [security.md](security.md).
- The exit-code table and the `ExitCode` enum: [cli-contract](cli-contract.md).

**The mechanism** — clamp every number that arrived over the wire, build one
HTTP client, own one retry policy, return N outcomes for N items — is portable
to any ingesting CLI. **The pinned decisions** are: continue-and-collect as the
batch default (inverting cargo's fail-fast), no partial-success exit code, and
ingestion lints scoped to modules rather than the workspace. Not re-litigated.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn.
"Ingestion path" means every module between the registry response and the
verified artifact at its final path — name that directory list before landing
the scoped lints, or the verification commands have no target.

## Bounded Ingestion

Release builds wrap arithmetic silently while debug builds panic, so an
overflow on the ingestion path is invisible in exactly the build an agent
tests in. `overflow-checks = true` in release is a net behind explicit
`checked_*`, never a substitute for it.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PKG-01 | On the ingestion path, every `+ - * <<` whose operand traces to a manifest field, HTTP header or archive header uses `checked_*`, `saturating_*`, or `try_from` with a typed error on failure. | `#![warn(clippy::arithmetic_side_effects)]` as an inner attribute on ingestion modules; read every hit in the diff | MUST |
| PKG-02 | Scope `clippy::arithmetic_side_effects` and `clippy::as_conversions` to ingestion modules via inner attributes. Never in `[workspace.lints]` or `clippy.toml` — both are `restriction`-group and fire on trusted loop counters, and a repo-wide `deny` gets deleted within a week. | `rg -n --glob '*.toml' --glob '!external/**' -e arithmetic_side_effects -e as_conversions .` returns nothing | MUST |
| PKG-03 | No `as` for numeric narrowing or signed↔unsigned conversion on the ingestion path. `u32::try_from(x)?` / `usize::try_from(x)?`. `u64 as usize` is correct on 64-bit and wrong on a 32-bit target. | `#![deny(clippy::as_conversions)]` scoped per PKG-02; backstop `rg -n --type rust --glob '!external/**' -e ' as [ui]\d' -e ' as [ui]size\b' .` — module-scoped, so discard hits outside the ingestion module you are editing | MUST |
| PKG-04 | Never pass a declared length to any `with_capacity`. Clamp against a named `MAX_*`, then `try_reserve`, then grow as bytes actually arrive — `with_capacity` aborts on an allocation it cannot satisfy and there is no `Result` to catch. | `rg -n --type rust --glob '!external/**' 'with_capacity\(' .` — every argument is a compile-time constant or already clamped in the same function | MUST |
| PKG-05 | Every decompression step enforces two independent caps: an absolute output-byte cap via `Read::take`/`AsyncReadExt::take` on the **decompressor's output**, and an expansion-ratio cap; use the tighter. Capping the compressed reader limits download size, which was never the risk. | Three commands, because a union hit for one cap proves nothing about the other: `rg -n --type rust --glob '!external/**' '\.take\(' .` shows a cap on each decoder output; `rg -n --type rust --glob '!external/**' MAX_EXPANSION_RATIO .` must be non-empty; `rg -n --type rust --glob '!external/**' 'const MAX_\w+_BYTES' .` must be non-empty | MUST |
| PKG-06 | Per-entry decompressed-size cap on every archive entry **plus** a separate hard cap on entry count, incremented with `checked_add`. One 5 GB member and a million 5 KB members are different attacks. | `rg -n --type rust --glob '!external/**' MAX_ARCHIVE_ENTRIES .` — a counter checked against it once per `entries()` iteration | MUST |
| PKG-07 | `Content-Length` and every declared size field is a sizing *hint*. The real bound is bytes actually counted through `.take(cap)` — the header can be absent, wrong, or pre-decompression while the body is auto-decoded. | `rg -n --type rust --glob '!external/**' 'content_length\(\)' .` — every use feeds a `.min(MAX_*)` or a `try_reserve`, never a bare allocation and never the only limit on the read loop | MUST |
| PKG-08 | Hash bytes in the same loop iteration that writes them; write to a temp path in the target directory; compare the digest; only then rename. Delete the temp on any failure. The temp indirection is what stops unverified bytes being reachable under their final name. | `rg -n --type rust --glob '!external/**' -e 'fs::rename\(' -e '\.persist\(' .` — every hit is preceded in the same function by a digest comparison that returns early on mismatch | MUST |
| PKG-09 | A partial or range fetch is never digest-verified. A resumed download re-hashes the **complete reassembled file from byte 0** before publish — disk state left by a previous process run is untrusted input, and a correct suffix hash proves nothing about a truncated prefix. | The verification call's argument is the whole file handle, never a variable scoped to "bytes downloaded this attempt" | MUST |
| PKG-10 | Bound concurrent blob downloads with `Arc<Semaphore>` sized from a named constant; bound every inter-task chunk pipeline with `mpsc::channel(N)`. `unbounded_channel` is banned on the ingestion path — it relocates the uncapped allocation from one bad header to one slow consumer. | `rg -n --type rust --glob '!external/**' unbounded_channel .` returns zero; `Semaphore::new(` takes a named constant, not a literal | MUST |
| PKG-11 | Every limit is a named constant with a rationale comment, a stated configurability decision, and a dedicated typed error variant carrying the limit and the offending value (`LayerTooLarge { limit, actual }`). A limit trip is a caller decision point: hostile input (stop) vs transient I/O (retry). | `rg -n --type rust --glob '!external/**' 'const MAX_' .` cross-referenced against the module's error enum — every constant has a variant naming it | MUST |
| PKG-12 | Pin `tar >= 0.4.45` and record in code which size field wins when a PAX extension and the base header disagree. Older versions produce a parser differential against Go's `archive/tar` for the same bytes. | `cargo tree -i tar` shows ≥0.4.45; `cargo deny check advisories` clean | SHOULD |

## Registry Resilience

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PKG-13 | Exactly one function constructs the `reqwest::Client`. Every other call site is injected the built client — an ad hoc client silently loses the timeouts, the retry wiring, the pool config and the SSRF-guarded resolver at once. | `rg -n --type rust --glob '!external/**' -e 'Client::new\(' -e 'Client::builder\(' -e 'ClientBuilder::new\(' .` hits only the designated builder file. Wire as a CI gate, not a review heuristic | MUST |
| PKG-14 | The shared client sets `connect_timeout`, `read_timeout` and a finite `pool_max_idle_per_host`. `None` on either timeout is an unbounded hang; `pool_max_idle_per_host` defaults to `usize::MAX`, so a long-running TUI accumulates idle sockets. | Unit test asserting the built client carries `Some` for both timeouts; `rg -n --type rust --glob '!external/**' pool_max_idle_per_host .` is non-empty | MUST |
| PKG-15 | A whole-request `.timeout()` goes only on size-bounded calls (manifest GET/PUT, HEAD, tag list, token exchange). Streaming transfers rely on `connect_timeout` + `read_timeout` alone — one number cannot bound both a 20 KB manifest and a 4 GB blob. | No request reaching `.bytes_stream()` also carries a `.timeout(` wrap; every `.timeout(` site sits on a size-bounded response | MUST |
| PKG-16 | Retry is one policy value: retryable set `{429, 502, 503, 504}` plus transport errors, full jitter `random(0, min(cap, base·2^n))`, an attempt cap, a total wall-clock cap, and a `Retry-After` override that always wins. No inline retry loops, no status matching outside the one classifier. | `rg -n --type rust --glob '!*retry*' --glob '!external/**' -e 'StatusCode::' -e 'loop \{' .` — every hit is a finding except a non-retry event loop; the classifier and the policy live in the excluded module | MUST |
| PKG-17 | Never wrap a session-`POST` or a chunk-`PATCH` in the generic retry helper. OCI requires chunks in order, a replayed `PATCH` is rejected `416`, and an ambiguous failure hides how many bytes the server committed. Restart whole from a fresh `POST`. | The `PATCH` fn consumes the session handle by value, so reuse is a compile error. Backstop: classify the HTTP verb at every retry-helper call site | MUST |
| PKG-18 | `401` gets a dedicated auth path, never the generic retry policy: single-flight refresh shared across concurrent callers, exactly one refresh-and-retry per original request, proactive refresh before `expires_in` on long transfers. A 401 recurring after a fresh token is a hard failure. | `rg -n --type rust --glob '!external/**' -e UNAUTHORIZED -e '\b401\b' .` — handling sits outside anything the retry policy wraps, with a one-shot flag between the check and the re-attempt, never a bare `continue` | MUST |
| PKG-19 | The shared client's DNS resolver is the SSRF-guarded resolver on any path reaching a config- or registry-influenced host. A client built without it reopens the resolve→validate→connect TOCTOU window. | `rg -n --type rust --glob '!external/**' -e dns_resolver -e GuardedResolver .` present on every production construction path | MUST |
| PKG-20 | Retry attempts are visible on every interface: one overwritten status line on a TTY, one structured event per attempt in `--json`, one plain line per attempt otherwise. A >1s unexplained stall reads as a hang; CR spinners corrupt CI logs. | Review the retry policy's notify hook for all three branches; assert in a test that a non-TTY run emits no `\r` | SHOULD |

Replay safety is a type-level property, not a review finding:

```rust
/// Consumes the session: an ambiguous failure cannot be retried in place.
async fn patch_chunk(session: UploadSession, chunk: &[u8]) -> Result<UploadSession, UploadError>;
```

## Batch and Partial Failure

Default is continue-and-collect, inverting cargo's fail-fast. Compiling a
doomed graph is expensive; pulling an independent package is cheap and the
partial cache is a valid, resumable state. Partial success is nonzero and
classified by the worst failure — no tool surveyed and no `sysexits` value has
a partial-success code, so we do not invent a 15th one.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PKG-21 | Every command over N independent items returns one shared `BatchReport<Item, T, E> { succeeded, failed, skipped }`. A scalar `Result<(), E>` cannot say "47 succeeded, 3 failed", so it forces every implementation to abort early or lie. Per-command bespoke report structs are equally banned. `skipped` is distinct from `failed`: a skipped item's installer never ran. | `rg -n --type rust --glob '!external/**' -e 'struct \w*Report' -e 'struct \w*Summary' .` — any command-local type carrying succeeded/failed fields is a finding | MUST |
| PKG-22 | Inside a loop or fan-out over independent items: no bare `?` on a per-item call, no `.ok()`/`let _ =` discarding a per-item `Result`, no `collect::<Result<Vec<_>, _>>()`, no `try_join_all`, no `JoinSet::join_all()`. Accumulate into `Vec<(Item, Result<T, E>)>`, draining with `while let Some(res) = set.join_next_with_id().await`. Five spellings of one bug: `join_all()` on a `JoinSet` *panics* and cancels the rest, the opposite of `futures::join_all`. | `rg -n --type rust --glob '!external/**' -e '\.join_all\(' -e 'try_join_all\(' -e 'collect::<Result<' .` — every hit must be genuinely order-dependent; `rg -n --type rust --glob '!external/**' -e '\.ok\(\);' -e 'let _ = ' .` — every hit inside a loop body needs a justification comment; restrict to added lines on a diff | MUST |
| PKG-23 | Every batch function's doc comment names its strategy — fail-fast, continue-and-collect, or transactional — answered against: is partial application a valid state, is re-running convergent, does item K's failure invalidate K+1. The lockfile write that *records* the batch stays transactional even when the downloads feeding it are not. | Read the doc comment. Absence is the finding; no particular choice is | MUST |
| PKG-24 | The batch exit code is the **worst** classified failure among `failed` items, via the existing classification chain, falling back to `Failure` (1) only for genuinely mixed kinds. Never a new partial-success code, never derived from item counts or progress-bar state — a progress counter ticks on completion, not on success. | Read the final `ExitCode` derivation: it walks `failed` through `classify()` and takes the worst. `rg -n --type rust --glob '!external/**' -e '\.position\(\)' -e '\.length\(\)' .` — any hit feeding a branch or an exit code is a finding | MUST |
| PKG-25 | `--json` batch output is always `{ "summary": {...}, "items": [...] }` — `summary.status` ∈ `success`/`partial_failure`/`failure`/`cancelled`, per-item `status` ∈ `succeeded`/`failed`/`skipped`, `summary.exit_code` mirroring the process code, per-item errors reusing the existing error-slug envelope. Never a bare array. A script must branch on one field without counting the array. | One shared schema snapshot test run against every batch command in CI, not eyeballed per command | MUST |
| PKG-26 | Terminal rendering truncates to a fixed head (20) plus a `… and N more failures (see --json)` trailer; `--json` is never truncated. Untrusted item names in that output go through the terminal sanitizer — a batch multiplies that CWE-150 surface by N. | A test constructing >20 failures asserts the trailer appears and that `--json`'s item count still equals `summary.total` | SHOULD |
| PKG-27 | On SIGINT mid-batch: stop spawning, let in-flight atomic writes complete or abandon their temp file without touching the final path, and emit a report where every unattempted item is `skipped` with `SkipReason::Cancelled` and `summary.status` is `cancelled` — distinct from `partial_failure`, because the two call for different recovery. | Integration test that signals mid-batch and asserts every item is accounted for, none silently absent | MUST |
| PKG-28 | Retries-exhausted-on-transient (`TempFail`, 75) and hard not-found (`NotFound`, 79) resolve to different exit codes in every binary. No batch or retry path collapses them into a generic `1`. | A test asserting the two variants classify to distinct integers; `rg -n --type rust --glob '!external/**' 'process::exit\(' .` returns nothing outside the one boundary | MUST |

## What Agents Get Wrong Here

1. **`for item in items { do_thing(item)?; }` as the first draft of every batch
   loop.** Compiles, reads as idiomatic, and passes any suite whose fixtures all
   succeed — which is the only input shape a shallow suite covers.
2. **`as` instead of `try_from`.** Shorter, never a compile error, no type-level
   signal that the operand is tainted. A warn-only lint gets ignored; a denied
   one fails the agent's own `cargo clippy` self-check.
3. **Inline `reqwest::Client::new()` "just for this one call."** Threading the
   shared client through is more work and nothing at the call site looks wrong.
4. **`.ok()` added to silence a `Result` during development, never revisited.**
   How the real `dd` bug shipped into audited coreutils.
5. **`Vec::with_capacity(declared_len)` as "efficient" preallocation.** Textbook
   advice applied without asking where the length came from. One line, instant DoS.
6. **Setting only `.timeout()` and believing it covers a streaming download.**
   `read_timeout` is the one that matters and never appears in tutorials.
7. **`GzDecoder::new(reader)` chained straight into `read_to_end`.** No crate doc
   example shows the `.take()` wrapper, so the model copies the unbounded shape.
8. **`JoinSet::join_all()` because the name reads like `futures::join_all`.**
   Opposite failure semantics; turns one flaky download into total batch loss.
9. **`collect::<Result<Vec<_>, _>>()` as the shortest thing that "handles the
   Result".** Chosen over the `BatchReport` fold every time unless forbidden.
10. **A bare `loop { if 401 { refresh; continue } }`.** Both failure modes —
    infinite loop, and N concurrent refreshes that 429 the auth server — appear
    only under concurrency the agent never simulates.
11. **Wrapping a chunk-`PATCH` in the generic retry helper.** "Retry" reads as a
    uniform wrapper for any fallible async call; verb replay safety is not part
    of the model's default reasoning. Hence the by-value session handle.
12. **Inventing a partial-success exit code, or serialising the loop's output as
    a bare JSON array.** Both are the agent shipping its data structure instead
    of the contract a script branches on.

## Sources

- [corrode.dev — Bugs Rust Won't Catch](https://corrode.dev/blog/bugs-rust-wont-catch/) — the audited `chmod -R` and `dd` batch bugs
- [corrode.dev — Pitfalls of Safe Rust](https://corrode.dev/blog/pitfalls-of-safe-rust/) — debug/release overflow divergence and `as`-cast truncation
- [clippy — `arithmetic_side_effects`](https://rust-lang.github.io/rust-clippy/master/index.html#arithmetic_side_effects) — `restriction` group, allow-by-default, why scoping matters
- [tokio — `JoinSet`](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html) — `join_next` cancel-safety and the `join_all()` panic-and-cancel trap
- [reqwest — `ClientBuilder`](https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html) — `timeout` / `connect_timeout` / `read_timeout` / `pool_max_idle_per_host`
- [OCI distribution-spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) — chunked-upload ordering, `416`, manifest byte-exactness
- [AWS — Exponential Backoff and Jitter](https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/) — the full-jitter formula
- [RUSTSEC-2026-0068](https://rustsec.org/advisories/RUSTSEC-2026-0068.html) — tar-rs PAX-vs-header size differential
