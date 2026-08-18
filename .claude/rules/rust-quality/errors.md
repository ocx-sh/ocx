# Error Handling

Error types, cause chains, message style, panic policy, and the error →
exit-code contract for every Rust crate in the family. Loads while editing
`error.rs`, `main.rs`, or any module that returns `Result`.

Contents: [The Split](#the-split-pinned) · [Error Types and Chains](#error-types-and-chains) ·
[Messages and Reporting](#messages-and-reporting) ·
[Panics and Exit Codes](#panics-and-exit-codes) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers, and the difference matters when adopting this elsewhere:

- **The mechanism** — concrete enums in library-shaped code, an opaque
  error only at the boundary, every wrapper linked by `#[source]`, panics
  reserved for broken invariants — is general Rust practice.
- **The choices** — `thiserror` + `anyhow` and nothing else; the split
  drawn by *role* rather than by crate type; lowercase `Display` text
  everywhere including `anyhow` context strings; the full cause chain on
  by default — are *pinned decisions*. Shipped, tested, not re-litigated.

## The Split (pinned)

One crate does not license `anyhow` everywhere. Every subsystem module owns
a concrete `thiserror` enum in its own `error.rs`. `anyhow` starts at
`app::run` and stops there.

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallError {
    /// Pass-through: nothing but the source matters.
    #[error(transparent)]
    Package(#[from] PackageError),

    /// Adds a fact the source cannot know. The message names the path,
    /// never the cause — the cause comes back out of `source()`.
    #[error("cannot read manifest {path}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
```

`#[error("{0}")] Package(PackageError)` is wrong in both directions: it
prints the inner text *and* returns `None` from `source()`, truncating the
chain. `#[error(transparent)]` is the correct spelling for a pass-through.

## Error Types and Chains

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ERR-01 | Subsystem modules return a concrete `thiserror` enum. `anyhow::Error` appears only in `main.rs`, `app::run`, and the command handlers directly beneath them — `anyhow` in a reusable module destroys downstream `match`-ability and exit-code classification. | `rg -l --type rust anyhow -g '!external/**' -g '!main.rs' -g '!app.rs' -g '!**/command/**' .` — every hit is a candidate downgrade | MUST |
| ERR-02 | Every public error enum carries `#[non_exhaustive]` — adding a variant to an exhaustive public enum is a semver break. | `rg -B2 --type rust --glob '!external/**' 'pub enum \w*Error' .` — the attribute precedes each | MUST |
| ERR-03 | Every variant wrapping another error carries `#[source]` (or `#[from]`, which implies it). Never `#[error("{0}")]` on a wrapped error without it — `source()` then returns `None`, and both `{err:#}` rendering and downcast classification silently lose the inner error. | `rg -A2 --type rust --glob '!external/**' '#\[error\("\{0\}"\)\]' .` — each hit has `#[source]`/`#[from]` on the field, or is `#[error(transparent)]` | MUST |
| ERR-04 | Never `map_err` a source into a `String`-carrying variant, and never store `error.to_string()` in a field — stringifying erases the source chain and all downcast ability. | `rg --type rust --glob '!external/**' 'map_err.*to_string\(\)' .` empty | MUST |
| ERR-07 | Use `#[from]` only when the variant needs nothing beyond the source. The moment a path, URL, digest or identifier matters, write an explicit named-field variant and `map_err` it in — a `#[from]` field can hold only the source, so taking it drops the call-site context. | Review each `#[from] io::Error`-shaped variant: can the message name *which* file or URL? If not it is under-specified | MUST |
| ERR-08 | Keep `Err` payloads small; box anything past 128 bytes. `Result<T, E>` is sized to its largest variant in every frame it crosses, including the `Ok` path. | `cargo clippy --workspace --all-targets -- -D clippy::result_large_err` | SHOULD |
| ERR-25 | Do not add `eyre`, `color-eyre`, `miette`, `ariadne`, `snafu` or `error-stack`. Span diagnostics pay only when there is user-authored source text to point into; a `Diagnostic` derive with no `#[label]` is a heavier `thiserror`. | `rg -e miette -e eyre -e snafu -e error-stack -e ariadne -g 'Cargo.toml' -g '!external/**' .` empty | SHOULD |

## Messages and Reporting

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ERR-05 | `#[error("…")]` text and `anyhow` context strings are lowercase, unpunctuated, and never open with `error:`/`failed:`. Acronyms keep canonical case (`JSON`, `I/O`, `TLS`, `SHA-256`). The string must compose when nested inside a `{err:#}` chain. | `rg --type rust --glob '!external/**' -e '#\[error\("[A-Z][a-z]' -e '#\[error\(".*[.!]"\)' .` empty — the trailing `[a-z]` mechanizes the acronym allowlist, since `JSON`/`TLS`/`I/O` never continue lowercase; `rg -i --type rust --glob '!external/**' -e '\.context\(.*"error' -e '\.context\(.*"failed' .` empty | MUST |
| ERR-06 | A variant's `Display` says only what `source()` does not. Never interpolate the source's text *and* return it from `source()` — a reporter walking the chain then prints the same sentence twice. | `#[error("…: {source}")]` or `#[error("…: {0}")]` on a variant that also has `#[source]` is the violation; drop the interpolation | MUST |
| ERR-16 | Sanitize the rendered chain for terminal control, `\r`, NUL and bidi (`Cf`) characters at the single stderr boundary, before printing. Chains quote package names, tags and paths read off the wire (CWE-150). | `rg -n --type rust --glob '!external/**' -e 'eprintln!' -e 'writeln!\(io::stderr' .` — each routes through the sanitizer, pinned by a same-file structural test | MUST |
| ERR-17 | Credentials are `secrecy::SecretString`, never `String`. Any URL or path from auth-bearing config is scrubbed of userinfo and query string before interpolation — redaction by construction beats remembering it at each `{:?}`. | `rg -n --type rust --glob '!external/**' -e 'token: (Option<)?String' -e 'password: (Option<)?String' -e 'secret: (Option<)?String' -e 'api_key: (Option<)?String' -e 'bearer: (Option<)?String' .` — a secret-named binding held as a bare `String` is a candidate leak; the bare words match every doc comment, so anchor on the type and restrict to added lines on a diff | MUST |
| ERR-18 | A function that returns `Result` does not also log the error it returns. Log once, where propagation stops. | Any `tracing::error!`/`warn!` immediately preceding a `return Err`/`?` on the same value | MUST |
| ERR-19 | No silent swallowing. `let _ = result`, `.ok()`, and `unwrap_or_default()` on a `Result` each need a comment naming why the error is discardable — otherwise it is indistinguishable from a forgotten error path. | `rg -n --type rust --glob '!external/**' -e 'let _ = ' -e '\.ok\(\);' -e 'unwrap_or_default\(\)' .` — a hit with no adjacent rationale is a finding; restrict to added lines on a diff | MUST |
| ERR-20 | Print with the alternate format `{err:#}`, never `{err}` — the plain form shows only the top message and throws the chain away. | `rg -n -e '\{err\}' -e '\{e\}' -g 'main.rs' -g '**/cli/**' -g '!external/**' .` empty | SHOULD |
| ERR-21 | A batch over N targets collects `Vec<(Target, Error)>` and reports "K of N failed" with per-item detail; it does not `?` out at the first failure. | A bare `?` in a loop body over packages/targets is the violation | SHOULD |
| ERR-22 | Public fallible functions carry a `# Errors` doc section; public functions that can panic carry `# Panics`. | `cargo clippy --workspace -- -W clippy::missing_errors_doc -W clippy::missing_panics_doc` | SHOULD |

## Panics and Exit Codes

Panics mean bugs and keep exit 101. No `catch_unwind` around `main`, no
`human-panic`-style downgrade of a panic into ordinary error text. The
numeric exit-code table is pinned in the sibling rule
[cli-contract.md](cli-contract.md) — never duplicate it here.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| ERR-09 | `[lints.clippy]` sets `unwrap_used`, `expect_used` and `panic` to at least `warn` for non-test code, with `allow-unwrap-in-tests`/`allow-expect-in-tests` in `clippy.toml`. All three are restriction-group and allow-by-default — a clean clippy run proves nothing without them. | `rg -A8 '\[lints\.clippy\]' --glob '**/Cargo.toml' --glob '!external/**' .` shows all three — the glob covers a workspace root and its member manifests alike, then `cargo clippy --workspace --all-targets -- -D warnings` | MUST |
| ERR-10 | An `expect` message states the invariant that guarantees success, never the failure. `expect("hardcoded IP address is valid")`, not `expect("parse failed")`. Prefer `expect` over `unwrap` in production code. | `rg -ni --type rust --glob '!external/**' -e '\.expect\("failed' -e '\.expect\("could not' -e '\.expect\("error' .` empty — those openings name the failure, not the invariant | MUST |
| ERR-11 | `main` returns `std::process::ExitCode`. `std::process::exit` never appears in library code, and in `main` only as the terminal statement — it skips every destructor on every live stack, dropping lockfile release, temp-dir cleanup and buffered writes. | `rg -n --type rust --glob '!external/**' 'process::exit' .` — every hit is the last statement of a `main` | MUST |
| ERR-12 | One `#[repr(u8)] #[non_exhaustive] ExitCode` enum per workspace, sysexits-aligned, with `From<ExitCode> for std::process::ExitCode`. A separate binary with its own taxonomy needs an ADR. | `rg -n --type rust --glob '!external/**' 'ExitCode::from\([0-9]' .` empty; `rg -n --type rust --glob '!external/**' 'enum ExitCode' .` returns exactly one | MUST |
| ERR-13 | Derive the exit code from the error's **structure** — an exhaustive `match`, or a `ClassifyExitCode`-style trait walked over the chain. Never from `Display` text. Either mechanism is fine; string matching is not. Wording changes, taxonomy must not. | `rg -e 'to_string\(\).*contains' -e 'Display.*match' -g '**/cli/**' .` at the classification site empty | MUST |
| ERR-14 | Classification matches are exhaustive — no `_ => Failure` wildcard, or a new variant ships silently mis-classified. Intended fall-throughs are listed explicitly and locked by a test. | `_ =>` arms in `classify*` fns; each existing fall-through has a pinning test | SHOULD |
| ERR-15 | Never assign 101, or any code ≥ 128, to a modeled error path — 101 is Rust's panic signal and 128+N is signal-derived. Forwarding a *child's* `128 + signum` status is the sole exception. | `rg -n --type rust --glob '!external/**' -e 'ExitCode::from\(101' -e 'process::exit\(101' -e '= 101' .` empty outside panic-hook code | MUST |
| ERR-23 | `catch_unwind` appears only at an FFI boundary or a documented supervised-worker boundary. Never around `main`, never as control flow — unwinding across FFI is UB, and a panic laundered into error text hides a broken invariant from CI and from anything watching exit 101. | `rg -n --type rust --glob '!external/**' 'catch_unwind' .` — each hit adjoins an `extern "C"` boundary or a worker-supervision comment | MUST |
| ERR-24 | A poisoned `Mutex` is propagated (`.expect("<invariant>")`) or explicitly recovered via `into_inner()`/`clear_poison()`. Never downgrade a poison to a default — poisoning means a thread already panicked mid-mutation. | `rg -n --type rust --glob '!external/**' -e '\.lock\(\)\.unwrap_or' -e '\.lock\(\)\.ok\(\)' -e '\.lock\(\)\.map_or' -e 'let Ok\(.*\.lock\(\)' .` — each hit is a violation unless it recovers via `into_inner()`; a bare `.lock()` grep is 90% compliant noise | SHOULD |

## What Agents Get Wrong Here

1. `.unwrap()`/`.expect()` written as "temporary" handling during iteration
   and never revisited — especially in paths that "obviously can't fail":
   file I/O, network, parsing.
2. Treating a clean `cargo clippy` as proof of panic-policy compliance.
   `unwrap_used`, `expect_used` and `panic` are restriction-group and
   allow-by-default; read the manifest before trusting the exit code.
3. Capitalized, punctuated `#[error("Failed to read the file.")]` — generic
   good-message advice that double-capitalizes once the CLI prepends `error: `.
4. Reaching for `anyhow` inside library-shaped code "for convenience". It
   compiles, looks idiomatic, and infects the whole call chain.
5. `map_err(|e| MyError::X(e.to_string()))` — the most common way to destroy
   a source chain while appearing to handle the error properly.
6. Dropping context instead of restructuring the variant. Asked to include a
   path in an error, an agent that cannot make `#[from]` carry a sibling
   field settles for a bare `#[from] io::Error`. The diff looks like work.
7. `std::process::exit()` mid-`main` "to fail fast" — the shortest code that
   produces the right code, silently dropping all pending cleanup.
8. Logging the error at every layer that also propagates it with `?`.
9. Hallucinating `Error::provide()` and `thiserror`'s `#[backtrace]` as
   stable. Both are nightly-only behind `error_generic_member_access`.
10. Debug-printing a credential-bearing struct — models do not reliably tell
    safe-to-debug from secret-bearing unless the *type* enforces it.
11. Exhaustive `match` on a `#[non_exhaustive]` foreign error enum. Compiles
    today, breaks on the next dependency bump.

## Sources

- [Rust API Guidelines — C-GOOD-ERR](https://rust-lang.github.io/api-guidelines/interoperability.html) — lowercase, unpunctuated, `Send + Sync + 'static`
- [rust-lang/project-error-handling#27](https://github.com/rust-lang/project-error-handling/issues/27) — names the double-reporting anti-pattern
- [thiserror README](https://github.com/dtolnay/thiserror/blob/master/README.md) — canonical enum shape and the `#[from]`-holds-only-the-source constraint
- [`anyhow::Context`](https://docs.rs/anyhow/latest/anyhow/trait.Context.html) — eager `.context()` vs lazy `.with_context()`
- [`std::error::Error`](https://doc.rust-lang.org/std/error/trait.Error.html) and [rust#99301](https://github.com/rust-lang/rust/issues/99301) — `source()` is stable, `provide()` is not
- [clippy `unwrap_used`/`expect_used`](https://raw.githubusercontent.com/rust-lang/rust-clippy/master/clippy_lints/src/methods/mod.rs) — verbatim proof both are allow-by-default
- [Cargo `[lints]`](https://doc.rust-lang.org/cargo/reference/manifest.html#the-lints-section) — workspace-wide lint config syntax
- [rustc-dev-guide — Diagnostics](https://rustc-dev-guide.rust-lang.org/diagnostics.html) — the `error:`/`note:`/`help:` house style
