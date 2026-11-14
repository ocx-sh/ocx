# API Design and Idioms

The shape of a function, a derive list, and an owned value. Signatures and
conversions, which traits get derived and which leak secrets, the ownership
shape a value actually needs, and the code-shape smells worth a review
comment. Loads with the Rust quality rule on any diff that adds a public
function, a `#[derive(...)]` line, a `.clone()`, or a shared-state wrapper.

Contents: [Designing a Signature](#designing-a-signature) ·
[Choosing a Derive Set](#choosing-a-derive-set) ·
[Choosing an Ownership Shape](#choosing-an-ownership-shape) ·
[Reviewing Code Shape](#reviewing-code-shape) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Type-level design — traits, dispatch, newtype invariants, module visibility —
is in `architecture.md`. This file is the surface: the names, the derives, and
the ownership.

**The mechanism** is portable Rust practice: standard conversion traits, a
scoped derive policy, one interior-mutability decision made at the concurrency
boundary. **A pinned decision**: eager-derive (C-COMMON-TRAITS) is MUST only
for types crossing the lockfile / OCI-manifest / cache-key / `--json`
boundary — those are a public API even though nothing is published — and
judgment for internal plumbing. `Debug` is the exception: MUST everywhere,
enforced by a lint.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## Designing a Signature

| ID | Rule | Verification | Severity |
|---|---|---|---|
| API-09 | Owned conversions are `From`/`TryFrom`; borrowing conversions are `AsRef`/`AsMut`. Never `impl Into`/`impl TryInto` — only the std traits satisfy `impl Into<T>` bounds and `?`. A `From` that panics or `.unwrap()`s is a `TryFrom` wearing a disguise. | see block API-09 — anchoring `impl` to the line start keeps argument-position `impl Into<T>` bounds, which are correct, out of the result; any hit is a finding. Then see block API-09 (2) — a `From` body reaching a panic within eight lines | MUST |
| API-10 | An invariant-bearing newtype enforces it in exactly one place: a `TryFrom` impl, reached from deserialization via `#[serde(try_from = "Raw")]`. No infallible `From` from the unvalidated shape. **Supersedes** "newtypes always implement `From`" — a second unchecked constructor is not an invariant, and `derive(Deserialize)` on the real type *is* that constructor. | see block API-10 — the second line of the match restricts it to newtypes (tuple structs); every hit must carry `#[serde(try_from = "Raw")]` in the same attribute block | MUST |
| API-12 | `as_*` never allocates or clones; `to_*` never consumes `self`; `into_*` always consumes `self`. The prefix is documentation every caller trusts without reading the body. | see block API-12, scanning each body for `.clone()`, `to_vec()`, `to_owned()` or `String::from` | SHOULD |
| API-13 | Getters are `x(&self)`/`x_mut(&mut self)`, never `get_x`. Reserve `get`/`get_mut` for a single `Cell::get`-shaped accessor. | see block API-13 — the bare `fn` covers the `pub`, `pub(crate)` and `async` spellings a `pub fn` anchor misses | SHOULD |
| IDIOM-01 | Do not return `Option`/`Result` solely so every caller branches on it identically — the N copies diverge silently, and no single diff shows it. Push the branch into the function. | Reading heuristic; no lint. On a diff: when a PR adds a second call site to an `Option`-returning function, compare the two post-call shapes | SHOULD |
| IDIOM-03 | Replace a `&str`/`String`/`Vec<String>` parameter with an enum when the crate itself enumerates the valid values. The discriminator is not malformability — it is whether the valid set is already written down as `match` arms somewhere. Paths, URLs, references and free text are genuinely string-shaped. Closed-set enums generally: `architecture.md` ARCH-05. | `git diff -U0 -G'fn .*: *&str' -- '*.rs'`. Reviewer question: does every valid value appear as a literal in this crate? | SHOULD |

```sh
CHECKS=.claude/rules/rust-quality/checks
# API-09
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e '^[[:space:]]*impl.*[^[:alnum:]_]Into<' -e '^[[:space:]]*impl.*[^[:alnum:]_]TryInto<' --
# API-09 (2)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | PAT='(impl From<[^\n]*\n([^\n]*\n){0,8}[^\n]*\.unwrap\()|(impl From<[^\n]*\n([^\n]*\n){0,8}[^\n]*\.expect\()|(impl From<[^\n]*\n([^\n]*\n){0,8}[^\n]*panic!)' xargs -0 -r awk -f "$CHECKS/multiline.awk" --
# API-10
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | PAT='derive\([^\n]*Deserialize[^\n]*\n([#/][^\n]*\n)*(pub )?struct [[:alnum:]_]+\(' xargs -0 -r awk -f "$CHECKS/multiline.awk" --
# API-12
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | PAT='fn as_[a-z_]*\([[:space:]]*&self' A=8 xargs -0 -r awk -f "$CHECKS/context.awk" --
# API-13
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'fn get_[a-z_]*\([[:space:]]*&self' --
```

## Choosing a Derive Set

The reflexive `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`
line is the most common derive in any Rust corpus and the one that leaks
tokens and mints unversioned wire formats. Scan field names before you write
it.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| API-01 | Every `pub struct`/`pub enum` implements `Debug`, and every crate root carries `#![warn(missing_debug_implementations)]` — the lint is allow-by-default, so without it a missing `Debug` fails at a caller three files away. | see block API-01 — the crate roots it lists **are** the finding. Written the other way round, as a search for the attribute, silence means "not declared anywhere" and reads as a pass; both audited repos fail this and read clean. Then `cargo check --workspace` | MUST |
| API-02 | Never `#[derive(Debug)]` on a type with a field named or typed `token`, `secret`, `password`, `key`, `credential` or `auth`. Hand-write `Debug`, opening with `let Self { .. } = self;` so a later added field is a compile error, not a silent leak. Clippy has no equivalent lint; the grep is the gate. | see block API-02 — each match spans the derive and the offending *field declaration*, so struct literals no longer count; a hit is a finding unless the field type is `SecretString`/`SecretBox<_>`/`Zeroizing<_>`. Bare `key:` is deliberately unmatched (it is overwhelmingly a map or cache key); `secret_key`/`auth_key` fall out of the `secret`/`auth` patterns | MUST |
| API-03 | A credential becomes `secrecy::SecretString`/`SecretBox<T>` at ingress — env read, credential-file parse, auth header — never stored as a bare `String`/`Vec<u8>` on a struct. Wrapping at the boundary makes every downstream `Debug`, clone and drop redacted by construction. | see block API-03 — `var\w*\(` covers `env::var`, `env::var_os` and the local `var("…")` wrapper closures; the credential-name anchor keeps `PATH`/`TERM`-shaped env reads out. Each credential-valued result wrapped before storage | MUST |
| API-04 | Never `#[derive(Zeroize)]`/`impl Zeroize` on a type that *always* holds a secret; use `ZeroizeOnDrop` plus a custom `Drop`. A bare `Zeroize` permits a mid-lifetime wipe, leaving a blanked but still type-valid value. | see block API-04 — every hit paired with `ZeroizeOnDrop` | SHOULD |
| API-05 | Any type in a lockfile, OCI manifest, cache key, or `--json` output derives `Debug, Clone, PartialEq, Eq, Hash`; each omission carries a one-line comment naming the blocking field. Internal plumbing is judgment. | Two halves. No derive at all: see block API-05 — an item with a blank line above it carries no attribute. A missing member: see block API-05 (2) — a serde-facing derive short of the set, and no adjacent comment, is the finding. Both run workspace-wide: discard every hit outside the lockfile/manifest/cache-key module the change touches | SHOULD |
| API-06 | Derive `Copy` only for data that is plain *forever* — numeric wrappers, fixed-size handles, payload-free enums. Anything `*Config`/`*Options`/`*Policy`/`*Settings` omits it; removing `Copy` later breaks at every use site at once. | Per `Copy` derive: could the next field plausibly be `String`/`Vec`/`PathBuf`/`Box`? If yes, require `// Copy: fixed-size, will not grow heap fields` | SHOULD |
| API-07 | Derive `PartialOrd`/`Ord` on an enum only when declaration order *is* the intended order. Pin it with `// order is significant: A < B < C, do not reorder` and a test asserting it — alphabetizing during an unrelated tidy silently reverses every `sort()`/`BTreeMap`, with no compiler or clippy signal. | see block API-07 — the second line of the match is what restricts it to `enum`; each hit needs the comment *and* an `assert!(A < B)` test | MUST |
| API-08 | `#[derive(Default)]` only when a doc comment names the concrete runnable state it produces. Otherwise delete it for a named constructor — a zero state (`port: 0`, empty required `PathBuf`) type-checks and propagates as valid, failing far from the derive. | see block API-08 — the type-name filter is in the pattern, not left to the reader; require a `///` justification or a builder in the same `impl` | MUST |
| API-11 | Do not derive `Serialize`/`Deserialize` reflexively — only when the type genuinely crosses a process, file or network boundary. A type with any `unsafe` method relying on a field invariant never derives `Deserialize`. | `cargo clippy --workspace -- -W clippy::unsafe_derive_deserialize`; per derive, answer "which file or wire message is this written into?" | SHOULD |
| API-15 | Decide `#[non_exhaustive]` when the type is introduced, for every wire/manifest/config type — not just error enums. Adding it later is breaking. Internal crate-private enums stay exhaustive so matches remain total. | Review gate on any new `pub enum`/`pub struct` in a serde-facing module | SHOULD |
| API-14 | Add `derive_partial_eq_without_eq`, `derived_hash_with_manual_eq` and `derive_ord_xor_partial_ord` at `warn` to `[workspace.lints.clippy]`, and re-audit that table each toolchain bump — several live in nursery/pedantic, so a bare `cargo clippy` never surfaces them, and clippy moves lints between groups. | see block API-14, which locates the table. Then one command per lint, because a union hit count of three proves nothing: see block API-14 (2); see block API-14 (3); see block API-14 (4) — each must return a line reading `warn` or stronger; an empty result for any one is the finding. `cargo clippy --workspace` is then the gate | MUST |

```sh
CHECKS=.claude/rules/rust-quality/checks
# API-01
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/lib.rs' ':(glob)**/main.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -L -E -e 'warn\(missing_debug_implementations\)' --
# API-02
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | PAT='derive\([^)]*Debug[^;{]*\{[^}]*\n[[:space:]]+(pub )?(([[:alnum:]_]+_)?(tokens?|secrets?|passwords?|credentials?|auths?)(_[[:alnum:]_]+)?|api_keys?|private_keys?)[[:space:]]*:' xargs -0 -r awk -f "$CHECKS/multiline.awk" --
# API-03
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'var[[:alnum:]_]*\("[^"]*TOKEN' -e 'var[[:alnum:]_]*\("[^"]*SECRET' -e 'var[[:alnum:]_]*\("[^"]*PASSWORD' -e 'var[[:alnum:]_]*\("[^"]*_KEY' -e 'docker_credential::[[:alnum:]_]*credential[[:alnum:]_]*\(' -e 'Authorization' --
# API-04
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'derive\(Zeroize' -e 'impl Zeroize for' --
# API-05
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | PAT='(\n\n(pub )?struct [[:alnum:]_]+)|(\n\n(pub )?enum [[:alnum:]_]+)' xargs -0 -r awk -f "$CHECKS/multiline.awk" --
# API-05 (2)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e '^#\[derive\(.*erialize.*\)\]' -- | grep -v -E -e '^[^:]+:[0-9]+:#\[derive\(.*[^[:alnum:]_]Hash($|[^[:alnum:]_])'
# API-07
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | PAT='derive\([^\n]*Ord[^\n]*\n([#/][^\n]*\n)*(pub )?enum ' xargs -0 -r awk -f "$CHECKS/multiline.awk" --
# API-08
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | PAT='(derive\([^\n]*Default[^\n]*\n([#/][^\n]*\n)*(pub )?struct [[:alnum:]_]*Config($|[^[:alnum:]_]))|(derive\([^\n]*Default[^\n]*\n([#/][^\n]*\n)*(pub )?struct [[:alnum:]_]*Settings($|[^[:alnum:]_]))|(derive\([^\n]*Default[^\n]*\n([#/][^\n]*\n)*(pub )?struct [[:alnum:]_]*Options($|[^[:alnum:]_]))' xargs -0 -r awk -f "$CHECKS/multiline.awk" --
# API-14
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Cargo.toml' ':(exclude,glob)external/**' | PAT='\[(workspace\.)?lints\.clippy\]' A=10 xargs -0 -r awk -f "$CHECKS/context.awk" --
# API-14 (2)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Cargo.toml' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'derive_partial_eq_without_eq' --
# API-14 (3)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Cargo.toml' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'derived_hash_with_manual_eq' --
# API-14 (4)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Cargo.toml' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'derive_ord_xor_partial_ord' --
```

## Choosing an Ownership Shape

The highest-frequency Rust failure in machine-written code, by a wide margin.
It shows up at four scales and they are **one mistake**: making the borrow
checker stop talking instead of deciding who owns the value. The fix at every
scale is the same question — who owns this, and should the two copies diverge?

```rust
let name = cfg.name.clone();               // 1. stray clone: do the copies diverge? (STATE-24)
fn get(&mut self, k: &K) -> Option<&V>     // 2. &mut on a getter: viral up every caller (STATE-20)
let c = Arc::new(RefCell::new(map));       // 3. Arc over !Sync: Send error or BorrowMutError (STATE-21)
let s = Arc::new(Mutex::new(cfg));         // 4. one lock site, never crosses a thread (STATE-23)
```

| ID | Rule | Verification | Severity |
|---|---|---|---|
| STATE-24 | For every `.clone()` on non-`Arc`/`Rc` data, answer: "if I mutate the clone, should the original see it?" Yes → the clone is wrong. `redundant_clone` proves only that a clone was *wasted*; it cannot see cross-function redundancy or divergence bugs, and the review question lives exactly in that gap. | `cargo clippy -- -W clippy::redundant_clone` as the floor; a `// clone: <why>` comment convention makes the rest greppable | SHOULD |
| STATE-36 | Take a value out from behind `&mut` with `mem::take`/`mem::replace` — `Option::take` for `Option` fields — never with a clone you then overwrite. The placeholder allocates nothing for `String`/`Vec`/`HashMap`/`Option`, and the clone leaves a stale second copy live for the rest of the scope, which is exactly where STATE-24's divergence bug starts. | `git diff -U3 -G'\.clone\(\)' -- '*.rs'` — restrict to added lines on a diff; a tree-wide scan of `.clone()` is thousands of hits on any mature crate. Keep hits where the same place expression is assigned within a few lines. `redundant_clone` cannot see this — the clone is genuinely used before the overwrite | SHOULD |
| STATE-20 | A cache/memoization getter takes `&self` and has one of exactly three shapes: `fn get(&self, k) -> Option<&V>` (append-only, never evicts), `-> Option<Arc<V>>`/`Option<Rc<V>>` (evicting), or no getter at all (precompute the map, pass it in). `&mut self` on a getter is viral — every caller up the graph takes `&mut self` too and the read/write distinction dies for the whole chain. An evicting cache must never hand out `&V`. | see block STATE-20 — any hit on a type with more than one caller is a finding | MUST |
| STATE-21 | Never wrap `Cell`/`RefCell` in `Arc`. Pick the interior-mutability type when the value's concurrency boundary is designed, not when a panic reveals it — `Arc<T>` is `Send + Sync` regardless of `T`, so the `!Sync` interior breaks either as a distant `Send`-bound error or as a runtime `BorrowMutError` under an interleaving single-threaded tests never produce. | `cargo clippy` (`clippy::arc_with_non_send_sync`, warn-by-default) plus see block STATE-21 for the nested cases the lint misses | MUST |
| STATE-23 | A newly introduced `Arc<Mutex<T>>`/`Arc<RwLock<T>>` with exactly one lock call site is compiler appeasement, not a design decision — atomic refcounting, lock cost and poisoning risk bought for a value that never crosses a thread. No lint exists; contention is a runtime property. | see block STATE-23, keeping only the types the diff introduces, then count distinct `.lock()`/`.read()`/`.write()` sites for that type; one site total is the finding | SHOULD |
| STATE-22 | `std::sync::Mutex` is the default lock in async code; `tokio::sync::Mutex` requires a comment explaining why the critical section must span an `.await`. Reaching for the async mutex usually means the critical section is drawn too large. | see block STATE-22 — the optional brace catches `use tokio::sync::{Mutex, watch};`, which a fully-qualified pattern misses; every hit needs a justifying comment | SHOULD |
| STATE-25 | Write `Arc::clone(&x)`/`Rc::clone(&x)`, not `x.clone()`, so a reviewer scanning for STATE-24 candidates does not have to resolve the type first. | Enable `clippy::clone_on_ref_ptr` in `[workspace.lints]` — a restriction lint, allow-by-default, so silence means the decision was never made | CONSIDER |

```sh
# STATE-20
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'fn get[[:alnum:]_]*\([[:space:]]*&mut self' -e 'fn lookup[[:alnum:]_]*\([[:space:]]*&mut self' -e 'fn fetch[[:alnum:]_]*\([[:space:]]*&mut self' -e 'fn cached[[:alnum:]_]*\([[:space:]]*&mut self' --
# STATE-21
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'Arc<(std::)?(cell::)?(Ref)?Cell<' --
# STATE-23
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'Arc<(std::sync::)?Mutex<' -e 'Arc<(std::sync::)?RwLock<' --
# STATE-22
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'tokio::sync::\{?[^};]*Mutex' --
```

A `Mutex<T>` that starts guarding *behaviour* — staleness checks, size bounds,
refresh ordering — rather than a plain field escalates to an owned task plus a
channel; that ladder is `async.md` ASYNC-16.

## Reviewing Code Shape

| ID | Rule | Verification | Severity |
|---|---|---|---|
| IDIOM-04 | Every `.unwrap_or_default()`, `.to_string_lossy()`, `.ok()`, and `let _ = <fallible expr>` carries a same-line justification comment. All four compile clean and read idiomatic while discarding an error or an encoding signal — `to_string_lossy` substitutes U+FFFD, which on a cache key or a path derived from a tarball entry name is silent corruption. None is inherently wrong; the comment is the receipt. | see block IDIOM-04 — a hit with no same-line `//` justification is a finding; restrict to added lines on a diff. On a mature tree this is hundreds of steady-state hits, so it is a gate on a change, never on the tree | MUST |
| IDIOM-09 | `#[allow(static_mut_refs)]` and `#[allow(unsafe_op_in_unsafe_fn)]` block the review outright. In edition 2024 `static_mut_refs` is deny-by-default, so the allow is the difference between a failing build and shipped UB — and it is the exact shortest edit an agent reaches for when the build is red. | see block IDIOM-09 — non-empty fails CI. No model in the loop | MUST |
| IDIOM-08 | Every lint suppression is item-scoped and carries `reason = "..."`. Crate/module-scoped suppression silences the lint for code added a year later by an author who never saw the justification. Use `#[expect]` where the condition is currently true — it suppresses identically and warns when it goes stale. | see block IDIOM-08 and see block IDIOM-08 (2) — the second mechanizes "the next line opens a `mod` or `impl`" instead of leaving the reader to filter `-A1` context; both MUST. The `#[allow]`→`#[expect]` migration is SHOULD, on new and touched code only | MUST |
| IDIOM-05 | Enable `wildcard_imports` and `allow_attributes` in `[workspace.lints]`: **neither is on by default**, so a project running plain `cargo clippy` gets neither. Never lower `let_underscore_lock` (deny by default) or `let_underscore_future` (warn by default). Run `let_underscore_must_use` once as an audit, never as a standing lint: it fired on 32 to 156 sites per workspace measured, and under `warnings = "deny"` every hit must clear before the enable lands (`code-docs` GRD-10). | One command per lint, because two union hits could be two copies of one lint: see block IDIOM-05; see block IDIOM-05 (2) — each must return a line reading `warn` or stronger; an empty result for either is the finding. see block IDIOM-05 (3), which must be empty | MUST |
| IDIOM-07 | No glob imports outside `#[cfg(test)] mod tests` and trait-only preludes. This is a semver argument, not a style one: adding a public item is a minor version bump, so a dependency upgrade can introduce a colliding name and break a build that changed nothing locally. Re-export globs: `architecture.md` ARCH-18. | see block IDIOM-07 — the trailing `grep -v` drops the test-module and prelude cases the rule allows, which are the bulk of a raw glob-import scan; mechanical backup is `wildcard_imports` | SHOULD |
| IDIOM-11 | Before writing a helper, check `std`, then the crates already in the graph, then this codebase's own utility modules. Only then invent one. A second implementation of an existing helper is not caught by any lint, diverges on the first bug fix, and is the most common way an agent adds lines that already existed. | On any PR adding a free function under a `util`/`helper`/`common` module: name the `std` or in-tree equivalent it is not, in the PR body | MUST |
| IDIOM-12 | Do not own non-domain code. A hand-rolled serializer, codec, hash, retry loop, semver parser, glob matcher or date formatter is a maintenance liability with a known-good crate behind it — vendor or depend, and reserve hand-rolling for the cases where the ecosystem genuinely has nothing. | On a PR introducing a parser or codec: the `deny.toml`/dependency review must show why no crate was taken | SHOULD |
| IDIOM-14 | Declared dispatch must be the dispatch that runs. A trait method implemented by every impl and called by none, or a `downcast_ref` chain doing the traversal a declared `accept`/`visit` was supposed to do, is scaffolding left beside a hand-written duplicate: the next variant must then be added twice, and the trait tells every reader the wrong control flow. | One grep per trait method, with the method name written into the pattern — for a method named `accept`, see block IDIOM-14 — and read only the call sites outside the trait and its own impls: N impls with zero such sites is the finding. Then see block IDIOM-14 (2) restricted to `visit`-named functions; error-chain and dynamic-value downcasts are not this | SHOULD |
| IDIOM-15 | A fallible function that consumes its argument returns that argument inside the `Err` — `String::from_utf8` and `FromUtf8Error::into_bytes` are the std instance. Without it every caller that might retry clones defensively before *every* attempt, including the ones that succeed. | On a new value-consuming `fn f(x: T) -> Result<U, E>`: is any call site inside a retry loop (`package-manager-domain.md` PKG-16)? If yes, `E` carries `T`. Retrofitting it later is a wire change for a `#[non_exhaustive]` error type | CONSIDER |
| IDIOM-10 | Do not decompose a god-struct by having the facade `Deref` to the extracted type. It preserves `.method()` call sites while every trait bound still points at the god-struct — the extraction banks no type-system win, which was its entire purpose. Explicit forwarding or direct call-site migration only. `Deref` generally: `architecture.md` ARCH-06. | On an extraction PR: a new `Deref` impl whose `Target` is a type the same PR introduced is the failure | SHOULD |

```sh
CHECKS=.claude/rules/rust-quality/checks
# IDIOM-04
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e '\.unwrap_or_default\(\)' -e '\.to_string_lossy\(\)' -e '\.ok\(\)' -e 'let _ = ' --
# IDIOM-09
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e '#\[allow\(static_mut_refs\)\]' -e '#\[allow\(unsafe_op_in_unsafe_fn\)\]' --
# IDIOM-08
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e '^[[:space:]]*#!\[allow\(' --
# IDIOM-08 (2)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | PAT='(#\[allow\([^\n]*\n[[:blank:]]*(pub[^\n]{0,12})?mod($|[^[:alnum:]_]))|(#\[allow\([^\n]*\n[[:blank:]]*impl($|[^[:alnum:]_]))' xargs -0 -r awk -f "$CHECKS/multiline.awk" --
# IDIOM-05
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Cargo.toml' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'wildcard_imports' --
# IDIOM-05 (2)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Cargo.toml' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'allow_attributes' --
# IDIOM-05 (3)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Cargo.toml' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'let_underscore_lock *= *"allow"' -e 'let_underscore_future *= *"allow"' --
# IDIOM-07
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e '^[[:space:]]*use .*::\*;' -- | grep -v -E -e '^[^:]+:[0-9]+:[[:space:]]*use (super::\*|.*prelude::\*)'
# IDIOM-14
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e '\.accept\(' --
# IDIOM-14 (2)
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.rs' ':(exclude,glob)external/**' | xargs -0 -r grep -H -n -E -e 'downcast_ref' --
```

## What Agents Get Wrong Here

1. **`.clone()` the instant `cannot borrow` or `value moved` appears**, without
   asking whether the two values are meant to diverge. Highest frequency in the
   whole corpus. `redundant_clone` catches only the provably-dead subset.
2. **`.unwrap_or_default()` / `.ok()` / `to_string_lossy()` to make a type error
   or an `unused_must_use` warning disappear.** The path of least resistance
   that also compiles silently.
3. **`Arc<Mutex<T>>` as the reflex answer to any sharing question**, including
   single-threaded paths, because it makes the type `Clone` and interior-mutable
   in one move. Nothing warns; contention is invisible to static analysis.
4. **The reflexive `#[derive(Debug, Clone, Serialize, Deserialize)]` template**
   on a `TokenStore`/`AuthConfig`, without scanning field names. Caught only by
   API-02's grep, never by review attention.
5. **`RefCell` reached for to turn `&mut self` into `&self`**, without checking
   whether the type ever crosses a `tokio::spawn`. The most dangerous of the
   set: `cargo build` and single-threaded `cargo test` both pass.
6. **`&mut self` on a cache getter**, the first signature that stops the borrow
   checker complaining, with the caller cost invisible from where it is written.
7. **Deriving the minimum set that makes the current test pass** — `Debug` only,
   no `Eq`/`Hash`. Surfaces in an unrelated later PR as a trait-bound error at a
   distant call site.
8. **Emitting `&str` where the enum already exists**, because the variant set was
   unknown while drafting and the retrofit never happens.
9. **Suppressing a lint the moment the build goes red.** The allow is the
   shortest edit that satisfies the tool, and the agent bears no cost for a
   warning it will never see fire again.
10. **`impl From` with an `.expect()` inside** to satisfy a caller that wants
    `.into()`, inheriting an undocumented panic into every `impl Into<T>`
    consumer.
11. **`#[derive(Default)]` to shorten a struct literal**, minting an unrunnable
    config that then escapes test code.
12. **Alphabetizing enum variants during an unrelated tidy-up** — a
    formatting-shaped diff that reverses every ordering derived from it.
13. **Patching the call site instead of the derive**: a `HashMap` lookup misses,
    so a normalization step goes in before `.get()` rather than fixing a manual
    `PartialEq` that disagrees with a derived `Hash`.

## Sources

- [Rust API Guidelines: interoperability](https://rust-lang.github.io/api-guidelines/interoperability.html) — C-COMMON-TRAITS, C-CONV-TRAITS
- [Rust API Guidelines: naming](https://rust-lang.github.io/api-guidelines/naming.html) — the `as_`/`to_`/`into_` triad and the no-`get_` rule
- [corrode.dev: pitfalls of safe Rust](https://corrode.dev/blog/pitfalls-of-safe-rust/) — `Debug`-on-secrets, `Default`-on-config, `#[serde(try_from)]`
- [corrode.dev: when Rust gets ugly](https://corrode.dev/blog/ugly/) — stringly-typed parameters, the silent-data-loss quartet
- [matklad: push ifs up and fors down](https://matklad.github.io/2023/11/15/push-ifs-up-and-fors-down.html) — where a branch belongs
- [rust-unofficial: Deref polymorphism](https://rust-unofficial.github.io/patterns/anti_patterns/deref.html) — trait bounds do not propagate through `Deref`
- [docs.rs/secrecy](https://docs.rs/secrecy/latest/secrecy/) — `SecretBox`/`SecretString`, redacted `Debug`, `ExposeSecret`
- [Rust 1.81.0 release notes](https://blog.rust-lang.org/2024/09/05/Rust-1.81.0/) — `#[expect]` and the intended `#[allow]` migration
