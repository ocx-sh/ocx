# Reviewing a Rust Diff

You loaded this because you are reviewing Rust someone else — or something
else — wrote, rather than writing new code. The rest of this rule set is
organised by what you are building; this file is organised by what a diff
can break, which is a different reading order.

Anything `cargo clippy -- -D warnings` catches is out of scope by
construction: the linter already ran. What is left is the code that
compiles, passes, and is wrong.

The mechanical checks on what a change did to the project's own safety net
are separate — see
[diff-integrity.md](diff-integrity.md), and run them regardless of what
these dimensions turn up.

Contents: [Scope First](#scope-first) ·
[1. Correctness and Ownership](#1-correctness-and-ownership) ·
[2. Contracts](#2-contracts) · [3. Concurrency](#3-concurrency) ·
[4. Security and Untrusted Input](#4-security-and-untrusted-input) ·
[5. Platform and Durability](#5-platform-and-durability) ·
[6. Tests and Verifiability](#6-tests-and-verifiability) ·
[7. Docs and Observability](#7-docs-and-observability) ·
[8. Terminal UI](#8-terminal-ui) · [Evidence](#evidence)

## Scope First

```bash
git diff --stat origin/main... .          # size
git diff --name-only origin/main... .     # surface
```

Classify every changed file by **contract weight** and review in that
order — the review budget is finite and this is where it earns the most:

| Weight | Surface | Why first |
|---|---|---|
| 1 | On-disk and wire formats, lockfiles, manifests | A shipped format is forever; a mistake here is unrecallable |
| 2 | Exit codes, CLI flags, output shape | Scripts pin these; breaking one breaks users silently |
| 3 | Public API, error enums, trait definitions | Semver and every downstream call site |
| 4 | Internal logic | The usual bug surface |
| 5 | Tests, docs, scaffolding | Reviewed for honesty, not for style |

State the diff size. Over ~1,000 changed lines, real scrutiny per line
collapses — say so, review weights 1–3 exhaustively, and name explicitly
what you did not review rather than implying coverage you did not give.

Run the dimensions below in the order the diff's contract weight demands,
not the order listed. A change touching a serialized format leads with 2;
a change to the install path leads with 5; a pure refactor is carried by 1
and 6. Skip a dimension only when the diff cannot touch it, and **say
which you skipped** — a silent dimension is indistinguishable from a
skipped one.

## 1. Correctness and Ownership

The dimension the compiler is blindest to: code that compiles and does
the wrong thing.

- Does every new `Result` path propagate, or does something get swallowed
  by `let _ =`, `.ok()`, or `unwrap_or_default()` with no stated reason?
  (ERR-19)
- A new `unwrap`/`expect` on a runtime-fallible path? Does each surviving
  `expect` message state the *invariant*, not the failure? (ERR-09, ERR-10)
- Arithmetic on a value that came off the wire, a file, or a user: is
  overflow handled, and is every `as` cast lossless or checked?
- Off-by-one, inverted condition, wrong early return — read the branch
  the tests do not cover, not the one they do.
- **Ownership shape.** The highest-frequency defect class in agent-written
  Rust, appearing at four scales that are one mistake: a `.clone()` added
  to silence the borrow checker; a cache reached through `&mut self` where
  `&self` was the contract; the wrong interior-mutability primitive;
  `Arc<Mutex<_>>` around something with a single lock site. For every
  non-`Arc` clone, ask whether mutating the copy should have been visible
  to the original. (IDIOM family, STATE-20 … STATE-25)
- Do three or more new functions thread the same leading parameter pair?
  A missing type, not a style preference. (ARCH-01)
- Did a type grow past the method ceiling, or gain another inherent
  `impl` block in a new file? (ARCH-03)
- A new `String`/`&str` parameter carrying a format invariant that should
  be a parsed newtype? (ARCH-04)
- New trait with one implementation and no exercised test double?
  (ARCH-07) New `impl Deref` on a non-smart-pointer? (ARCH-06)
- Is decision logic newly mixed with direct `fs`/HTTP/`Command`/clock/env
  calls, making the error branches untestable? (ARCH-12)
- Derive set: does a new type deriving `Debug` hold a secret? Are the
  standard conversions present and infallible where they claim to be?
  (API family)

## 2. Contracts

Anything an existing consumer already depends on. Highest weight, first.

- **Exit codes**: a new failure path — which code, and is it in the
  pinned table? Any bare integer at a process exit, any `process::exit`
  outside `main`? (EXIT-01, EXIT-02, EXIT-04)
- Does a new error variant reach the classifier, and does the match stay
  exhaustive rather than falling into a wildcard? (EXIT-07, ERR-13)
- **Streams**: anything new on stdout that is not the result? Does
  `--format json` stay a pure payload? (CLI-01, CLI-02)
- **Flags**: a new global flag defined once and flattened, or redeclared?
  Help text ASCII and free of internal references? (CLI-10, CLI-12)
- **On-disk and wire formats**: is a changed serialized struct still
  readable by the shipped version — new fields defaulted, nothing renamed
  or removed, version discriminant honoured? A shipped format is forever.
  (DATA-FMT family)
- Is anything hash-ordered, filesystem-ordered, or float-formatted on its
  way into a file another tool diffs? (DATA-DET family)
- **Public API**: renamed or removed `pub` item, changed signature, new
  variant on an enum lacking `#[non_exhaustive]`? (ERR-02, ARCH-15)
- Error message text changed — is a test, a doc, or a user's script
  matching on it?
- Visibility widened (`pub(crate)` → `pub`) without a real external
  consumer? (ARCH-15)

## 3. Concurrency

Rare bugs, expensive bugs, almost never caught by tests.

- A `std::sync` guard held across an `.await`? (ASYNC-02)
- Blocking work — `std::fs`, `Command`, hashing, decompression — inside
  an `async fn` without `spawn_blocking`? (ASYNC-01)
- A new network or subprocess await without a deadline? (ASYNC-04)
- A fan-out whose width comes from caller or wire data, unbounded?
  (ASYNC-05) A new unbounded channel? (ASYNC-06)
- A `select!` branch built from a future that is not cancel-safe —
  `read_exact`, `write_all`, `Semaphore::acquire` — where losing partial
  work matters? (ASYNC-07)
- A spawned task whose handle is dropped, so its panic disappears?
  (ASYNC-09) `block_on` reachable from inside a task? (ASYNC-08)
- Lock held while entering a thread pool? (ASYNC-11)
- Two locks taken in a new order — does any other site take them in the
  opposite order?
- Retry against a remote without jittered backoff, or without respecting
  a rate-limit response? (ASYNC-10, PKG family)

## 4. Security and Untrusted Input

Assume every byte from a registry, an archive, a manifest, or a
subprocess is hostile.

- **Archive extraction**: is every entry path validated against escape —
  `..`, absolute paths, symlink and hardlink targets, Windows reserved
  device names, trailing dots and spaces — *after* normalisation, and are
  entry count, entry size and total size bounded? (SEC, PLAT-17)
- **Containment**: does the new path go through the one containment
  helper, or does it call `join` directly? Is containment decided by
  canonicalising both sides, not by string prefix? (PLAT-01, PLAT-05)
- **Digest verification**: verified *before* the bytes are used or
  executed, with no window in between? Re-verified after a resumed
  download? (DATA-DIG family, STATE-6)
- **Subprocess**: shell interpolation, an argument built from untrusted
  data, or reliance on `PATH` for a downloaded binary?
- **Secrets**: could a token reach a log line, an error, a `Debug` impl,
  `argv`, or a span field? (ERR-17)
- **Network**: TLS verification untouched, redirects and response sizes
  bounded, timeouts present?
- **Untrusted text to a terminal**: is registry-controlled text rendered
  without escape-sequence neutralisation? (CLI-03, TUI family)
- New `unsafe` — is there a `// SAFETY:` comment, and is the invariant it
  states actually guaranteed here? (LINT-07)
- New dependency — does it pass the `deny.toml` gates, and is it
  maintained? (CI-07)

## 5. Platform and Durability

The dimension that only fails on someone else's machine, or after a power
cut.

- **Durable write**: does a new write to a cache, lockfile or install
  tree go through the durable-write helper — temp file in the *target's*
  parent, sync, rename — or does it truncate in place? (STATE-1, STATE-2,
  STATE-3)
- Is a failed `fsync` treated as fatal for that data rather than retried?
  (STATE-4)
- Does a multi-file install become visible through exactly one rename?
  (STATE-5)
- **Drop and interruption**: any `unwrap`/`expect`/`panic!` inside a
  `Drop` body? A `debug_assert!` in `Drop` unguarded by
  `thread::panicking()`? A `process::exit` after a guard exists?
  (STATE-11, STATE-13, STATE-15)
- Is cleanup relying on a signal handler or `Drop` where a fixed staging
  location plus a startup sweep is the correct shape? (STATE-7, STATE-8)
- **Paths**: canonicalisation through `dunce`, not bare `canonicalize`?
  Any `format!`-built path? Any lossy `to_string_lossy` on a value that
  gets *recorded* rather than displayed? (PLAT-06, PLAT-07, PLAT-11)
- **Windows**: does the change assume POSIX rename or delete semantics?
  Is there a retry for a transient sharing violation? (PLAT-14)
- **Time**: `Instant` for elapsed and TTL, `SystemTime` only for
  persisted values? Any unwrapped `duration_since`? Is mtime the sole
  staleness gate? (PLAT-27, PLAT-28, PLAT-29)

## 6. Tests and Verifiability

Reviewed for honesty, not for style. The mechanical detectors are in
[diff-integrity.md](diff-integrity.md); this is the reading pass.

- Would the new test pass against the pre-change code? Mentally revert
  the fix — does it go red?
- Is the assertion strong enough, or is it `assert!(result.is_ok())` on a
  function whose interesting property is the value?
- Self-referential — asserting the implementation against itself?
  (TEST-13)
- For a bug fix: a regression test encoding the actual reported input?
- Error paths tested, or only the happy path?
- Does the test depend on wall-clock time, hash iteration order, a
  socket, an environment variable, or a fixed temp path? (TEST-05 …
  TEST-09)
- Do path assertions canonicalise both sides, and is every `!contains`
  paired with a positive assertion proving the check can match? (TEST-08)
- New exit code or user-facing stderr message — a test asserting the code
  and the stream separately? (TEST-10)
- A structural source-text guard used where a behavioural seam was
  available? (TEST-11)

## 7. Docs and Observability

- Does a new or changed `pub` fallible function document `# Errors`, and
  a panicking one `# Panics`? (DOC family)
- Behaviour changed without the user-facing doc, help text, or changelog
  changing?
- Is a new comment restating the code rather than the rationale? Would a
  better name, an extracted function, or a type have removed the need?
- Commented-out code, or a stale `TODO` with no issue reference?
- New logging: correct level, on stderr, not duplicating an error that is
  also returned? (ERR-18, OBS family)
- Could a new span field carry a token, a credential, or an authenticated
  URL?

## 8. Terminal UI

Only when the diff touches the TUI; skip loudly otherwise. The full set is
in [tui.md](tui.md) — on a diff, the highest-yield four:

- Is the terminal restored on every exit path, including a panic and a
  signal, by something `process::exit` cannot skip?
- Does registry-controlled text reach a widget without escape and
  control-character neutralisation?
- Is width computed with `unicode-width` rather than `len()`, and does
  truncation cut on a grapheme boundary?
- Does a new binding collide with a convention users already have —
  Ctrl-C above all?

## Evidence

A finding is admissible only with all four: a resolvable `file:line` you
re-read after writing the claim; the rule ID it violates, or a
one-sentence statement of the invariant broken; a failure scenario as
concrete inputs → wrong outcome; and a refutation attempt — you tried to
prove it safe and failed.

Findings that fail the bar are dropped, not downgraded to nits. A nit is a
real but trivial defect, not an unverified guess. Behaviour claims need a
citation in the *source*: `fn validate_path` does not validate the path
because it is called that.

**Zero findings is a valid, common result** on a small, well-made diff.
Manufacturing findings to look thorough produces over-engineering
downstream, which is more expensive than the nit that went unreported.
