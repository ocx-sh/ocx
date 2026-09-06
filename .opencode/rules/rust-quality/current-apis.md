# Current APIs

A model reaches for the API it saw most during training, and that is the API
of two to four years ago. The recall arrives phrased as fact, not as a guess —
no hedge, no "I think this was renamed." This file is the lookup that replaces
the guess. Loads with the Rust quality rule whenever new code calls into std
or a third-party crate.

Contents: [Use This, Not That](#use-this-not-that) · [Rules](#rules) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

## Use This, Not That

Which crate to reach for per job is a different question — that table lives in
[the cargo rule](../rust-cargo.md). This one is about drift inside crates you
already depend on.

| Reaches for | Correct in 2026 | Since | The tell you got the old one |
|---|---|---|---|
| `static mut X` + `unsafe { X += 1 }` | `AtomicU64`, `Mutex`/`RwLock`, `LazyLock`, `OnceLock`; `&raw mut`/`&raw const` only for FFI | ed. 2024 | `error: creating a mutable reference to mutable static` — deny-by-default, no `cargo fix` |
| `unsafe fn` body doing unsafe ops bare | explicit `unsafe {}` *inside* the `unsafe fn` | ed. 2024 | `warning: unsafe_op_in_unsafe_fn` |
| `extern "C" { fn … }` | `unsafe extern "C" { … }`, per-item `safe`/`unsafe` markers | ed. 2024 | `missing_unsafe_on_extern` |
| `#[no_mangle]`, `#[export_name]`, `#[link_section]` | `#[unsafe(no_mangle)]` etc. | ed. 2024 | hard error on a 2024 crate |
| `env::set_var("K", "v")` | `unsafe { env::set_var(…) }` **and** a serialization guard | 1.85, all editions | "call to unsafe function" in test setup |
| RPIT that "doesn't capture" the input lifetime | 2024 captures every in-scope generic and lifetime; `+ use<>` restores the old set | ed. 2024 | callers break at *their* site after a signature change, definition still compiles |
| `trait Captures<'a> {}` + `impl Trait + Captures<'a>` | `+ use<'a, T>` precise capturing | 1.82 free fns, **1.87** RPITIT | a `Captures` helper trait anywhere in the file |
| `impl Trait + '_` as a capture workaround | `+ use<…>` names exactly what is captured | 1.82 | `'_` on a return type with no borrow reason |
| `gen` as an identifier | `r#gen`, or rename | ed. 2024 | `keyword_idents_2024` |
| `gen { … }` generator blocks | **still unstable.** Reserved ≠ shipped | — | reaching for it off a changelog memory |
| nested `if let` pyramids | `let … else { return … };` (1.65) and `if let A = a && let B = b` chains — chains are **2024-only** | 1.88 | parse error with *no* edition hint on a 2021 crate |
| `Fn() -> impl Future` closure workarounds | `async` closures; `AsyncFn`/`AsyncFnMut`/`AsyncFnOnce` | 1.85 | hand-rolled `Box::pin(async move {…})` in a closure argument |
| `#[async_trait]` on every trait | native `async fn` in traits — but **not** dyn-compatible, so keep `#[async_trait]` where the trait is stored as `dyn` | 1.75 | `async_trait` on a trait that is only ever used generically |
| `lazy_static! { static ref X … }` | `std::sync::LazyLock` | 1.80 | a `lazy_static` dependency with no non-std reason |
| `once_cell::sync::Lazy` / `OnceCell` | `LazyLock` / `OnceLock`. Keep `once_cell` only for non-`Sync` `OnceCell` or reentrant patterns, with a comment saying which | 1.80 | direct `once_cell` dep for a plain global |
| `LazyLock` treated like a poisoned `Mutex` | **unrecoverable** — a panicking init closure poisons every future access forever; there is no `PoisonError::into_inner` | 1.80 | fallible init inside `LazyLock::new` with no test |
| `#[allow(lint)]` | `#[expect(lint, reason = "…")]` — self-expires when the lint stops firing | 1.81 | a bare `#[allow]` with no reason string |
| `rand::thread_rng()` | `rand::rng()` (also out of the prelude) | rand 0.9 | unresolved import from the prelude |
| `Rng::gen()`, `gen_range()`, `gen_bool()`, `gen_ratio()` | `random()`, `random_range()`, `random_bool()`, `random_ratio()` | rand 0.9 | the `gen` keyword reservation forced this rename |
| `rand::distributions::Standard` | `rand::distr::StandardUniform` | rand 0.9 | `distributions` module not found |
| `SliceRandom` for everything | split into `IndexedRandom`, `IndexedMutRandom`, `SliceRandom` | rand 0.9 | `choose` not found on the trait imported |
| `rand::rngs::OsRng`, the `Rng` trait, `os_rng` feature | `SysRng`, `RngExt` (`rand_core` renamed `RngCore`→`Rng`), `sys_rng` | **rand 0.10**, Feb 2026 | knowing only the 0.9 table is still one major version stale |
| transitive `thiserror` derive via a re-export | every crate invoking `derive(Error)` lists `thiserror` **directly** | thiserror 2 | derive macro resolves in the parent crate, not the child |
| `#[error("{r#type}")]` | `#[error("{type}")]` — unraw the field name | thiserror 2 | raw-identifier interpolation rejected |
| `clap::App`, `Arg::with_name`, `AppSettings::*` | `clap::Command`, `Arg::new`, builder methods | clap 4 | `App` not found in `clap` |
| `structopt::StructOpt` derive | `clap::Parser` derive | clap 3+ | a `structopt` dependency at all |
| `syn::Meta`/`NestedMeta` attribute walking, `syn::export` | syn 2 `attr.parse_nested_meta(…)`, `attr.meta` | syn 2 | proc-macro crate that will not build against syn 2 |
| hyper 0.14 / http 0.2 types at a reqwest seam | hyper, http, http-body **1.x** | reqwest 0.12 | two `http` versions in `cargo tree` |
| `native-tls` as reqwest's default, feature `rustls-tls` | `rustls` is the default backend; the feature is now `rustls`; roots via `rustls-platform-verifier` | reqwest 0.13 | `rustls-tls` feature not found |
| rustls defaulting to `ring` | default provider is **aws-lc-rs**; there is no `rustls-tls-ring` feature on reqwest 0.13 — ring needs a direct `rustls` dep with its `ring` feature | reqwest 0.13 | a C compiler suddenly required on a cross target |
| `query`/`form` always available on reqwest | separate opt-in features | reqwest 0.13 | method missing on `RequestBuilder` |
| `error-chain`, `failure` | `thiserror` for libraries, `anyhow` for binaries | long dead | either name in a manifest |
| `async-std` | tokio (or smol) | discontinued upstream | `async_std::task::spawn` |

**Pinned decisions.** Accept **aws-lc-rs**: the release matrix is per-target
native runners and containers, so its always-required C compiler costs
nothing here. A future move to single-host cross-compilation (`cross`,
zig-cc) re-opens the ring choice — do not switch silently. And the
`default-features = false, features = ["rustls"]` pin on `reqwest` is
load-bearing, not redundant boilerplate: it is what keeps exactly one crypto
provider in the graph.

## Rules

| ID | Rule | Verification | Severity |
|---|---|---|---|
| EVO-1 | Read `edition` and `rust-version` from the target crate's `Cargo.toml` before emitting any edition-gated syntax (let-chains, `gen`, `unsafe extern`, async closures). | `git grep -n -e '^edition' -e '^rust-version' -- '*Cargo.toml'`; if `edition < 2024`, no let-chains | MUST |
| EVO-2 | Never add `#[allow(static_mut_refs)]`, `#[allow(unsafe_op_in_unsafe_fn)]` or `#[allow(missing_unsafe_on_extern)]` to clear an edition-2024 error — the lint *is* the defect report. | `git grep -n -e 'allow(static_mut_refs' -e 'allow(unsafe_op_in_unsafe_fn' -e 'allow(missing_unsafe_on_extern' -- '*.rs'` — any match blocks the merge | MUST |
| EVO-3 | No `static mut`. Process-global mutable state is `Atomic*`, `Mutex`/`RwLock`, `LazyLock`, or `OnceLock`. | `git grep -n 'static mut ' -- '*.rs'` returns nothing | MUST |
| EVO-4 | Every new `unsafe {}` block carries a `// SAFETY:` comment naming the invariant — per block, not per function. | `clippy::undocumented_unsafe_blocks` in `[lints.clippy]`; existing sites go on a backfill list, not a day-one CI gate | MUST (new code) / SHOULD (backfill) |
| EVO-5 | New FFI declares `unsafe extern "C" { … }` with per-item `safe`/`unsafe`, and `#[unsafe(no_mangle)]`/`#[unsafe(export_name)]`/`#[unsafe(link_section)]`. A mechanical `cargo fix` cannot check signatures. | `git grep -n -e '^extern "C"' -e '^extern "system"' -- '*.rs'` returns nothing on a 2024 crate | MUST |
| EVO-6 | After changing the signature of a function returning `impl Trait`, re-derive what it captures; add `+ use<…>` where the set must be pinned. | `cargo build` on all dependents; `git grep -n -e '-> impl ' -- '*.rs'` for the affected signatures. RPITIT `use<…>` needs Rust ≥ 1.87 | SHOULD (MUST if the crate is published) |
| EVO-7 | Suppress with `#[expect(lint, reason = "…")]`, never bare `#[allow]`. Convert an `#[allow]` whenever you touch its line. | `git grep -c '#\[allow(' -- '*.rs'` must trend down; a new `#[allow]` in a diff needs a justification in review | SHOULD |
| EVO-8 | `LazyLock`/`OnceLock` for lazy globals; `once_cell`/`lazy_static` only for API std lacks, with a comment saying which. Never treat `LazyLock` poisoning as recoverable. | `cargo tree -e normal -i once_cell`, `-i lazy_static` — no match is clean; every direct edge justified or removed | SHOULD |
| EVO-9 | Before calling into a high-churn crate (`rand`, `thiserror`, `reqwest`, `rustls`, `clap`), read the pinned version from `Cargo.lock` and check the API against *that version's* docs. Never generate symbol names from memory. | `grep -A1 '^name = "rand"' Cargo.lock`, then `docs.rs/<crate>/<version>`; stale-name sweep `git grep -n -e 'thread_rng()' -e '\.gen()' -e '\.gen_range(' -e 'rand::distributions' -e 'rand::rngs::OsRng' -- '*.rs'` | MUST |
| EVO-10 | Exactly one rustls crypto provider is reachable. Do not "simplify" a TLS feature pin or add a second TLS-using dependency without re-checking — `install_default()` succeeds at most once per process, so a conflict is a *runtime* failure on the first handshake. | `cargo tree -e features -i rustls`, `-i ring`, `-i aws-lc-rs` — confirm one backend is *enabled* (lock edges include optional deps and prove nothing); plus one test doing a real handshake | MUST |
| EVO-11 | A dependency change touching the TLS or crypto backend is validated by the real release target matrix, not a host `cargo check`. | Run the full `dist-workspace.toml` target list in CI on the bumping PR; a host-only green check is not evidence | MUST |
| EVO-12 | Ban `async-std`, `structopt`, `error-chain` and `failure` in `deny.toml`'s `[[bans.deny]]` — a transitive upgrade could reintroduce one silently. | `cargo deny check bans` in CI; `cargo tree -i <crate>` to locate the path | SHOULD |
| EVO-13 | Wrap every `env::set_var`/`remove_var` in `unsafe` **and** serialize it — one owning test, a mutex-guarded helper, or `#[serial]`. The keyword alone silences the compiler and leaves the data race. | `git grep -n -e 'env::set_var' -e 'env::remove_var' -- '*.rs'` — every hit inside a documented single-owner or serialization convention | MUST |
| EVO-14 | Any crate invoking `#[derive(thiserror::Error)]` lists `thiserror` as its own direct dependency; unraw field names in `#[error("…")]`. | `git grep -l 'derive(.*Error' -- '*.rs'` and `git grep -l '^thiserror' -- '*Cargo.toml'` — the two crate sets must match | MUST |
| EVO-15 | Declare `rust-version` matching the pinned `rust-toolchain.toml` channel — with `resolver = "3"` the MSRV-aware resolver does nothing without it, and a routine `cargo update` can pull a crate needing a newer compiler than the pin. | `git grep -n '^rust-version' -- '*Cargo.toml'` equals the `rust-toolchain.toml` channel | SHOULD |

When the pinned docs are unreachable, do not fall back to memory. Read the
vendored source in `~/.cargo/registry`, or state the uncertainty and stop —
a confident wrong symbol costs more than a question.

## What Agents Get Wrong Here

1. **Stale crate-API recall delivered as fact.** `rand::thread_rng()`,
   `gen_range`, a reqwest 0.11-era builder chain — written flat, no hedge.
   Highest frequency by a wide margin. Prompting does not fix it; reading
   `Cargo.lock` then that version's docs does.
2. **Silencing the compiler to close the diff.** The error is real; `#[allow]`
   is the shortest path to green. Every existing `#[allow]` in the tree is an
   invitation to add the next one.
3. **Edition-gated syntax with no edition check.** A let-chain in a 2021 crate
   is a parse error with no edition hint, so the "fix" is mangling correct
   logic. Highest risk in ports and vendored forks.
4. **Treating `cargo check` as proof.** Crypto-provider conflicts (runtime),
   cross-target C-toolchain gaps (other machine) and env-var races (other
   thread) are all invisible to it.
5. **Feature-pin "simplification".** `default-features = false, features =
   ["rustls"]` looks redundant to anyone who has not met feature unification.
   Deleting it merges two providers and fails at the first handshake. Read the
   comment above a dependency before editing the line.
6. **Assuming reserved means available.** `gen { … }` blocks are still
   unstable; the keyword reservation was preparatory.
7. **Blanket modernization.** "Replace `once_cell` with `LazyLock`" applied
   without checking whether unrecoverable poisoning or the non-`Sync`
   `OnceCell` API was the reason for the dependency.

## Sources

- [Rust 1.85.0](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/) — edition 2024 ships; `static_mut_refs`, `unsafe_op_in_unsafe_fn`, `unsafe extern`, unsafe `env::set_var`, async closures
- [Rust 2024 Edition Guide](https://doc.rust-lang.org/edition-guide/rust-2024/index.html) — canonical list of every breaking change, with per-item migration pages
- [rust-lang/rust RELEASES.md](https://raw.githubusercontent.com/rust-lang/rust/master/RELEASES.md) — per-version stabilization ground truth (1.82 and 1.87 precise capturing, 1.88 let-chains)
- [Rust 1.88.0](https://blog.rust-lang.org/2025/06/26/Rust-1.88.0/) — let-chains, and why they are 2024-only
- [`std::sync::LazyLock`](https://doc.rust-lang.org/std/sync/struct.LazyLock.html) — stabilization and the unrecoverable-poisoning contrast with `Mutex`
- [rand CHANGELOG](https://github.com/rust-random/rand/blob/master/CHANGELOG.md) — the 0.9 and 0.10 rename tables
- [thiserror 2.0.0](https://github.com/dtolnay/thiserror/releases/tag/2.0.0) — direct-dependency requirement, dropped `{r#type}`
- [rustls `CryptoProvider`](https://docs.rs/rustls/latest/rustls/crypto/struct.CryptoProvider.html) and [aws-lc-rs build requirements](https://aws.github.io/aws-lc-rs/requirements/) — once-per-process install, always-required C compiler
