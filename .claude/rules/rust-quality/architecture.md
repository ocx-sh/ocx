# Type-Driven Architecture

Where behaviour lives and where boundaries fall: free function vs method vs
trait, newtypes and parse-don't-validate, the dispatch ladder, I/O seams,
module visibility, and the shape of the workspace. Loads with the Rust
quality rule whenever types, traits, modules or crate boundaries are in play.

Contents: [Where Behaviour Lives](#where-behaviour-lives) ·
[Traits and Dispatch](#traits-and-dispatch) · [I/O Seams and Ports](#io-seams-and-ports) ·
[Modules and Visibility](#modules-and-visibility) · [Workspace and Crates](#workspace-and-crates) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

## Where Behaviour Lives

This is deliberately two-sided. A blanket "turn free functions into methods"
is the pressure that grows a 600-method god struct: every new behaviour lands
on the nearest big type because a receiver was *imaginable*. The real signal
is mechanical — a repeated parameter tuple is a struct nobody wrote
(ARCH-01). A function with no privileged argument stays free (ARCH-02). And
the type that would otherwise absorb everything is capped (ARCH-03). The
three rules are one rule; applying any of them alone produces the defect the
other two prevent.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ARCH-01 | Three or more functions in a module sharing the same leading parameter pair/triple: introduce a struct holding those values, convert the functions to `&self`/`&mut self` methods. | `rg -l --pcre2 -U --type rust --glob '!external/**' '(?s)fn \w+\(\s*\w+: (&?(?:mut )?[A-Z]\w+)[^,)]*,.*?fn \w+\(\s*\w+: \1[^,)]*,.*?fn \w+\(\s*\w+: \1[^,)]*,' .` — the backreference makes the file list *be* the finding: three signatures sharing one leading type. Module-scoped: discard hits outside the module your change touches | MUST |
| ARCH-02 | Keep a free function when it has no privileged argument (a pure step in a pipeline), or when making it a method would create a dependency cycle — an error-to-exit-code classifier is a free function *by design*. | Name the receiver it would take. `()`, a config the caller already owns, or a type in a lower layer → it stays free | MUST |
| ARCH-03 | A single type gets at most **2** inherent `impl` blocks and **25** inherent methods. Past that, split the method clusters into cooperating types — field-count god-struct checks never catch this. | `rg -l --pcre2 -U --type rust --glob '!external/**' '(?s)^impl(?:<[^>]*>)?\s+(\w+)(?:<[^>]*>)?\s*\{.*?^impl(?:<[^>]*>)?\s+\1(?:<[^>]*>)?\s*\{.*?^impl(?:<[^>]*>)?\s+\1(?:<[^>]*>)?\s*\{' .` — the trailing brace excludes trait impls and the backreference ties all three blocks to one type, so every file listed is a finding; over 25 `fn` in one block is the same finding by inspection | MUST |
| ARCH-23 | When the borrow checker rejects two simultaneous borrows through one struct and each field path would type-check alone, split the struct along that seam — never reach for `.clone()`, `RefCell` or `Arc<Mutex>` instead. The compiler has named the cut; STATE-21/23/24 forbid the three ways of ignoring it and none of them names the fix. Distinct from ARCH-01/03: those push work *into* a type on a method count, this pushes fields *out* of one on a compiler error. | No lint — the trigger is an E0499/E0502 citing two field paths off one receiver. On a diff, the failure signal is a borrow-checker fix that adds a wrapper instead of moving a field | SHOULD |
| ARCH-24 | Encode a state machine's legal transitions as types when the transition graph is known at compile time: one marker type per state, each transition consuming `self` and returning the next type, each operation defined only on the states where it is legal. ARCH-05's argument — enum over `bool` for a closed set — applied to sequences instead of values. | Trigger: an enum field named `state`/`phase`/`status` read by a `match` at the top of three or more methods, each carrying an "invalid in this state" arm. Applied, the illegal call becomes a `cargo build` error rather than a runtime check | CONSIDER |
| ARCH-04 | Any `String`/`&str`/`Vec<u8>` carrying a format invariant — digest, registry reference, credential, relative extraction path, version — is parsed once at the trust boundary into a newtype with a **private** field and a fallible constructor, and never re-validated downstream. | `rg -n --type rust --glob '!external/**' -e 'digest\s*:\s*&?[Ss]tr' -e 'reference\s*:\s*&?[Ss]tr' -e 'token\s*:\s*&?[Ss]tr' -e 'credential\s*:\s*&?[Ss]tr' -e 'url\s*:\s*&?[Ss]tr' -e 'path\s*:\s*&?[Ss]tr' .` — each hit is a parameter or field owing a newtype, and a mature tree is never at zero, so restrict to added lines on a diff; `rg -n --type rust --glob '!external/**' 'pub struct \w+\(pub ' .` must be empty | MUST |
| ARCH-05 | No bare `bool` or open `String` parameter for a closed set of choices — the compiler exhaustiveness-checks an enum and cannot check a string. | `rg -n --type rust --glob '!external/**' 'fn \w+\([^)]*:\s*bool' .` — each hit must be a genuine yes/no, not a mode; a mature tree is never at zero, so restrict to added lines on a diff: `git diff --name-only -G'fn \w+\([^)]*:\s*bool' origin/main -- '*.rs'` | SHOULD |
| ARCH-06 | Never `impl Deref`/`DerefMut` except on a real smart pointer owning exactly one inner value. Deref-as-inheritance poisons method resolution. | `rg -n --type rust --glob '!external/**' 'impl(<[^>]*>)?\s+(std::ops::)?Deref(Mut)?\s+for' .` — every hit justified in review; `DerefMut?` matched `DerefMu`, never plain `Deref`, and read clean over real impls | MUST |

## Traits and Dispatch

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ARCH-07 | Define a trait only when there is a **second real implementation** or a **test double the suite actually exercises**. One impl and no double → inherent `impl`. The double must be able to reproduce the failures that matter: this is why a registry transport or credential store earns a port and the **filesystem does not** — no fake produces `EXDEV`, `ENOSPC`, a real permission denial, or Windows file locking, so a `FileSystem`/`Vfs` trait buys a false sense of coverage. Exempt: a blanket-impl alias trait that names a repeated `Fn`/`FnMut` bound and is implemented for every closure matching it — one `impl` line and no double by construction, but an open implementor set, which is the opposite of what this rule stops. | `rg -n --type rust --glob '!external/**' '^\s*(pub(\(crate\))?\s+)?trait \w+' .` names every trait; `rg -n --type rust --glob '!external/**' -e 'struct Mock' -e 'struct Fake' -e 'struct Stub' .` names every double that exists — a trait from the first list with a single `impl … for` line and no double in the second is the finding; `rg -n --type rust --glob '!external/**' -e 'trait FileSystem\b' -e 'trait Fs\b' -e 'trait Vfs\b' .` must be empty | MUST |
| ARCH-08 | Two parallel families of functions differing only by a backend prefix (`github_*`/`gitlab_*`, `local_*`/`remote_*`) are one trait with two impls written in longhand. | `rg -n --type rust --glob '!external/**' -e 'fn github_' -e 'fn gitlab_' -e 'fn local_' -e 'fn remote_' .` — the same suffix appearing under two prefixes is one trait written out twice | SHOULD |
| ARCH-09 | Dispatch ladder, in order: **generic parameter** by default; **enum + `match`** when the implementor set is closed, crate-owned and stored heterogeneously; **`dyn`** only for a genuinely open set, or to stop a type parameter propagating through the composition root — `dyn` in a hot loop measured ~3.4× slower, mostly lost inlining. | `rg -n --type rust --glob '!external/**' -e 'Box<dyn ' -e 'Arc<dyn ' .` — each needs ≥2 concrete types actually constructed against it, or is a composition-root field; a mature tree is never at zero, so restrict to added lines on a diff: `git diff --name-only -G'<dyn ' origin/main -- '*.rs'` | MUST |
| ARCH-10 | Never write `async fn` (or RPITIT) in a trait that is also used as `dyn Trait`. Apply `#[async_trait]` to the trait once, or keep dispatch generic — never scatter `Box::pin(async move {…})` at call sites. | `rg -l -U --type rust --glob '!external/**' 'trait \w+[^{]*\{[^}]*async fn' .` — `-l`, because a multiline match prints the whole trait body; no trait in a file it names may be stored as `dyn`, and `cargo build` fails with "not dyn compatible" | MUST |
| ARCH-11 | No `…Ext` extension trait purely to hang sugar on a `std` or foreign type. `StringExt`/`ResultExt`/`VecExt` are the pattern to stop, not extend. | `rg -n --type rust --glob '!external/**' '^\s*(pub(\(crate\))?\s+)?trait \w+Ext' .` — each hit needs a named reason | SHOULD |

## I/O Seams and Ports

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ARCH-12 | A function containing decision logic must not also call `std::fs::*`/`tokio::fs::*`, an HTTP client, `Command::new`, `Instant::now()`/`SystemTime::now()`, or `env::var` inline. Split it: the decision becomes a pure function over values, the I/O a thin caller. **A structural split, not automatically a trait** — ARCH-07 says when a port is justified, and for the filesystem specifically it is not (`testing.md`: a real temp directory, not a `FileSystem` abstraction). | A gate on a change, not on a tree — the tree-wide count is four figures on any real codebase, so restrict to added lines on a diff, one pass per needle: `git diff --name-only -G'fs::' origin/main -- '*.rs'`; `git diff --name-only -G'reqwest::' origin/main -- '*.rs'`; `git diff --name-only -G'Command::new' origin/main -- '*.rs'`; `git diff --name-only -G'::now\(' origin/main -- '*.rs'`; `git diff --name-only -G'env::var' origin/main -- '*.rs'` — in each file named, no added I/O call sits beside a branch; each is in a thin I/O caller, an adapter, or composition code | MUST |
| ARCH-13 | Read CLI args and `env::var` exactly once, in `main`/composition, into a `Context` struct passed down by reference. No `OnceLock`/`lazy_static`/`thread_local!` for anything a test needs to vary — and `env::set_var` is `unsafe` in edition 2024 because it races parallel test threads. | `rg -n --type rust --glob '!external/**' --glob '!**/main.rs' -e 'env::var' -e 'env::set_var' -e OnceLock -e 'lazy_static!' -e 'thread_local!' .` — `main.rs` is excluded because that is where these belong; a remaining hit outside config loading is a finding, and restrict to added lines on a diff | MUST |
| ARCH-14 | Domain and application code names only port traits, never a concrete adapter type. Concrete adapters are constructed only in `main`/composition; the compiler will not catch a stray concrete reference. | `rg -n --type rust --glob '!external/**' '^impl \w+ for \w+' .` names every concrete type sitting behind a trait; then per adapter, substituting its own name for `ADAPTER`, `rg -l --type rust --glob '!external/**' '\bADAPTER\b' .` must list only that adapter's module and the composition root | MUST |

## Modules and Visibility

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ARCH-15 | Default every item to private. Widen to `pub(crate)` for cross-module use, `pub(super)`/`pub(in path)` where narrower fits, bare `pub` **only** for a crate's genuine external contract. **This supersedes the older "`pub(crate)` is a design smell" rule** — that rule is withdrawn; RFC 2126, rustc's own lint docs and tokio's `lib.rs` all contradict it, and it yields four-figure bare-`pub` counts in crates nothing external consumes. | `unreachable_pub = "warn"` in `[workspace.lints.rust]`, then `cargo clippy --workspace` — zero new hits | MUST |
| ARCH-16 | Module dependencies run one way: `bin → adapters → application → core`, and `core` imports nothing above it. No module-level `use` cycle, even though one crate tolerates them — a crate split does not compile until every cycle is broken. | `cargo modules dependencies --lib` draws the graph and names a cycle outright. Falling back to grep, substitute the two suspected module names for `MODA`/`MODB` in `rg -l --type rust --glob '!external/**' --glob '**/MODA/**' 'use crate::MODB' .`, then run it again with the two swapped — both non-empty is a cycle | MUST |
| ARCH-17 | New multi-file modules use `name.rs` plus a sibling `name/` directory. Never `name/mod.rs` — a listing of `mod.rs` files gives a grepping agent zero filename signal. | `rg --files -g 'mod.rs' -g '!external/**' . .` returns nothing | SHOULD |
| ARCH-18 | No `pub use module::*;` glob re-export outside a module whose path contains `prelude`. A later same-named private item silently shadows a globbed export away. | `warn-on-all-wildcard-imports = true` in `clippy.toml`, then `cargo clippy -- -W clippy::wildcard_imports` | SHOULD |

## Workspace and Crates

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ARCH-19 | Every new workspace member names, in the PR description, at least one of: an actual second consumer, a hard dependency/feature isolation need, a **measured** compile-time bottleneck, or an independent semver promise. "For organization" is not a reason. | A diff adding `crates/*/Cargo.toml` without a stated reason is rejected | MUST |
| ARCH-20 | Adopt the pinned crate shape below. Never nest crate directories — Cargo's crate namespace is flat regardless of the directory tree, so a nested layout drifts. | `rg --files --glob 'crates/*/**/*/Cargo.toml' --glob '!external/**' . .` returns nothing — a bare `find crates …` aborts on a single-crate repo; `cargo tree -e normal -p '<name>-types'` shows no tokio/reqwest/starlark | MUST |
| ARCH-21 | Declare dependency versions, package metadata and lints once in `[workspace.dependencies]`/`[workspace.package]`/`[workspace.lints]`, and add `[lints] workspace = true` to **every** member manifest — workspace lints are not implicitly inherited, and a member without the opt-in silently gets none. | `rg --files-without-match '^\[lints\]' --glob '**/Cargo.toml' --glob '!external/**' .` — every member it lists is missing the opt-in; the workspace root manifest is not a member, so ignore it | MUST |
| ARCH-22 | Only if a crate is ever published to crates.io: declare internal deps as `{ path, version }`, gate merges on `cargo semver-checks`, enable `missing_docs`, seal public traits. Until publishing is decided, do none of it — each pays semver/docs overhead for a contract with no consumer. | `rg -n 'publish = false' --glob '**/Cargo.toml' --glob '!external/**' .` — a member lacking it activates this rule | CONSIDER |

**The crate shape is a pinned decision**, not a per-project derivation: flat
under `crates/*`, split by capability, the layout ripgrep, rust-analyzer and
uv all converged on. The justification adopted here is dependency isolation
and a shared test-support boundary — not compile time, which is unmeasured.

```text
crates/
  <name>-types/        domain newtypes + on-disk/wire schema; serde only, no async/HTTP
  <name>-core/         port traits + orchestration; no concrete adapters
  <name>-oci/          adapter implementing -core's ports
  <name>-store/        adapter implementing -core's ports
  <name>/              bin: clap, exit codes, TUI, composition root
  <name>-testsupport/  dev-dependency only
  xtask/
```

```toml
# workspace Cargo.toml
[workspace.lints.rust]
unreachable_pub = "warn"

# EVERY member Cargo.toml — inheritance is opt-in, silence is the failure mode
[lints]
workspace = true
```

## What Agents Get Wrong Here

1. **Bare `pub` reflex.** `pub` always compiles and never errors, so nothing
   pushes toward the truthful annotation. The most common visibility mistake
   by a wide margin. `unreachable_pub` at warn is the only feedback loop.
2. **Trait-per-struct on the word "extensible" or "testable".** One impl, a
   mock nobody varies, and a layer of indirection every future reader pays for.
3. **Growing the nearest big type instead of making a new one.** "Add X to the
   package manager" becomes another method on the existing `impl`. Six hundred
   methods is what that looks like after two years.
4. **`async fn` in a trait plus `dyn` storage.** Compiles as two separate
   edits, fails on use; the instinct is then to scatter `Box::pin(async move
   {…})` at call sites instead of fixing the trait once.
5. **Newtype without the invariant.** `struct Digest(pub String)` with a public
   field and a bare tuple constructor everywhere — type-distinct, zero
   enforcement, none of the safety it was asked for.
6. **`OnceLock` presented as dependency injection.** It compiles, removes the
   literal, and still stops two tests in one process from differing.
7. **Assuming `[workspace.lints]` is inherited.** The root table is added, the
   member opt-in is not, and every lint gate quietly stops applying.
8. **Recommending crate-per-module as "good architecture."** Generic modularity
   bias, contradicted by tokio's own consolidation of a finer split.
9. **Splitting a crate before breaking module cycles.** The plan reads
   correctly and does not compile, wasting a full refactor pass.
10. **Deref-as-inheritance.** "Let `Wrapper` behave like `Inner`" →
    `impl Deref for Wrapper`. Compiles, looks clever, violates C-DEREF.
11. **`unsafe { env::set_var }` in tests.** Silences the edition-2024 error and
    leaves the parallel-test data race intact.
12. **Hallucinated tooling.** `clippy::pub_use` does not exist (it is
    `wildcard_imports` plus a config key); `cargo publish --workspace
    --dependency-order` has no such flag. Also: an in-memory FS double proves
    nothing about rename-across-filesystems, permission bits, or partial writes.

## Sources

- [Rust API Guidelines: type safety](https://rust-lang.github.io/api-guidelines/type-safety.html) — C-NEWTYPE, C-CUSTOM-TYPE, C-DEREF
- [Rust API Guidelines: flexibility](https://rust-lang.github.io/api-guidelines/flexibility.html) — C-OBJECT, decide dyn-compatibility at trait-design time
- [AFIT/RPITIT stabilization](https://blog.rust-lang.org/2023/12/21/async-fn-rpit-in-traits/) — `async fn` in traits is not dyn compatible
- [RFC 2126: path clarity](https://rust-lang.github.io/rfcs/2126-path-clarity.html) — bare `pub` should truly mean public
- [`unreachable_pub`](https://doc.rust-lang.org/beta/nightly-rustc/rustc_lint/builtin/static.UNREACHABLE_PUB.html) — rustc's own recommendation of `pub(crate)`
- [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html) — inheritance tables and the `[lints]` non-inheritance footgun
- [matklad: large Rust workspaces](https://matklad.github.io/2021/08/22/large-rust-workspaces.html) — flat `crates/*`, why hierarchical crate trees rot
- [Small crates pattern](https://rust-unofficial.github.io/patterns/patterns/structural/small-crates.html) — both sides of the split argument, including tokio's reversal
