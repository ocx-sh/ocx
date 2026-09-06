# Documentation and Tracing

Rustdoc as a checkable contract, comments as two distinct registers, and
`tracing` as the diagnostic channel that never carries a result. Both halves
fail the same way: they compile, they look done, and nothing checks them.
Every rule here attaches a machine check to a claim.

Contents: [Rustdoc Contract](#rustdoc-contract) · [Comments: Two Registers](#comments-two-registers) ·
[Tracing and Log Levels](#tracing-and-log-levels) ·
[Spans for Concurrent Registry I/O](#spans-for-concurrent-registry-io) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers, and the difference matters when adopting this elsewhere:

- **The mechanism** — RFC 1574 section vocabulary, doctests as the example
  surface, `skip_all` spans, results on stdout — is general Rust practice.
- **The pinned decisions** are this family's, already shipped and not
  re-litigated: `missing_docs` on library crates only (binaries owe a `//!`
  per module instead); the error chain funnels through *one* logging seam
  because that seam is where terminal sanitization lives; no OpenTelemetry
  *crates* in a process measured in seconds, but its field-name vocabulary
  adopted anyway; the machine-readable JSON envelope is built
  by project code, never scraped from `tracing`'s fmt layer.

## Rustdoc Contract

| ID | Rule | Verification | Severity |
|---|---|---|---|
| DOC-01 | The text before the first blank line of any `///`/`//!` is exactly one complete sentence, third-person present indicative — "Returns the resolved digest.", not "Return…" / "This function returns…". That text is the item's entire entry in module listings and search. | `git diff -U0 -G'^\s*/// .*[^.]$' -- '*.rs'` — read the added `///` lines; a block whose summary (the text before the first blank doc line) has no closing period is a finding. Whole-tree the pattern is 20k+ wrapped continuation lines, so restrict to added lines on a diff | MUST |
| DOC-02 | Every `pub fn` returning `Result` carries a `# Errors` section naming the *conditions* that produce each error. "Returns an error if the operation fails" passes every lint and tells the caller nothing. | Two totals, read off the `N matches` line each prints: `rg -q --stats --type rust --glob '!external/**' '# Errors' .`; `rg -q --stats --type rust --glob '!external/**' 'pub fn .*-> .*Result' .` — the first must be ≥ the second | MUST |
| DOC-03 | Every function that can panic — indexing, slicing, `unwrap`/`expect`, integer division, `assert!` — carries a `# Panics` section stating the precondition. `missing_docs` cannot see this. | Review: a new `pub fn` with a panicking operation and no `# Panics` is a finding | MUST |
| DOC-04 | Every `unsafe fn` has a `# Safety` rustdoc section stating what the *caller* must uphold; every `unsafe` block has a `// SAFETY:` comment stating why it holds *here*. Two documents, two readers. | `rg -n --type rust --glob '!external/**' 'unsafe fn' .` → each has `# Safety`; `rg -n --type rust --glob '!external/**' 'unsafe \{' .` → each has `// SAFETY:` | MUST |
| DOC-05 | Only the canonical headers: `# Examples` (always plural), `# Panics`, `# Errors`, `# Safety`. No invented headers, no singular `# Example` — fixed vocabulary is what makes the corpus greppable. | `rg -n --type rust --glob '!external/**' -e '/// # Example$' -e '/// # Error$' -e '/// # Panic$' -e '/// # Safety$' .` must be empty | MUST |
| DOC-06 | Doc examples use `?`, hide setup with `# `-prefixed lines, and close with `# Ok::<(), E>(())`. Never `.unwrap()`/`.expect()` in a rendered example — examples get copy-pasted verbatim into code that then panics on first real error. | Extract fenced blocks from `///`/`//!` lines, grep for `.unwrap(`/`.expect(` | MUST |
| DOC-07 | Never fence a doctest `ignore`. Use `no_run` (network/disk), `text` (not Rust), or `compile_fail`. An unavoidable `ignore` carries a `// why:` on the same line. | `rg -n --type rust --glob '!external/**' '\x60{3}ignore' .` — every hit carries `// why:` | MUST |
| DOC-08 | The verify pipeline runs `cargo test --doc --workspace` as its own step. `cargo nextest run` does not execute doctests — a nextest-only pipeline verifies zero examples. | `rg -n 'test --doc' taskfiles/ .github/workflows/` | MUST |
| DOC-09 | CI runs `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`. rustdoc's link lints only *warn*; `cargo doc` "succeeds" while emitting dead intra-doc links. | `rg -n 'RUSTDOCFLAGS' .github/workflows/ taskfiles/` | MUST |
| DOC-10 | Library crates with an external consumer surface carry `#![warn(missing_docs)]` at the crate root. Binary crates do not — forcing it on intra-crate `pub` plumbing manufactures ceremony docs — but every module in them carries a `//!`. | `rg -n --type rust --glob '!external/**' 'missing_docs' .` — every library crate root must be a hit; for binaries, every `src/**/*.rs` starts with `//!` | MUST (lib) / SHOULD (bin) |
| DOC-11 | A `///` on a clap-rendered surface states the user contract and nothing else: short line ≤ ~70 chars, no trailing period, ASCII only, no ADR/RFC/section references, no dates, no implementation jargon. | The `task verify` help gates: valid definition, ASCII help, no internal references, ASCII completions | MUST |
| DOC-12 | Never emit `#[doc(cfg(...))]` or `#[doc(auto_cfg)]` in code that must build on stable — both are still nightly-gated, and the badge is invisible on a stable `cargo doc` anyway. A feature-gated `pub` item names its feature in prose. | `rg -n --type rust --glob '!external/**' -e 'doc\(cfg' -e 'doc\(auto_cfg' .` empty, or every hit inside `cfg_attr(docsrs, …)` | MUST |
| DOC-13 | Remove a public item only after a release carrying `#[deprecated(since = "X.Y.Z", note = "use Y instead")]`; both fields mandatory. The removal lands with a changelog entry under `Removed`. | `rg -n '#\[deprecated\]$' .` must be empty; a removed item shows a prior deprecating commit | MUST |
| DOC-14 | Every changelog entry sits under one of `Added / Changed / Deprecated / Removed / Fixed / Security`, written as a sentence for a human. A verbatim commit subject is rejected — Conventional Commits is the input discipline, not the output. | `rg -n -e '^\s*[-*] feat(\(.*\))?:' -e '^\s*[-*] fix(\(.*\))?:' -e '^\s*[-*] chore(\(.*\))?:' -e '^\s*[-*] refactor(\(.*\))?:' CHANGELOG.md` must be empty | SHOULD |
| DOC-15 | Every literal command transcript in README/docs is covered by a `trycmd` case or equivalent snapshot. `--help` and man pages are generated; transcripts are the highest-drift doc surface. | A test invoking `trycmd::TestCases::new().case("README.md")` exists | SHOULD |
| DOC-16 | Reference other items with intra-doc links (`[Manifest]`, `[Self::pull]`), never a hand-written docs.rs URL or a bare backticked name. Hand-written URLs rot silently. | Covered by DOC-09 — `broken_intra_doc_links` warns by default | SHOULD |
| DOC-17 | Declare one README↔`lib.rs` sync direction per crate: `#![doc = include_str!("../README.md")]`, or `cargo rdme --check` in CI. Never hand-maintain both. | `rg -n --type rust --glob '!external/**' 'include_str!\("../README' .` OR a `cargo rdme --check` CI step | CONSIDER |

```rust
/// Resolves a reference to a content digest.
///
/// # Errors
/// [`Error::NotFound`] when the tag is absent from the registry;
/// [`Error::Unavailable`] when the registry cannot be reached.
///
/// # Panics
/// Panics if `layers` is empty — callers check `is_empty` first.
///
/// # Examples
/// ```
/// let digest = resolve(&client, "app:1.0")?;
/// # Ok::<(), Error>(())
/// ```
```

## Comments: Two Registers

**The gate, before any comment is written.** The Ousterhout test: *if someone
unfamiliar with the code could write your comment just by reading the code, it
adds no value.* Then three substitutions — would a **better name**, an
**extraction into a named function**, or a **type** (enum, newtype) remove the
need for the comment? Add the comment only when all three fail. Most comments
an agent wants to write are a naming failure in disguise.

**Two registers, never mixed.** `///` and `//!` are for API consumers via
rustdoc: content is **contract** — what it does, when it fails, what invariants
hold. `//` is for the maintainer reading the implementation: content is
**rationale** — why this approach, what non-obvious constraint forced it.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| DOC-18 | Commented-out code is deleted, not left behind. VCS preserves history. | `rg -n --type rust --glob '!external/**' -e '^\s*// *let .*=' -e '^\s*// *fn .*\(' -e '^\s*// *if .*\{' -e '^\s*// *for .* in ' -e '^\s*// *self\.' -e '^\s*// *\}' .` — each hit is a finding. The trailing syntax is what separates commented-out code from prose starting "// letting", "// for it" | MUST |
| DOC-19 | No narration comments restating the next line (`// Create a new vector`), no tautological docs (`/// Returns the path` on `fn path()`), no closing-brace labels (`} // end if`). | Review against the Ousterhout test; a comment derivable from the code below it is a finding | SHOULD |
| DOC-20 | Preserve the comments that survive the gate: non-obvious constraints and "looks wrong, is correct" notes, parenthetical why-qualifications, external references (RFC, spec, algorithm), issue links, and phase markers in multi-step orchestration. | Review: a diff deleting one of these without replacing the information is a finding | SHOULD |

## Tracing and Log Levels

| ID | Rule | Verification | Severity |
|---|---|---|---|
| OBS-01 | A command's *result* goes to stdout through the printer, never through `tracing::info!` — routed through the logger it vanishes under `--quiet` and arrives with a level prefix no pipe can parse. Diagnostics and progress go to stderr. | For each `tracing::info!`, ask whether a script would want that line; any "yes" is a violation | MUST |
| OBS-02 | Every subscriber's fmt layer names its writer explicitly, and that writer is the one OBS-24 selects: stderr by default, a file/switchable writer under a TUI, a progress-aware wrapper under live bars. An accidental stdout writer silently corrupts every piped invocation. | `rg -n --type rust --glob '!external/**' 'with_writer' .` — every hit is one of those three, never `std::io::stdout` | MUST |
| OBS-03 | Never hold a `Span::enter()` guard across an `.await` — the guard stays entered while the task is parked, producing interleaved, wrong traces. Use `#[instrument]` on the async fn, `.instrument(span)` on the future you are about to await, or `.in_scope()` around the synchronous part only. | `rg -n '\.enter\(\)' .` → no `.await` before the guard drops | MUST |
| OBS-04 | Every `#[instrument]` carries `skip_all` plus an explicit `fields(...)` drawn from the OBS-20 vocabulary, and those field expressions are cheap — field access, `%`/`?` formatting, trivial arithmetic. The default records every argument via `Debug` on every call — at 200 concurrent layers, 200 full-descriptor serializations — and `fields(...)` is evaluated eagerly even when the span is disabled. | `rg -n -A3 '#\[instrument' .` — each has `skip_all`/`skip(...)`; read every `fields(...)` expression | MUST |
| OBS-05 | Anything carrying a credential, registry token, or auth header is typed so it has no `Debug`/`Display` — `secrecy::SecretString` or a newtype that deliberately omits both. `tracing` has no redaction mechanism; the only reliable defence is making the leak a compile error. | `rg -n --type rust --glob '!external/**' -e 'token: ' -e 'password: ' -e 'secret: ' -e 'credential: ' .` — the `: ` anchors it to a declaration rather than every mention of the word; each hit's type derives neither. Steady-state a mature tree carries legitimate hits, so restrict to added lines on a diff | MUST |
| OBS-06 | A `WorkerGuard` from `tracing_appender::non_blocking(...)` is bound in `main` and lives to process exit, never returned-and-dropped by a setup helper — an early drop discards buffered lines exactly in the crash case you needed them for. | `rg -n 'non_blocking\(' .` → trace the guard binding back to `fn main` | MUST |
| OBS-07 | Level semantics: `error` = the operation failed and the process exits non-zero; `warn` = degraded but continuing; `info` = coarse milestones a `-v` user wants; `debug` = per-request/per-file maintainer detail; `trace` = loop bodies and wire data. | Review: any `info!` firing more than a handful of times per invocation is `debug` | MUST |
| OBS-08 | The binary exposes an explicit level control (a `--log-level` ValueEnum, or `-v`/`-vv`/`-q`) whose precedence against the env-var chain is documented in one place and in `--help`. `RUST_LOG` stays the power-user escape hatch. | `--help` shows the control; the precedence chain appears in the env-var reference | MUST |
| OBS-09 | Never inline an `"Error:"`/`"error:"` prefix at a `tracing::error!` site — the level already categorizes the line. | `rg -n 'error!\("Error' .` must be empty | MUST |
| OBS-10 | Untrusted text — registry-sourced names, digests, error chains quoting wire documents — passes through the terminal sanitizer before reaching stderr/stdout. `tracing-subscriber` forwards `\n`, `\r`, NUL and the whole `Cf` bidi set straight to the terminal (CWE-150). The sanitizer itself and its test corpus are defined once, by `security.md` SEC-31; this rule only says the log path must not bypass it. | A structural test pins the sanitizer call at the error boundary | MUST |
| OBS-11 | `tracing-subscriber`'s JSON formatter output is unversioned internal debugging; it documents no schema stability. Any JSON promised to consumers is a separately constructed, versioned envelope. | No doc text promising a stable log-JSON shape; `--format json` output is built by project code, not scraped from a fmt layer | MUST |
| OBS-12 | No `opentelemetry*` dependency in a short-lived CLI — provider setup plus flush-on-shutdown for a process that runs for seconds is pure cost, against a crate whose own docs warn of ongoing breaking changes. Admissible only in a daemon mode, pinned, with the justification written down. This bans the *crates*, not the semantic-convention *names*, which OBS-20 requires. | Any new `opentelemetry*` line in `Cargo.toml` needs an accompanying daemon-mode rationale | MUST |
| OBS-13 | Panic handling makes zero network calls by default (`human_panic::setup_panic!()` writes a local report). Remote crash reporting is opt-in behind explicit runtime consent — this is a package manager holding registry credentials. | `rg -n 'sentry::init' .` → gated by a runtime config read, never unconditional in `main` | MUST |
| OBS-14 | Bridge `log`→`tracing` in one direction only, via `tracing_log::LogTracer::init()`. Both directions together recurse infinitely. | `rg -n -e 'LogTracer' -e 'log-always' .` — at most one direction configured | SHOULD |
| OBS-15 | `--version` embeds the git SHA, dirty flag, and build timestamp (vergen or equivalent). A bug report against a prebuilt binary is untriageable without it. | `--version` output contains a SHA; `rg -n 'VERGEN' .` | SHOULD |
| OBS-16 | Behaviour that depends on a log line being emitted is asserted with `#[tracing_test::traced_test]` + `logs_contain(...)`, not by eyeballing output. | `rg -n --type rust --glob '!external/**' 'traced_test' .` | CONSIDER |

## Spans for Concurrent Registry I/O

| ID | Rule | Verification | Severity |
|---|---|---|---|
| OBS-17 | Exactly one root span per command invocation (`grim_add`, `ocx_install`), created at subcommand dispatch and carrying `run_id` plus the subcommand; every other span descends from it. Without it a capture is a flat interleave and "which lines are my run" is unanswerable. | `rg -n '#\[instrument' .` — the dispatch fn is instrumented, and no other instrumented entry point runs with no ambient span | MUST |
| OBS-18 | Every retryable network unit of work is its own span with a **constant** name (`pull_layer`, one per layer, N concurrent) — never one bulk span with N log lines, never a digest in the name. Digest, URL and size are fields; transfer-progress ticks are not tracing events at all. 200 short-lived spans is normal, 200 span *names* breaks every group-by-name view. | `rg -n --type rust --glob '!external/**' -e 'span!' -e '#\[instrument' .` — every span name is a literal, never a `format!`; the trace alone answers "which layer was slow" | MUST |
| OBS-19 | Every future handed to `tokio::spawn` from inside an instrumented call chain is `.instrument(Span::current())` or `.in_current_span()` before spawning. `tokio::spawn` inherits no span, so the plain form compiles, runs, passes review, and silently orphans its whole subtree. | `rg -n --type rust --glob '!external/**' 'tokio::spawn\(' .` — every argument is wrapped, or the binary creates no spans at all | MUST |
| OBS-20 | Field names come from OpenTelemetry semantic conventions where one exists — `error.type`, `url.full`, `server.address`, `http.response.status_code`, `http.request.resend_count`, `oci.manifest.digest`. OCI concepts OTel has not standardized take one `oci.*` name each (`oci.repository`, `oci.layer.digest`, `oci.layer.size`), declared once in a shared constants module and never re-spelled at a call site. | Diff every `fields(...)`/`%name` identifier against the constants module; a name used at exactly one site is a suspected synonym | MUST |
| OBS-21 | `#[instrument(err)]` appears once per failure, on the span nearest the origin. Never re-`err` a hop that only `?`-propagates the same `Result`, and never also `error!` it at the top-level handler — `err`/`ret` each emit their own event, so every extra hop double-reports one user-visible failure. | For one error type, count `#[instrument(err` plus `error!(` sites on its path to the exit code; more than one firing is the defect | MUST |
| OBS-22 | Any URL recorded as a field, log line, or error message passes a redaction function stripping `user:pass@` userinfo and signed-URL parameters (`X-Amz-Signature`, `sig`, `Signature`), key names preserved. Never `%url` on a raw `Url` — blob fetches are exactly the presigned case, and OTel makes a credential-free `url.full` a MUST. | The redactor exists and has a unit test; every `url` at a `fields(`/`#[instrument]` site goes through it, not the raw value | MUST |
| OBS-23 | One `run_id` (UUID or ULID) generated per invocation and recorded on the root span. A registry-returned request ID is a *different* identifier: logged at `debug`, under its own field name, labelled as the registry's, never merged into `run_id`. The distribution spec defines no request-ID header, so the two ends genuinely differ. | Root span and bug-report output carry `run_id`; `rg -n 'request.id' .` — a distinct field name | SHOULD |
| OBS-24 | Exactly one component owns the terminal and the fmt writer follows it: a file while a raw-mode TUI holds the alt-screen, a progress-aware writer while bars are live, plain stderr otherwise. The switch is named in one place, not branched per call site. `tracing-indicatif` is an acceptable implementation, not a required one. | `rg -n --type rust --glob '!external/**' -e 'fmt::layer' -e 'with_writer' .` — never a bare stderr writer in a binary where a TUI or progress manager can take the screen | MUST |
| OBS-25 | If a `max_level_*`/`release_max_level_*` feature is set, the compiled ceiling is at least the highest level the verbosity flag advertises in `--help`. Those features strip call sites from the binary, so a mismatch makes `--log-level trace` emit nothing and read as a broken flag with no error. | `rg -n 'max_level' --glob '**/Cargo.toml' --glob '!external/**' .` — a bare `Cargo.toml` operand misses every workspace member — cross-checked against every level the flag's help text names | SHOULD |
| OBS-26 | The interactive `--debug`/`-v` path and a bug-report bundle are separate outputs with separate budgets: human-formatted to the terminal under OBS-24, structured JSON to a file. Both apply the identical OBS-05/OBS-22 redaction — a bundle pasted into a public tracker is the worse leak vector, not the excusable one. | The bundle writer and the fmt layer share one redaction path; read the bundle code for a direct token/URL write | MUST |

## What Agents Get Wrong Here

1. Writing a doc example and never running `cargo test --doc`. Hallucinated
   method names and stale signatures inside `///` blocks are fully
   mechanically catchable and caught by nothing in a nextest-only pipeline.
2. `.unwrap()` in a doc example "for brevity" — shorter, correct-looking in
   isolation, and copy-pasted straight into production.
3. `#[instrument]` sprayed on every function without `skip_all`. Looks
   thorough; silently records every argument via `Debug`, auth tokens and blob
   bytes included, on every call.
4. `tokio::spawn(pull_layer(layer))` with no `.instrument(...)`. It compiles,
   runs, passes review, and silently orphans the subtree from the trace.
5. Routing a result through `tracing::info!`. It disappears under `--quiet`
   and arrives unparseable when it doesn't.
6. Marking a failing doctest ```` ```ignore ```` to turn CI green — laundering
   a broken example into a permanently unchecked one.
7. `#[instrument(err)]` at every hop plus an `error!` at the handler: each
   addition locally correct, together four reports of one failure.
8. `# Example` singular, or an invented header like `# Usage`, breaking the
   greppability the whole section contract depends on.
9. Holding `span.enter()` across `.await` — the exact output of mechanically
   translating a sync tracing pattern into an async fn.
10. Inventing a field name per call site — `repo` here, `repository` there —
    because nothing cross-checks spelling between two functions.
11. Reaching for OpenTelemetry when asked to "add observability".
12. Putting the digest in the span *name* via `format!`, so every
    group-by-name view degenerates into N groups of one.
13. A bare `fmt().init()` in a binary that also runs a TUI, writing to stderr
    mid-render and shredding the alt-screen frame.
14. Narration comments (`// Increment the counter`) and `/// TODO: document`
    stubs — both satisfy a reviewer's glance and fail the Ousterhout test.
15. Bare `#[deprecated]` with no `since`/`note`: compiles, looks like
    diligence, leaves the caller with no version and no migration path.
16. `#[doc(cfg(...))]`, which reads as normal because docs.rs builds nightly,
    then fails to compile on a stable toolchain.
17. Chaining `.json().pretty()` on one fmt builder — mutually exclusive
    formatters — and assuming `RUST_LOG`'s `{field=value}` syntax filters
    events, when it matches *spans* only, at span-creation time.

## Sources

- [Rust API Guidelines — Documentation](https://rust-lang.github.io/api-guidelines/documentation.html) — `C-FAILURE`, `C-QUESTION-MARK`, `C-EXAMPLE`, `C-LINK`
- [RFC 1574](https://rust-lang.github.io/rfcs/1574-more-api-documentation-conventions.html) — the fixed header vocabulary and the summary-sentence convention
- [rustdoc book — Documentation tests](https://doc.rust-lang.org/rustdoc/write-documentation/documentation-tests.html) — attributes, hidden lines, `?` patterns, merged doctests
- [rustdoc book — Lints](https://doc.rust-lang.org/rustdoc/lints.html) — proof that `cargo doc` warns rather than fails
- [Cargo book — SemVer compatibility](https://doc.rust-lang.org/cargo/reference/semver.html) — deprecate-before-remove
- [`tracing::attr.instrument`](https://docs.rs/tracing/latest/tracing/attr.instrument.html) — `skip_all`/`fields` semantics and eager field evaluation
- [`tracing_subscriber::fmt::format::Json`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Json.html) — no stability contract
- [clig.dev](https://clig.dev/) — stdout/stderr separation, "don't treat stderr like a log file"
