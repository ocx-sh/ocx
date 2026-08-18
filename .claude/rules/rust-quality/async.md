# Async and Concurrency

Tokio rules for a fan-out CLI: blocking work, locks, deadlines, bounded
concurrency, cancellation, task handles, async traits, and deterministic
time-based tests. Loads when editing any file containing `async fn`,
`tokio::`, `std::sync::Mutex`, or a `spawn`/`JoinSet` call.

Contents: [Pinned Decisions](#pinned-decisions) ·
[Blocking and Thread Boundaries](#blocking-and-thread-boundaries) ·
[Locks and Shared State](#locks-and-shared-state) ·
[Deadlines, Bounds, and Retries](#deadlines-bounds-and-retries) ·
[Cancellation, Tasks, and Actors](#cancellation-tasks-and-actors) ·
[Async Traits and Testing](#async-traits-and-testing) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers. **The mechanism** — never block a worker thread, never hold a
guard across an await, bound every fan-out, treat cancellation as a design
input — is general tokio practice and portable anywhere. **The decisions
below** are pinned for this project: chosen once, not re-litigated per PR.

## Pinned Decisions

- **`multi_thread` is the default runtime flavour.** `current_thread`
  freezes every spawned task the moment `block_on` returns, and every
  fan-out path here spawns. `current_thread` is allowed only in a binary
  that provably never spawns.
- **Locks are `std::sync`, critical sections are short.** `tokio::sync::Mutex`
  is the exception that needs a reason (ASYNC-03), not the default.
- **No `parking_lot`.** The performance gap closed, and it silently drops
  poisoning.

## Blocking and Thread Boundaries

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ASYNC-01 | Never call blocking work (`std::fs`, `std::process::Command`, sha256/digest, tar/zip extract, compression, `rayon`) directly in an `async fn` body — wrap it in `tokio::task::spawn_blocking`. Tokio's threshold is ~10–100 µs between awaits; past that the worker thread starves every other in-flight task. | Restrict to added lines on a diff — the tree-wide count is legitimate sync code and never zero. `git diff -U0 -G'std::fs::' origin/main -- '*.rs'`; `git diff -U0 -G'std::process::Command' origin/main -- '*.rs'`; `git diff -U0 -G'Sha256::' origin/main -- '*.rs'` — each added hit inside an `async fn` must be in a `spawn_blocking` closure | MUST |
| ASYNC-08 | `Handle::block_on`/`Runtime::block_on` must not be reachable from any code that can already be running inside a tokio task — it panics, far from the call site, inside a sync callback. | `rg -n --type rust --glob '!external/**' 'block_on\(' .`; trace each caller to its entry point. The legal nested form is `block_in_place` + `Handle::current().block_on` | MUST |
| ASYNC-11 | Never call into a thread pool (`rayon`, `spawn_blocking`) while holding a lock the pool's workers could also need — circular wait, and the deadlock reproduces only under load. | Read every `spawn_blocking(`/`rayon::` call site; no enclosing scope may still hold a guard | MUST |
| ASYNC-18 | Long-lived or unbounded-duration blocking work goes on a dedicated `std::thread`, not `spawn_blocking` — runtime shutdown cannot cancel work already inside the blocking pool, so a stuck closure wedges process exit. | Read every `spawn_blocking` closure for an unbounded loop or an un-timed network call | SHOULD |

## Locks and Shared State

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ASYNC-02 | Never hold a `std::sync`/`parking_lot` `MutexGuard` or `RwLock` guard across an `.await`. | `cargo clippy --all-targets -- -D clippy::await_holding_lock -D clippy::await_holding_invalid_type -D clippy::await_holding_refcell_ref` in CI | MUST |
| ASYNC-03 | Default to `std::sync::Mutex`/`RwLock`; reaching for `tokio::sync::Mutex` requires a comment naming the `.await` the critical section must span — needing the async lock is a signal the critical section is too big. | `rg -n --type rust --glob '!external/**' -e 'tokio::sync::\{?[^};]*Mutex' -e 'tokio::sync::\{?[^};]*RwLock' .` — the optional brace catches `use tokio::sync::{Mutex, watch};`, which a fully-qualified pattern misses; every hit needs the justification comment | MUST |
| ASYNC-12 | Never put shared mutable state behind `Arc<RefCell<T>>`/`Arc<Cell<T>>`, and never add `unsafe impl Send`/`Sync` to silence a compiler error — that impl removes the only check that the design is sound. | `rg -n --type rust --glob '!external/**' -e 'Arc<RefCell' -e 'Arc<Cell' -e 'unsafe impl.*Send' -e 'unsafe impl.*Sync' .` — zero is the expected answer | MUST |
| ASYNC-19 | Use `std::sync::OnceLock`/`LazyLock`, not `lazy_static!`/`once_cell::sync::Lazy`, in new code. | `rg -n --type rust --glob '!external/**' -e 'lazy_static!' -e 'once_cell::sync::Lazy' .` — zero is the expected answer; vendored crates are excluded because we do not own them | SHOULD |
| ASYNC-20 | Do not use `thread_local!` for state that must survive an `.await` — a task resumes on a different worker after every await, so the read is stale or foreign. Use `tokio::task_local!`. | `rg -n --type rust --glob '!external/**' 'thread_local!' .`; no read/write pair may straddle an `.await` | CONSIDER |
| ASYNC-21 | Hand-rolled atomics need a comment naming the happens-before relationship: `Relaxed` for independent counters, `Acquire`/`Release` pairs for publish, `SeqCst` only for a genuine multi-location total order. | `rg -n --type rust --glob '!external/**' 'Ordering::[ARS]' .` — the class picks the five atomic orderings and drops `std::cmp::Ordering::Less`/`Greater`/`Equal` | CONSIDER |

ASYNC-02 is a CI gate, not a review habit. Pin it at the workspace root:

```toml
[workspace.lints.clippy]
await_holding_lock = "deny"
await_holding_invalid_type = "deny"
await_holding_refcell_ref = "deny"
```

## Deadlines, Bounds, and Retries

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ASYNC-04 | Every await on a network or subprocess operation carries a deadline — `tokio::time::timeout`, or a client-level timeout configured at construction. A wedged registry otherwise hangs the process forever with no exit code. | `rg -n -A6 --type rust --glob '!external/**' -e 'Client::builder\(' -e 'ClientBuilder::new\(' -e 'reqwest::Client::new\(\)' .` must show `.timeout(`/`.connect_timeout(`; anchor on the call, a bare `ClientBuilder` also matches the type's own definition and doc links, and `Client::new()` skips the builder so it carries no deadline at all. Registry call sites need a `timeout(` wrapper | MUST |
| ASYNC-05 | Bound every fan-out whose length is caller- or wire-controlled: `buffer_unordered(n)`, or a `JoinSet` gated by a `Semaphore`. Never `join_all`/`try_join_all` over an unbounded list. | `rg -n --type rust --glob '!external/**' -e 'join_all' -e 'try_join_all' -e 'FuturesUnordered::new' .`; each hit proves a small constant bound or is replaced | MUST |
| ASYNC-06 | Never use `mpsc::unbounded_channel` (or any uncapped queue) without a comment justifying why backpressure is inapplicable — an uncapped queue turns any consumer stall into unbounded memory growth. | `rg -n --type rust --glob '!external/**' 'unbounded_channel' .` | MUST |
| ASYNC-10 | Retries against the registry use jittered exponential backoff from a crate already in the tree (`backon` if adding one), never a hand-rolled fixed-delay `sleep` loop — fixed delays synchronise across clients and amplify load on a struggling registry. | `rg -n -B4 --type rust --glob '!external/**' 'time::sleep\(' .` — the sleep call is what the violation is made of; the words `retry`/`backoff` are prose and match hundreds of lines. Every hit inside a retry loop must sleep on a growing, jittered delay, never `sleep(fixed)` | MUST |

## Cancellation, Tasks, and Actors

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ASYNC-07 | Every `select!` branch must be built from a cancel-safe future, or the code must state in a comment what partial work is acceptable to lose. `select!` drops every losing branch outright; `read_exact`, `read_to_end`, `write_all`, `Mutex::lock`, `Semaphore::acquire` lose data or queue position. | Read every `tokio::select!` block; flag branches calling the non-cancel-safe list. Applies equally to `tokio::time::timeout` wrapping those calls | MUST |
| ASYNC-09 | Every `tokio::spawn` handle is `.await`ed, held in a `JoinSet`/`TaskTracker`, or explicitly detached with a comment saying why the failure may be ignored — a dropped `JoinHandle` does not cancel the task, it only discards its error. | `rg -n --type rust --glob '!external/**' 'tokio::(task::)?spawn\(' .`; any handle bound to `_` or dropped needs the comment | MUST |
| ASYNC-16 | An `Arc<Mutex<T>>` where `T` owns an I/O handle or does `.await` work internally should become an actor: one owning task plus a cloneable handle over an `mpsc` channel. Spawn in the handle constructor, never in a message-handling method — `spawn` needs `'static`, so per-call spawning leaks tasks. | Grep for `Arc<Mutex<` wrapping a client/connection/file handle; grep the handle `impl` for `tokio::spawn(` outside `fn new` | SHOULD |
| ASYNC-17 | Never build a cycle of bounded channels between tasks that can each block on a full `send().await` from inside their own receive loop — circular backpressure is a deadlock by construction. | Draw the message graph; any cycle of awaited bounded sends is a defect | SHOULD |

## Async Traits and Testing

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ASYNC-13 | Any test asserting timeout, retry, or backoff behaviour uses `#[tokio::test(start_paused = true)]` with `tokio::time::advance` — never a real `sleep` as a wait. Real-time sleeps are slow and flaky under CI load. | `rg -n --type rust --glob '!external/**' 'sleep\(Duration::from_' .` — flag hits inside `#[tokio::test]` bodies without `start_paused = true` | MUST |
| ASYNC-14 | New traits with async methods use native `async fn`; `#[async_trait]` is permitted only where a `dyn Trait`/`Box<dyn Trait>`/`Arc<dyn Trait>` site exists in the same crate. AFIT is stable since 1.75; the macro costs a heap allocation per call. | `rg -n --type rust --glob '!external/**' '#\[async_trait\]' .` and, per hit, `rg -n --type rust --glob '!external/**' 'dyn NameOfTrait' .`. No `dyn` site → delete the macro. Existing hits are not a backlog — see the note below the table | SHOULD |
| ASYNC-15 | When a trait's futures must cross a `tokio::spawn` boundary, add the `Send` bound with `#[trait_variant::make(… : Send)]` — never by hand-writing `Pin<Box<dyn Future + Send>>` in the trait. | `rg -n --type rust --glob '!external/**' 'Pin<Box<dyn Future' .` — any hit in a trait definition (not a hand-rolled `Future`/`Stream` impl) is obsolete | SHOULD |

Existing `#[async_trait]` usages are decided per site as a refactor touches
them. No bulk migration.

Concurrency correctness is tested with paused tokio time, not `loom` or
`shuttle` — those pay off for authors of lock-free data structures, and
this codebase has none.

## What Agents Get Wrong Here

1. **Porting a sync function to async by adding `async` to the signature**
   and calling the CPU- or FS-bound body directly. Compiles, passes tests,
   starves the runtime under load. The single most likely defect here.
2. **Reaching for `#[async_trait]` reflexively**, because training data is
   saturated with pre-1.75 code — plus its cousin, hand-writing
   `Pin<Box<dyn Future + Send>>` as a trait return type because it looks expert.
3. **Holding a `std::sync` guard across an `.await`** and concluding it is
   fine because it compiled. A `current_thread` test runtime does not reject
   `!Send` futures.
4. **Choosing `unbounded_channel` because it has no capacity argument to
   reason about** — the fewest-decisions API wins when the goal is code that compiles.
5. **Unbounded `join_all` over a caller-controlled list** ("download every
   layer"). Fine on a 3-layer test image; 429s and pool exhaustion on 200.
6. **Wrapping `read_to_end`/`write_all` in `select!` or `timeout` and
   assuming cancellation is clean.** The drop is safe; the buffer and stream
   position are not. No compiler pass catches this.
7. **Fixed-delay retry loops** (`loop { attempt; sleep(1s).await }`) instead
   of the backoff crate already in the tree.
8. **Dropping a `JoinHandle`** without reasoning about whether the task
   outlives the runtime or whether its error is now unobservable.
9. **`Ordering::SeqCst` on every atomic "to be safe"**, masking a design that
   wanted a `Mutex`, a channel, or a `OnceLock`.
10. **Hallucinated poisoning semantics** — `.lock().unwrap()` against a
    `tokio::sync::Mutex`, whose `lock()` returns no `Result` at all. A tell
    that std-Mutex-shaped code was copied without checking the type.
11. **`block_on` as an always-safe escape hatch** for calling async from sync
    deep in the call graph. It panics, and the panic lands far from the cause.

## Sources

- [tokio `select!` docs](https://docs.rs/tokio/latest/tokio/macro.select.html) — the cancel-safe / not-cancel-safe API lists
- [Alice Ryhl — Async: What is blocking?](https://ryhl.io/blog/async-what-is-blocking/) — the 10–100 µs rule
- [tokio `spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) — pool sizing, and why shutdown cannot cancel
- [tokio shared-state tutorial](https://tokio.rs/tokio/tutorial/shared-state) and [`tokio::sync::Mutex`](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html) — std-lock-first, and no poisoning
- [Actors with Tokio](https://ryhl.io/blog/actors-with-tokio/) — the canonical actor shape and the bounded-cycle deadlock
- [`StreamExt::buffer_unordered`](https://docs.rs/futures/latest/futures/stream/trait.StreamExt.html#method.buffer_unordered) — the bounded fan-out combinator
- [clippy `await_holding_lock`](https://rust-lang.github.io/rust-clippy/master/#await_holding_lock) — the ASYNC-02 gate
- [`tokio::time::pause`](https://docs.rs/tokio/latest/tokio/time/fn.pause.html) — `start_paused` semantics for deterministic timing tests
