# Performance

Performance rules for the OCX-family Rust CLIs (`ocx`, `grim`, `ocx-mirror`).
This project treats performance as two user-visible numbers: end-to-end
wall-clock per subcommand (cold cache and warm cache, reported separately) and
the bytes a user downloads to get the binary. Everything below those two is
folklore until a tool's output is pasted.

Contents: [Pinned Decisions](#pinned-decisions) ·
[Measurement Discipline](#measurement-discipline) ·
[CI Gating and Budgets](#ci-gating-and-budgets) ·
[Concurrency and I/O](#concurrency-and-io) ·
[Build and Distribution](#build-and-distribution) ·
[Data Layout and Allocation](#data-layout-and-allocation) ·
[Startup Latency](#startup-latency-and-the-interactive-budget) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers. **The mechanism** — measure before you optimise, bound your
fan-out, buffer your I/O, justify your profile keys — is general Rust practice
and portable. **The pinned decisions** below are this project's calls, already
paid for with measurements; they are not re-litigated in a PR.

## Pinned Decisions

- **hyperfine is the primary harness.** It measures whole subcommands, which is
  what users experience. A `criterion` bench earns its place only after a
  flamegraph names a specific function. `iai-callgrind`/Gungraun is rejected,
  not deferred: Linux-only, mid-rename, and a fix for a noise problem avoided
  by never gating on wall-clock.
- **Binary size outranks `opt-level = 3`.** `opt-level = "s"` measured 33.4 MB →
  18.6 MB at 9% on one compress+hash path. For a prebuilt binary, download bytes
  are latency on every install; 9% on an internal path is not.
- **`panic = "abort"` is banned** in the CLI binaries. See PERF-17.
- **Keep the default SipHash hasher.** Nearly every map key here — digest, ref,
  package name, index entry — arrives in a wire document from a registry, which
  is exactly the untrusted-input case the fast hashers carve out.
- **No `mmap`, no BLAKE3.** Store directories are multi-writer, and every
  durable digest must match an OCI manifest.

## Measurement Discipline

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PERF-01 | Never state a performance number you did not produce. Any commit, PR, or comment containing `%`, `x faster`, `speedup` or `optimiz` carries the hyperfine table, criterion block, or linked artifact that produced it. | `git log -p -E --grep '[0-9.]+[x%] faster' --grep speedup` for history, `rg -n --type rust --glob '!external/**' -e '[0-9.]+[x%] faster' -e speedup .` for comments — each hit needs adjacent tool output | MUST |
| PERF-02 | Benchmark release builds only, and benchmark the profile actually shipped — dev builds are 10–100x slower and measure the debug allocator. A `target/release` number in a repo that ships `target/dist` describes a binary nobody installs. | `rg -n hyperfine -g '*.sh' -g '*.yml' .` — every target path is the shipped profile | MUST |
| PERF-03 | Every hyperfine invocation declares cache state: `--prepare` (cold) or `--warmup N` (warm), never neither. Cache state alone is a >10x factor for a package manager. | `rg -nP '^\s*-? *hyperfine(?!.*--warmup)(?!.*--prepare)\s' -g '*.sh' -g '*.yml' .` returns nothing — same scope as PERF-02, and the leading anchor keeps it on real invocations rather than the prose, task deps and download steps that also spell `hyperfine` | MUST |
| PERF-04 | Report cold-cache and warm-cache as two separate budget lines. They are two different user promises, and a blended number hides a regression in either. | Review: a single unqualified "`grim install` takes Xs" is a defect | MUST |
| PERF-05 | In a criterion/divan benchmark wrap both the input and the returned value in `std::hint::black_box` — wrapping only the input still lets LLVM delete a pure computation whose result is unused. | Each `fn bench_*` has `black_box(` inside the call arguments *and* around the call expression; a reported >90% win is a prompt to re-read for this | MUST |
| PERF-06 | Never emit `#![feature(test)]`, `extern crate test`, or `#[bench]` — nightly-only since 2015 with no stabilization path, and the toolchain is pinned stable, so it will not build. | `rg -n -g '*.rs' -e 'feature\(test\)' -e 'extern crate test' -e 'test::Bencher' .` → zero hits | MUST |

## CI Gating and Budgets

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PERF-07 | No CI job fails a build on a wall-clock benchmark number while running on a GitHub-hosted runner. ~2.66% CoV on shared runners makes a 2% gate ~45% false-positive and trains everyone to ignore it. The fix is isolated hardware, not a looser threshold. | Under `runs-on: ubuntu-latest`/`macos-*`/`windows-*`, any timing comparison is advisory (`continue-on-error: true` or comment-only) | MUST |
| PERF-08 | The binary-size gate compares `stat -c%s` on the shipped binary against a checked-in byte threshold. `cargo bloat` is advisory only — its own docs call the attribution "guesswork", so it explains *why* and cannot be the gate. | Any CI step parsing `cargo bloat` output into a failure condition is a defect | SHOULD |
| PERF-09 | Every regression threshold in CI config carries an adjacent comment citing the variance measurement it was derived from, re-measured on that runner. | `rg -n -e alert-threshold -e noise-threshold .` plus size thresholds — a bare number with no justification comment is a defect | SHOULD |

## Concurrency and I/O

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PERF-10 | Every fan-out over registry or network calls goes through `tokio::sync::Semaphore` or `StreamExt::buffer_unordered(n)`. Never a raw `join_all` or unbounded `FuturesUnordered` — ghcr.io rate-limits and resets connections under unbounded fan-out. | `rg -n --type rust --glob '!external/**' join_all .` — every hit over an HTTP/OCI client call needs a `Semaphore`/`buffer_unordered` in the same function or an explanatory comment | MUST |
| PERF-11 | Acquire the concurrency permit **before** spawning the task, not inside the task body — a semaphore gating only the fetch body still lets a wide frontier spawn thousands of tasks and hold their memory. | For each `JoinSet`/`spawn` in a loop, `acquire()`/`acquire_owned()` appears lexically outside the spawned future, not as its first statement | MUST |
| PERF-12 | Never `mmap` (`memmap2`, `Mmap`, `MmapMut`). Concurrent external truncation of a mapped file is UB, and the store and cache directories are multi-writer by design. | `rg -n -g '*.rs' -e memmap -e Mmap .` → zero hits | MUST |
| PERF-13 | Wrap repeated small file reads and writes in `BufReader`/`BufWriter`, and take `stdout().lock()` once outside any printing loop — Rust file I/O is unbuffered by default and `println!` re-locks stdout on every call. | Any `File::open`/`File::create` feeding a read/write loop without a `Buf*` wrapper, or a `println!` inside a loop with no outer lock | SHOULD |
| PERF-14 | Pre-size collections with `with_capacity`/`reserve` when the source length is known or size-hinted — reaching ~20 elements costs 4 reallocations under the default doubling schedule. | `Vec::new()`/`HashMap::new()` immediately followed by a loop over a `.len()`-bearing source | SHOULD |
| PERF-15 | Do not call sync `std::fs::*` inside an `async fn`; use `tokio::fs` or a `spawn_blocking` wrapper whose doc comment names the sync primitive it bridges. It blocks a runtime worker thread. | Not grep-reliable: `clippy::await_holding_lock` plus review of any new `std::fs::` call added to a file containing `async fn` | SHOULD |

## Build and Distribution

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PERF-16 | Every key in a `[profile.*]` block carries a comment with the measured number that justified it. `lto`, `codegen-units` and `opt-level` are runtime-vs-build-time-vs-size trades, not free wins. | Read `Cargo.toml`: an uncommented profile key is a defect. Absent `lto`/`codegen-units` means Cargo defaults (`lto = false`, `codegen-units = 16`) are in force by omission — confirm that is intended | MUST |
| PERF-17 | Never set `panic = "abort"` in `ocx`, `grim`, or `ocx-mirror`. It silently removes the unwind mechanism behind every `resume_unwind(join_err.into_panic())` site that propagates spawned-task panics, and it compiles clean. Two independent research rounds have now recommended it generically as a standard release lever; both are overruled by the in-tree measurement. | `rg -n 'panic *= *"abort"' -g Cargo.toml .` — only a dependency-free launcher shim profile with no spawned tasks may match | MUST |
| PERF-18 | Pin compression backend features explicitly; never inherit a crate's default backend. `flate2`'s features are additive and resolve by priority, so an unpinned backend means throughput changes on a `cargo update` — and its default `miniz_oxide` is the slowest documented option. | Two commands, both across every manifest in the repo: `rg -n --glob '**/Cargo.toml' --glob '!external/**' -e flate2 -e zstd -e async-compression .` is the inventory of declarations to eyeball; `rg -nP --glob '**/Cargo.toml' --glob '!external/**' -e '^flate2 *=(?!.*features)' -e '^zstd *=(?!.*features)' -e '^async-compression *=(?!.*features)' .` is the gate and must return nothing — a member's `.workspace = true` line inherits the root pin and is correctly not a hit | SHOULD |

The reference shape, and what PERF-16/17 are asking for:

```toml
[profile.dist]
opt-level = "s"      # 33.4 MB -> 18.6 MB; costs 9% on the compress+hash path
codegen-units = 1    # link 96s -> 187s, buys ~1.1 MB
lto = "fat"
strip = "symbols"
# panic = "abort" is deliberately absent: it drops spawned-task panic
# propagation silently and still compiles. See PERF-17.
```

## Data Layout and Allocation

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PERF-19 | Do not replace the default `HashMap`/`HashSet` hasher without (a) a profile showing the map is hot and (b) a written argument that its keys are not attacker-supplied. FxHash and foldhash are faster and explicitly not DoS-resistant; `ahash` is not a free upgrade either — rustc measured 1–4% *slowdowns* switching to it. | `rg -n -g '*.rs' -e rustc_hash -e FxHash -e ahash -e foldhash .` — currently zero hits, and that is the intended steady state; each new hit needs both justifications in a comment | MUST |
| PERF-20 | Do not introduce `SmallVec`/`ArrayVec`/`CompactString` without a schema-enforced upper bound on the data. `SmallVec` adds a branch to every access; `ArrayVec` panics or truncates past its fixed `N`. "Usually small in practice" is not a bound. | For each such type, point at the schema field or protocol constant that caps the length; otherwise `Vec` | SHOULD |
| PERF-21 | Never add `#[repr(C)]` for performance — it *disables* the compiler's size-minimizing field reordering and only matters for FFI ABI stability. | `rg -n -A3 '#\[repr\(C\)\]' -g '*.rs' .` — every hit sits at an `extern "C"` boundary | MUST |
| PERF-22 | Box an enum variant that is both rare and much larger than its siblings; do not silence `clippy::large_enum_variant` without a comment. Types over 128 bytes are `memcpy`'d on move; every instance pays for the largest variant. Boxing a *hot* variant is a pessimization, so this is conditional. | `cargo clippy` — an allowed `large_enum_variant` needs an adjacent rationale; add `const _: () = assert!(size_of::<T>() <= N);` on error enums crossing task boundaries | CONSIDER |
| PERF-23 | Extract the non-generic body of a generic function into a separate non-generic `fn` once the body is more than a few lines — each instantiation duplicates the whole body, in compile time and icache. Monomorphisation across a 304-crate graph is what cost 6.9 MB and forced `opt-level = "s"`. | `cargo llvm-lines --release` — its rows already sort by Lines descending; a generic function with a high Lines × Copies product is the trigger | CONSIDER |
| PERF-24 | Defer startup-optional initialization behind `std::sync::LazyLock`/`OnceLock` — a CLI invocation should not pay for regexes, config parsing, or lookup tables it never touches. | `rg -n -g '*.rs' -e 'static [A-Z_]+: *Vec' -e 'static [A-Z_]+: *HashMap' -e 'static [A-Z_]+: *Regex' .` for eager statics | SHOULD |

## Startup Latency and the Interactive Budget

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PERF-25 | Measure startup with `hyperfine --shell=none --warmup N`, never bare `time` and never a shell `for` loop. Below ~5 ms hyperfine's shell-startup correction is noisier than the thing measured — exactly the regime a `--version` invocation lives in — and a hand-rolled loop re-adds fork/exec to every sample. | `rg -nP '^\s*-? *hyperfine(?!.*--shell=none)(?!.* -N )\s' -g '*.sh' -g '*.yml' .` — same scope and anchor as PERF-03; a startup benchmark without it is a defect, PERF-03's cache declaration still applies on top | MUST |
| PERF-26 | Never downgrade a runtime to `current_thread` without first checking the reachable call graph for `block_in_place`; `ocx` stays multi-thread. `Handle::block_on` inside `block_in_place` panics on a `current_thread` runtime, so this is a correctness constraint, not a perf trade — and it overrides the otherwise-sound "a CLI does not need worker threads". | `rg -n block_in_place -g '*.rs' .` before touching any runtime construction — a flavor change in a crate with a hit, or with a `runtime_flavor()` assertion, is a defect | MUST |
| PERF-27 | A hand-built `tokio::runtime::Builder` names the drivers it needs (`enable_io()`/`enable_time()`) instead of `enable_all()`, or carries a comment saying why all are needed. Drivers are opt-in on a hand-built builder, so `enable_all()` is a signal nobody checked which the runtime uses. | `rg -n -A5 -g '*.rs' -e Builder::new_current_thread -e Builder::new_multi_thread .` — each hit shows named drivers or a rationale comment | SHOULD |
| PERF-28 | Never claim a startup or runtime win from swapping the global allocator (mimalloc/jemalloc), from musl/`+crt-static` linking, or from PGO without a same-hardware `hyperfine --shell=none` before/after pair in the same commit — these are the four reflex moves and no published number exists for any of them. BOLT is not adopted at all: its own README scopes it to warmed-up long-running services. | `rg -n -e global_allocator -e crt-static -e profile-use -e profile-generate -e llvm-bolt .` — currently zero hits and that is the intended steady state; each new hit needs the pair, and BOLT hits are a defect regardless | MUST |
| PERF-29 | A subcommand whose work stays under ~100 ms shows no progress indicator; one that can cross ~1 s must show one. The bands are Miller (1968) and Card, Moran & Newell (1991) via NN/g, not a folklore round number, and they are the only startup threshold this project asserts. | Any function issuing a registry fetch or an unbounded directory walk has an `indicatif` handle or a status `eprintln!` in scope | SHOULD |
| PERF-30 | Use `strace -c` to establish what a startup actually costs before optimizing it, and treat the syscall count as a diagnostic, never a gate — config and dotfile probing is invisible in code review but shows up immediately as `openat` counts. | `strace -c target/dist/ocx --version` before any startup change; a claimed startup win with no syscall or hyperfine evidence is PERF-01, and a blocking threshold on the count is PERF-07/09 | CONSIDER |

## What Agents Get Wrong Here

1. **Asserting a speedup that was never measured.** "3x faster because it avoids
   an allocation", written as a conclusion in a commit message or a comment,
   with no tool ever run. The most common failure by a wide margin.
2. **Unbounded `join_all` over registry calls "for speed."** Locally it looks
   like a win; against ghcr.io it is a rate-limit incident. The agent optimizes
   the only variable it can see.
3. **Benchmarking a debug build**, or benchmarking `target/release` when the
   shipped artifact is `target/dist` with different LTO settings.
4. **`black_box` on the input only.** LLVM deletes the unused result, the
   benchmark reports near-zero, and the agent reports that as the win.
5. **Reflexive `SmallVec`/`ArrayVec`/`CompactString`** on an unbounded
   collection — pure branch overhead, or a panic on real input past `N`.
6. **`#![feature(test)]` / `#[bench]` from stale training data.** Does not
   compile on a pinned stable toolchain.
7. **Copying a CI perf threshold out of a blog post** onto a shared runner,
   guaranteeing constant false failures.
8. **`unsafe { mmap }` for "fast file reading"** on a store path a concurrent
   process may write. Compiles, reads idiomatically, is UB, and clippy cannot
   see it — which is why PERF-12 is a ban and not a judgement call.
9. **Hallucinated hasher APIs** — `FxHashMap::new()` (only `default()` exists),
   or the type imported from `std::collections`.
10. **`#[repr(C)]` added "for performance"**, disabling the exact layout
    optimization the agent believed it was enabling.
11. **`Arc<Mutex<T>>` as the default shared-state shape**, then optimizing the
    `.clone()` — a free refcount bump — while ignoring the lock contention that
    is the actual cost.
12. **The four reflex startup "fixes"** — mimalloc, musl-static, PGO, BOLT.
    Asked to make a CLI start faster an agent reaches for all four because they
    sound standard; all four are exactly where no published number exists.
13. **Downgrading a runtime to `current_thread`** "because a CLI does not need
    worker threads." Correct in general, a panic in `ocx`, and the in-tree
    `debug_assert!` catches it only in debug builds.

## Sources

- [perf-book](https://nnethercote.github.io/perf-book/general-tips.html) — measurement-first framing, [build configuration](https://nnethercote.github.io/perf-book/build-configuration.html), [heap allocations](https://nnethercote.github.io/perf-book/heap-allocations.html), [hashing](https://nnethercote.github.io/perf-book/hashing.html), [type sizes](https://nnethercote.github.io/perf-book/type-sizes.html), [I/O](https://nnethercote.github.io/perf-book/io.html)
- [hyperfine](https://github.com/sharkdp/hyperfine) — `--warmup`/`--prepare`/`--shell=none`, shell-overhead correction
- [`std::hint::black_box`](https://doc.rust-lang.org/std/hint/fn.black_box.html) — what it does and does not guarantee
- [CodSpeed: benchmarks in CI without noise](https://codspeed.io/blog/benchmarks-in-ci-without-noise) — 2.66% CoV, 45% false positives, 0.56% bare metal
- [astral.sh/blog/uv](https://astral.sh/blog/uv) — the cold/warm-cache split methodology for a package-manager CLI
- [memmap2](https://docs.rs/memmap2/latest/memmap2/) · [foldhash](https://docs.rs/foldhash/latest/foldhash/) · [flate2](https://docs.rs/flate2/latest/flate2/) — the UB boundary, the "minimally DoS-resistant" disclaimer, and backend priority resolution
- [cargo-bloat](https://github.com/RazrFalcon/cargo-bloat) — its own "guesswork" accuracy disclaimer · [rust-lang/rust#66287](https://github.com/rust-lang/rust/issues/66287) — why `#[bench]` is permanently nightly
- [NN/g response-time limits](https://www.nngroup.com/articles/response-times-3-important-limits/) — Miller 1968 / Card, Moran & Newell 1991, the 0.1 s and 1 s bands behind PERF-29
- [tokio runtime](https://docs.rs/tokio/latest/tokio/runtime/index.html) — `current_thread` vs `multi_thread`, and drivers being opt-in on a hand-built builder · [LLVM BOLT](https://github.com/llvm/llvm-project/tree/main/bolt) — its own deploy-and-warm-up workflow · [strace(1)](https://man7.org/linux/man-pages/man1/strace.1.html) — startup syscall accounting
