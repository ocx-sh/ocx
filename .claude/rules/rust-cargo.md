---
paths:
  - "**/Cargo.toml"
  - "**/clippy.toml"
  - "**/rustfmt.toml"
  - "**/deny.toml"
  - "**/rust-toolchain.toml"
summary: Lint policy, toolchain pinning, dependency gates, CI and release settings for Rust
keywords: rust,cargo,clippy,lints,msrv,toolchain,ci,cargo-deny,release,supply-chain
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# Rust Cargo, Lints and CI

Lint declaration, toolchain pinning, CI job design, supply-chain gates and
release profiles for a Rust workspace that ships prebuilt binaries. Loads
while editing any `Cargo.toml` or a tool config beside it.

**It does not glob CI workflows**, deliberately. A workflow filename says
nothing about its language — a repository's `.github/workflows/` holds the
website deploy, the notification job and the Rust gate side by side, and
matching them all pays this file's whole context cost on every one of them.
Working on Rust CI is a subject you arrive at, not a path you land on:
route here from [rust-quality.md](rust-quality.md) or read
[CI and Supply-Chain Gates](#ci-and-supply-chain-gates) directly.

Contents: [Lint Policy](#lint-policy) · [The Lints Stanza](#the-lints-stanza) ·
[Toolchain and Tooling](#toolchain-and-tooling) ·
[CI and Supply-Chain Gates](#ci-and-supply-chain-gates) ·
[Release and Distribution](#release-and-distribution) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

**Adding, replacing or upgrading a dependency? Read
[rust-cargo/crates-of-record.md](rust-cargo/crates-of-record.md) first** —
the crate this family uses for each job, the superseded crate a model
reaches for instead, and what changed to make it wrong (DEP-01…08).

Two layers:

- **The mechanism** — policy in `[workspace.lints]` rather than `RUSTFLAGS`,
  one denial switch, self-expiring suppressions, a ratcheted rollout,
  SHA-pinned actions, an explicit ship profile — is general Rust practice.
- **The selections** — wholesale `pedantic`, the named restriction lints, no
  `nursery`, no MSRV matrix, advisories off the PR gate — are *pinned
  decisions*. They are derived from an exactly pinned toolchain and a
  binary-distribution model. Adopt or replace them wholesale; do not
  re-litigate them lint by lint.

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn,
CONSIDER = Suggest.

## Lint Policy

| ID | Rule | Verification | Severity |
|---|---|---|---|
| LINT-01 | All lint policy lives in the workspace root `[workspace.lints.rust]` / `[workspace.lints.clippy]`. Every member crate carries exactly `[lints] workspace = true` and defines no lints of its own. One exception: a lint with a nonzero backlog lives in LINT-15's `-W` list, not the manifest, until its count reaches zero — a manifest entry is unconditionally subject to `-D warnings` and therefore cannot ratchet. | `rg -l '^\[lints\]' --glob '**/Cargo.toml' --glob '!external/**' .` — every hit contains `workspace = true` and nothing else; every key in `[workspace.lints.clippy]` has a live count of 0 | MUST |
| LINT-02 | Never set `RUSTFLAGS=-D warnings` in CI or `.cargo/config.toml`. Denial comes from `[workspace.lints.rust] warnings = "deny"` plus `cargo clippy -- -D warnings` — `RUSTFLAGS` also denies *path*-dependency warnings, turning a vendored fork's upstream noise into a build failure, and busts the incremental cache. | `rg -n --hidden 'RUSTFLAGS' --glob '.github/**' --glob '.cargo/**' --glob 'taskfiles/**' .` is empty — `--hidden` is what reaches the dot-directories, and one that does not exist contributes nothing instead of erroring | MUST |
| LINT-03 | Declare individual clippy lints at `warn`, never `deny`. Teeth come from LINT-02's single switch — per-lint `deny` over a group-level `warn` only creates `priority` puzzles. | `rg -n '= "deny"' Cargo.toml` matches only `warnings = "deny"` | SHOULD |
| LINT-04 | Enable `clippy::pedantic` as a whole group at `warn` with `priority = -1`, behind an allow-list where every allowed lint carries a trailing rationale comment, and bring it in through the LINT-16 ratchet rather than straight into the `-D warnings` gate — the one audited shipping project that enables the group wholesale still carries 15 allows. Never enable `nursery`, `restriction` or `cargo` as groups — `nursery` is upstream-declared false-positive-prone and `restriction` lints contradict each other by design. Pedantic is safe here *only* because TOOL-01 freezes group membership. | `rg -n -e nursery -e restriction Cargo.toml` finds no group-level entry; every `"allow"` under `[workspace.lints.clippy]` has a same-line `#` comment | MUST |
| LINT-05 | Name these restriction lints individually at `warn`: `unwrap_used`, `expect_used`, `indexing_slicing`, `panic_in_result_fn`, `unwrap_in_result`, `get_unwrap`, `dbg_macro`, `todo`, `unimplemented`, `mem_forget`, `string_slice`, `integer_division`. `arithmetic_side_effects` is deferred to LINT-19 wave 4 and, when it lands, is scoped by an `arithmetic-side-effects-allowed` per-type list rather than blanket-suppressed — clippy's own tracker calls it really noisy. | `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` exits 0; if `arithmetic_side_effects` is on, `clippy.toml` carries a non-empty `arithmetic-side-effects-allowed` | MUST |
| LINT-06 | Declare `await_holding_lock` and `await_holding_refcell_ref` explicitly and never suppress either — `std::sync::Mutex` is the house lock type, and this check is the only mechanical thing between that convention and a deadlock. | `cargo clippy --workspace -- -D clippy::await_holding_lock -D clippy::await_holding_refcell_ref` | MUST |
| LINT-07 | `unsafe_code = "forbid"` at workspace level; downgrade to `"deny"` in exactly the crates that need FFI, each with an adjacent comment naming why. | `rg -n unsafe_code --glob '**/Cargo.toml' --glob '!external/**' .`; every `"deny"` has a rationale comment | MUST |
| LINT-08 | Every suppression is `#[expect(<lint>, reason = "…")]`. Bare `#[allow]` only where a lint legitimately does not fire under some `cfg`, and then still with `reason`. `#[allow]` rots silently; `#[expect]` warns via `unfulfilled_lint_expectations` once the code is fixed — the only self-expiring suppression an unattended agent can be trusted with. | `git diff -U0 -G'#\[allow\(' origin/main -- '*.rs'` — every *added* `#[allow(` line carries `reason =` and an `// expect-impossible:` note; the standing tree count is never zero, so this one is restricted to added lines on a diff. `rg -UnP --type rust --glob '!external/**' '#\[expect\((?![^\]]*reason)' .` is empty | MUST |
| LINT-09 | Keep a workspace-root `clippy.toml` with `msrv`, `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-dbg-in-tests`, `allow-panic-in-tests`, `allow-indexing-slicing-in-tests`, `check-private-items = false`, and `disallowed-methods = ["std::env::set_var", "std::env::remove_var", "std::process::exit", "std::thread::sleep"]`. The test allowances are what let LINT-05 stay strict without an `#[expect]` per test; `check-private-items = false` is clippy's own staging knob that keeps LINT-12 public-API-first. | `clippy.toml` exists; `cargo clippy --workspace -- -D clippy::disallowed_methods` exits 0; `check-private-items` is absent or `false` | MUST |
| LINT-10 | Do **not** put `std::sync::Mutex` in `disallowed-types`. Short non-await-spanning critical sections under std `Mutex` are the audited house convention; reaching for `tokio::sync::Mutex` is the signal a critical section grew too big, and LINT-06 catches the real bug. | `rg -n 'disallowed-types' clippy.toml` — if present, must not list `std::sync::Mutex` | MUST |
| LINT-11 | Enable `unreachable_pub = "warn"` through the LINT-16 ratchet, not the manifest, until its count reaches zero — it was 17% of one published ~6,000-warning pedantic rollout and is only partly autofixable. When it fires the first fix is module nesting (`mod` vs `pub mod`), the second is `pub(crate)` — never widening the API to silence it. | `unreachable_pub` has a monotonically decreasing baseline entry, and moves into `[workspace.lints.rust]` only in the commit its count hits 0 | SHOULD |
| LINT-12 | `missing_errors_doc` and `missing_panics_doc` at `warn` on library-shaped crates — an agent consuming the API cannot see a failure mode a human reads out of the body. They land last, in LINT-19 wave 4, with `check-private-items = false`; content-free prose satisfies both and no grep catches it, so that stays a review requirement. | `cargo clippy -p <lib> -- -D clippy::missing_errors_doc -D clippy::missing_panics_doc` exits 0 only after wave 4; before that both sit in the baseline | SHOULD |
| LINT-13 | Never make `clippy::redundant_clone` a standing gate. It is `nursery` for cause; run it scoped and time-boxed during an explicit clone-reduction change. | `rg -n 'redundant_clone' Cargo.toml` is absent or `"allow"` | SHOULD |
| LINT-14 | `rustfmt.toml` carries stable options only — never `unstable_features`, `imports_granularity`, `group_imports`, `wrap_comments`, `format_code_in_doc_comments`. CI runs `cargo fmt --all -- --check`, never bare `cargo fmt`. | `rg -n -e unstable_features -e imports_granularity -e group_imports -e wrap_comments rustfmt.toml` is empty | MUST |
| LINT-15 | The `-D warnings` gate and the ratchet are two separate clippy invocations: `[workspace.lints]` carries only lints already at zero and runs under `cargo clippy --workspace --all-targets --locked -- -D warnings`; every lint with a backlog is passed as `-W clippy::<lint>` on a second invocation carrying no `-D warnings`. Cargo emits `[lints]` entries as plain `--warn` flags, so `-D warnings` promotes a manifest-level group `warn` to a hard deny on the first hit — the group blocks merges instead of ratcheting. | The ratchet step's command line contains no `-D warnings` | MUST |
| LINT-16 | Commit a per-lint-code baseline (`clippy-warn-baseline.json`, one integer per code). CI fails if any single code's live count exceeds its entry; decreases are committed; a code reaching 0 moves into `[workspace.lints.clippy]` and is deleted from the baseline in the same commit. Per-code, not aggregate — otherwise one lint's improvement masks another's regression. | Counts from `cargo clippy --workspace --all-targets --message-format=json`, grouped by `.message.code.code`, diffed against the committed file; no key appears in both it and `[workspace.lints.clippy]` | MUST |
| LINT-17 | A committed integer caps total suppressions — `#[allow(clippy::` and `#[expect(clippy::` counted together — and may only decrease; raising it needs an inline comment in the same diff naming the lint and why the code cannot be fixed. A newly introduced crate- or module-level `#![allow]`/`#![expect]` is rejected outright: function scope is the ceiling. | `rg --stats --type rust --glob '!external/**' -e '#\[allow\(clippy::' -e '#\[expect\(clippy::' .` reports matches ≤ the committed ceiling; `git diff --name-only -G'#!\[allow\(' origin/main -- '*.rs'` and `git diff --name-only -G'#!\[expect\(' origin/main -- '*.rs'` each list no file | MUST |
| LINT-18 | Run `cargo clippy --fix` for exactly one lint code per commit (`--fix -- -A clippy::all -W clippy::<one>`), with the full test suite after each, and send any autofix touching a public signature to manual review. `--fix` has a multi-year record of emitting broken and compiling-but-semantically-wrong rewrites, and implies `--all-targets`, so a blind group pass also rewrites tests and benches. | The commit message names exactly one `clippy::<code>`, and every file it touches had that code in the pre-fix baseline | MUST |
| LINT-19 | Enable in four waves, never a flag day: (1) lints already at zero plus `warnings = "deny"`, LINT-06/07/14 and `dbg_macro`/`todo`/`unimplemented`; (2) the confirmed-autofixable ones, one commit each under LINT-18 — `uninlined_format_args`, `redundant_closure`; (3) pedantic and the remaining LINT-05 lints into the ratchet; (4) doc lints and `arithmetic_side_effects`. Any lint whose initial count exceeds 500 is carved out into its own tracked issue and excluded from the gate until driven down — a must-not-increase gate over an unshrinkable backlog just blocks unrelated work. | Each wave is its own PR touching only `Cargo.toml`/the ratchet list plus lint fixes; no baseline entry above 500 lacks a linked issue | MUST |

### The Lints Stanza

```toml
[workspace.lints.rust]
warnings = "deny"          # the ONLY denial switch; everything below stays `warn`
unsafe_code = "forbid"     # "deny" + a why-comment in FFI crates only
unreachable_pub = "warn"   # only once its ratchet count is 0 (LINT-11)

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }  # frozen by the pinned channel; needs an annotated allow-list
# nursery, restriction, cargo: never as groups.
await_holding_lock = "warn"         # the std::sync::Mutex deadlock gate — never suppressed
await_holding_refcell_ref = "warn"
unwrap_used = "warn"                # test allowances live in clippy.toml
expect_used = "warn"
unwrap_in_result = "warn"
panic_in_result_fn = "warn"
get_unwrap = "warn"
indexing_slicing = "warn"           # untrusted manifests and registry-supplied names
string_slice = "warn"
integer_division = "warn"       # arithmetic_side_effects: wave 4 only, scoped (LINT-05)
mem_forget = "warn"
dbg_macro = "warn"
todo = "warn"
unimplemented = "warn"
missing_errors_doc = "warn"         # library-shaped crates, wave 4
missing_panics_doc = "warn"
```

Every member crate, in full: `[lints]` / `workspace = true`.

## Toolchain and Tooling

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TOOL-01 | `rust-toolchain.toml` pins an exact `channel = "X.Y.Z"` — never `stable`, `beta` or `nightly` — with `components = ["rustfmt", "clippy"]`. This is the premise LINT-04 rests on: a floating channel drifts the pedantic group under CI. | `rg -n 'channel' rust-toolchain.toml` matches `^\d+\.\d+\.\d+$` | MUST |
| TOOL-02 | Declare `rust-version` in `[workspace.package]` equal to the pinned channel. Add no MSRV matrix job — this project distributes binaries, not source, so an MSRV floor below the pinned channel is a fiction nobody consumes. | `rg -n 'rust-version' Cargo.toml` matches the toolchain channel | SHOULD |
| TOOL-03 | A toolchain bump is its own commit touching only `rust-toolchain.toml` plus the lint fixes it forces, re-captures the LINT-16 baseline, and re-greens the full gate before anything lands on it — it is the one moment new pedantic lints appear, and group membership moves between clippy releases, so a baseline captured against the old one silently means something else. | The commit changes no `src/**/*.rs` beyond lint fixes, and its diff includes the baseline file | SHOULD |
| TOOL-04 | No nightly toolchain, `-Z` flag or `#![feature(…)]` on any path producing a shipped artifact. Nightly lives only in non-blocking scheduled canaries. | `rg -n -e '\+nightly' -e '[-]Z ' .github/ taskfiles/` hits only `continue-on-error` jobs; `rg -n --type rust --glob '!external/**' '#!\[feature\(' .` is empty | MUST |
| TOOL-05 | Run `cargo shear` in the PR gate. Not `cargo-machete` (regex-based, false positives), not `cargo-udeps` (nightly), not rustc's `unused_crate_dependencies` (fires falsely on single-target deps). | A CI step runs `cargo shear`; `rg -n 'unused_crate_dependencies' Cargo.toml` is empty | SHOULD |

## CI and Supply-Chain Gates

| ID | Rule | Verification | Severity |
|---|---|---|---|
| CI-01 | Every workflow declares `permissions: {}` at top level and grants scopes per job — a workflow-level grant hands every job the maximum any one job needs. | `rg --files-without-match '^permissions: \{\}' --glob '*.yml' .github/workflows/` is empty | MUST |
| CI-02 | Pin every `uses:` to a full 40-character commit SHA with a trailing `# vX.Y.Z` comment. Tags are mutable. | `rg -nP 'uses: .*@(?![0-9a-f]{40})' .github/workflows/` is empty | MUST |
| CI-03 | Every `actions/checkout` sets `persist-credentials: false` — otherwise `GITHUB_TOKEN` stays in the git credential helper for every later step, third-party actions and build scripts included. | Count of `persist-credentials: false` equals count of checkout steps | MUST |
| CI-04 | Every `cargo` invocation in CI carries `--locked`. Without it CI resolves a dependency set the lockfile does not describe — masking the exact drift CI exists to catch. | `rg -nP '^\s*(?:- )?(?:run: )?cargo [bctn][a-z]+(?!.*--locked)' .github/ taskfiles/` returns no `build`, `check`, `clippy`, `test` or `nextest` step | MUST |
| CI-05 | CI invokes the same named task target a developer runs locally; never a bare cargo command with no local equivalent. | Every `run:` in a Rust job is `task <target>` and the target exists | SHOULD |
| CI-06 | A `continue-on-error: true` step is either paired with a later step failing the job on its recorded `outcome`, or lives in a job explicitly labelled non-blocking. An unpaired one is a check that can never be red. | Every `continue-on-error: true` has a `steps.<id>.outcome` reference or a `# non-blocking:` marker | MUST |
| CI-07 | Split `cargo deny`: `check bans licenses sources` blocks the PR; `check advisories` is non-blocking on PRs and blocking on a schedule. The advisory DB changes without your code, so a PR-blocking advisories gate punishes an unrelated commit and trains people to bypass it. | Two legs with differing `continue-on-error`; a `schedule:` workflow runs `cargo deny check advisories` blocking | MUST |
| CI-08 | Every `[advisories].ignore` entry and every non-default `[licenses].allow` entry carries an inline comment stating the machine-checkable condition for its removal. | Every `ignore` line is preceded by a comment containing `REMOVE when` | MUST |
| CI-09 | Restrict `Swatinem/rust-cache` saves to trunk (`save-if: github.ref == 'refs/heads/main'`) — PR branches otherwise evict the shared cache under the per-repo size cap. | Every `rust-cache` step has a `save-if` | SHOULD |
| CI-10 | Unit tests run natively on **every** OS that is a release target, or the workflow comments why that target is build-only. Cross-compiling an artifact you never execute tests on is an untested release. | Each release target OS appears in a matrix running `cargo nextest run`, or is annotated | SHOULD |
| CI-11 | With a merge queue, add `merge_group: { types: [checks_requested] }` to every workflow producing a required check — omitting it makes the check never report and wedges the queue silently. | Required-check list ⊆ workflows containing `merge_group` | CONSIDER |
| CI-12 | Report LINT-15's ratcheted invocation on PRs through a SHA-pinned `giraffate/clippy-action` with `reporter: github-pr-review`, advisory only — line-scoped review misses a lint whose span does not overlap a changed line, and it shrinks no existing backlog. | The action step exists and the LINT-16 ratchet step still runs alongside it | CONSIDER |

## Release and Distribution

| ID | Rule | Verification | Severity |
|---|---|---|---|
| REL-01 | Ship from an explicit named profile (`[profile.dist]`), never Cargo's `release` defaults, and comment each setting with its measured effect — `lto=false, codegen-units=16, strip="none"` is tuned for iteration, not distribution. | `[profile.dist]` sets at least `lto`, `codegen-units`, `opt-level`, `strip` | MUST |
| REL-02 | Never set `panic = "abort"` in a profile covering crates that call `catch_unwind` or `resume_unwind`. Record the rejection, with its measured size cost, as a comment in the profile — it compiles clean and removes panic propagation *silently*. | If `panic = "abort"` appears, `rg -n --type rust --glob '!external/**' -e catch_unwind -e resume_unwind .` is empty for the covered crates | MUST |
| REL-03 | Every Linux artifact is static-musl or has a documented glibc floor. Never an unpinned `-gnu` build assumed portable — it links the runner's glibc, and CI cannot catch it because CI ran on that glibc. | Every `-linux-gnu` target has a glibc suffix, a pinned older builder image, or a documented floor | MUST |
| REL-04 | Release artifacts carry embedded dependency data (`cargo-auditable`), an SBOM, and a signed build-provenance attestation. The first two answer "what is in this binary", the third "did it come from this repo". | `cargo-auditable = true` and `cargo-cyclonedx = true` in the dist config; the release workflow runs an attestation step with `attestations: write` | MUST |
| REL-05 | Validate the release configuration on pull requests — `pr-run-mode = "plan"` or a dedicated readiness workflow. Otherwise a config error surfaces only after the tag is pushed. | `pr-run-mode = "plan"`, or a PR-triggered workflow exercising the release config end to end | SHOULD |
| REL-06 | No long-lived publishing credential in repository secrets: job-scoped `GITHUB_TOKEN` for GHCR, OIDC trusted publishing for crates.io. A standing token publishes malicious versions indefinitely once any dependency of the publish job is compromised. | `rg -n -e CARGO_REGISTRY_TOKEN -e '_TOKEN: \$\{\{ secrets' .github/workflows/` returns only ephemeral tokens | SHOULD |
| REL-07 | If release automation parses commit messages, enforce the commit convention as a blocking PR check — version derivation from `feat:`/`fix:`/`!` is silently wrong for any commit that skipped it. | `cliff.toml`/`cog.toml` present ⇒ a required conventional-commit job exists | MUST |

## What Agents Get Wrong Here

1. **Suppressing the lint instead of fixing the code.** The tell is a bare
   `#[allow(clippy::…)]` in the same diff that introduced what it silences.
   Reject any diff adding an `#[allow]` or a reasonless `#[expect]`.
2. **Widening a suppression's scope instead of adding a new one** — function →
   module → crate, so the diff shows no new attribute. LINT-17 rejects new `#![…]`.
3. **`.unwrap()`/`.expect()` as the default error strategy** — training data is
   saturated with tutorial code. LINT-05 plus LINT-09's test allowances.
4. **Bare `as` casts between integer widths.** `usize as u32` on a file offset
   truncates silently, and `usize` differs across shipped platforms.
5. **Whole-group `cargo clippy --fix` in one pass** — dozens of lint codes in one
   diff, one of which can be a compiling-but-wrong rewrite. LINT-18: one per commit.
6. **Stale or hallucinated Action versions and inputs** — `actions/checkout@v3`,
   invented `with:` keys. SHA pinning forces a real lookup; still cross-check
   every `with:` key against the action's `action.yml`.
7. **Dropping `--locked`** when writing or editing a CI cargo command.
8. **Holding a `std::sync::MutexGuard` across `.await`.** Compiles, looks right,
   deadlocks only under concurrency — invisible to a single-threaded test. The
   one failure here an agent cannot pattern-match its way out of.
9. **`panic = "abort"` added as a "free" size win**, without checking for
   `catch_unwind`/`resume_unwind`.
10. **Writing `channel = "stable"`** when asked to set up a toolchain — the
    loosest thing that works locally, and drift is unobservable in one session.
11. **Calling `std::env::set_var` as ordinary safe code.** Edition 2024 made it
    `unsafe`; models either write it plainly or wrap it in a copied, wrong
    `unsafe` block. LINT-09's `disallowed-methods` fires either way.
12. **Making `cargo deny check advisories` a hard PR gate** on the "strict is
    safer" instinct — it fails every PR the day an unfixable transitive
    advisory lands.
13. **Satisfying a doc lint with content-free prose** — "returns an error if
    something goes wrong". No grep catches it; LINT-12 keeps it a review item.
14. **Confusing `cross` and `cargo-zigbuild` syntax** — the `.2.17` glibc
    target suffix is zigbuild-only.
15. **Adding a dependency that duplicates one already in the tree**, then
    leaving the loser in `Cargo.toml`. `cargo shear` catches only the unused
    half; the duplicate-purpose half needs a read of `[dependencies]`.

## Sources

- [Cargo Book — the `[lints]` section](https://doc.rust-lang.org/cargo/reference/manifest.html#the-lints-section) — syntax, `priority`, workspace inheritance
- [rustc book — lint levels](https://doc.rust-lang.org/rustc/lints/levels.html) — `#[expect]` semantics and `reason =`
- [Clippy lint configuration](https://doc.rust-lang.org/clippy/lint_configuration.html) — group definitions and the full `clippy.toml` knob list
- [rustfmt `Configurations.md`](https://raw.githubusercontent.com/rust-lang/rustfmt/master/Configurations.md) — the authoritative stable/unstable split
- [rustup — overrides](https://rust-lang.github.io/rustup/overrides.html) — `rust-toolchain.toml` fields and precedence
- [GitHub — security hardening for Actions](https://docs.github.com/en/actions/security-guides/security-hardening-for-github-actions) — SHA pinning, token scope, credential persistence
- [EmbarkStudios/cargo-deny-action](https://github.com/EmbarkStudios/cargo-deny-action) — the advisories-split pattern, verbatim from upstream
- [Cargo Book — profiles](https://doc.rust-lang.org/cargo/reference/profiles.html) — the defaults every REL-01 override is measured against
