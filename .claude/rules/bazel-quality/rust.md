---
title: Rust under Bazel
summary: The BZL-RUST family — toolchain pinning, crate_universe lockfiles and repin, build scripts, test and doc-test targets, the lint aspects, gazelle_rust and prost
---

# Rust under Bazel

Owns `BZL-RUST`: rules_rust toolchain registration and platform triples,
crate_universe (`crates_repository`, `crates_vendor`, annotations, private
registries), `cargo_build_script`, `rust_test`/`rust_doc_test` target parity,
the clippy and rustfmt aspects, gazelle_rust, prost/tonic, and the Rust half of
coverage. It does not own `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`,
`clippy.toml` or `rustfmt.toml` hygiene — the `rust-cargo` and `rust-quality`
rule sets own those files, and a rule here touches one only where the edit is
Bazel-specific. Sibling families, cited never restated: BZL-MOD owns
`MODULE.bazel.lock` and the Bzlmod lockfile modes; BZL-HERM owns network
reachability, the action environment and `--repo_env` versus `--action_env`;
BZL-CACHE owns credential helpers and cache flags; BZL-TEST owns coverage
flags, `manual`-tag semantics and the Windows coverage verification; BZL-ARCH
owns generator maturity and the `git_override`-versus-`bazel_dep` decision;
BZL-PY owns reaching a Rust binary from a Python test (BZL-PY-25).

Contents: [The crate_universe Freshness Gate](#the-crate_universe-freshness-gate) ·
[Pinning the Toolchain](#pinning-the-toolchain) ·
[Build Scripts and Crate Annotations](#build-scripts-and-crate-annotations) ·
[Test and Dependency Targets](#test-and-dependency-targets) ·
[Clippy, rustfmt and Coverage](#clippy-rustfmt-and-coverage) ·
[gazelle_rust](#gazelle_rust) ·
[Protobuf, the IDE Handback and the Repin Job](#protobuf-the-ide-handback-and-the-repin-job) ·
[Gaps](#gaps) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Measured 2026-09-06 against Bazel 8.7.0, 8.8.0 and 9.2.0 on a Linux host; a row
whose result could differ off Linux says so, and a `bazel help` read is a
property of the binary, not the host. Rows bind
[rules_rust 0.74.0](https://github.com/bazelbuild/rules_rust/releases/tag/0.74.0)
(2026-08-28), its separate `rules_rust_prost` module at the same version, and
gazelle_rust 0.1.0; a few source facts were read at `main`@51f3042 (2026-09-02),
strictly after the tag. crate_universe is under active repair — seven of roughly
32 entries in the 0.74.0 changelog are crate_universe correctness fixes from the
preceding six weeks — so re-read
[`crate_universe/extensions.bzl`](https://github.com/bazelbuild/rules_rust/blob/0.74.0/crate_universe/extensions.bzl)
at the tag you pin rather than trusting this as settled behaviour. Before citing
any Bazel flag, confirm it on **both** help surfaces at your pinned version:
`bazel help <command> --long` **and** `bazel help startup_options` — a flag lives
in either, and a reader who checks one surface misses the other. Every grep here
reads your own `MODULE.bazel`, `BUILD.bazel`, `.bzl` and `.rs` text; the BUILD
and `.bzl` content crate_universe renders into `@crate_index` and its spokes is
out of a grep's reach by construction, so where a rule's subject is generated,
its verification reads the configuration that produced it. MUST = Block,
SHOULD = Warn, CONSIDER = Suggest; **pinned** marks a default an adopter
overrides once, in their own config, never per call site.

## The crate_universe Freshness Gate

Rust's freshness gate is the inverse of Bzlmod's: `determine_repin()` shells to
`cargo-bazel query` and hard-`fail()`s on a digest mismatch on **every** ordinary
build, so there is no flag to add — the thing that must be *absent* is an
environment variable. Three reads cover this whole block, and the merge-driver
shape is BZL-MOD-04's, reused for a different file:

```bash
grep -n -A20 -e 'crates_repository(' -e 'crate.from_cargo(' -e 'crate.from_specs(' MODULE.bazel WORKSPACE*
grep -rn -e CARGO_BAZEL_REPIN -e CARGO_BAZEL_ISOLATED -e 'bazel sync' .bazelrc* .github/workflows/
git check-attr merge cargo-bazel-lock.json
```

Empty output from the first means this module declares no crate-universe
instance and the whole block is N/A.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-RUST-01 | Set an explicit `lockfile =` on every crate-universe instance — `crates_repository(lockfile = "//:cargo-bazel-lock.json")` under WORKSPACE, `crate.from_cargo(lockfile = ...)` or `crate.from_specs(lockfile = ...)` under Bzlmod — including in a non-root module. | Without it `determine_repin()` returns `True` unconditionally: every build silently re-splices, forever, and no freshness check is possible at all. In a non-root Bzlmod module the same omission is an outright `fail()`, because repinning is disabled across module boundaries. This is the single worst configuration in the ruleset and the one an unsure agent reaches for. | The first block grep. An instance whose call carries no `lockfile =` is the finding; **empty output — no crate-universe instance in this module — is the pass.** Ruleset behaviour, identical on 8.7.0 and 9.2.0. Note `cargo_lockfile` (Cargo's own `Cargo.lock`) is a different attribute and does not satisfy this. | MUST |
| BZL-RUST-02 | Scope every `CARGO_BAZEL_*` variable to the one job that needs it — `CARGO_BAZEL_REPIN` to a manually or schedule-triggered repin job, `CARGO_BAZEL_ISOLATED=false` to a named private-registry job — and reach for a committed `cargo_config =` file before either. | The always-on `REPIN` inverts the only gate into a silent regenerate; a global `ISOLATED=false` re-admits the host `~/.cargo/config.toml` state that `isolated = True` exists to keep out of generated targets. No `--crate_lockfile_mode` or `--locked` flag exists to add instead. The two network paths differ: per-crate tarball fetches render as plain `http_archive` calls covered by `--credential_helper` (BZL-CACHE-04), while the `cargo-bazel` splice and query phase is a `repository_ctx.execute()` subprocess Bazel's downloader cannot see. | The second block grep. **Empty in the build and test lanes is the pass — the default gate is intact.** A `REPIN` hit reachable from a `push` or `pull_request` trigger is the finding; a global `ISOLATED=false` with no note saying why `cargo_config =` was insufficient is the finding. Name which of the two network paths is failing before changing either. | MUST |
| BZL-RUST-03 | Repin with `CARGO_BAZEL_REPIN=1 bazel fetch --repo=@<repo>`, or with a plain build of a target in that repo; never `bazel sync --only=<repo>`, whatever the ruleset's own docs print. | `bazel sync` was deleted in [Bazel 9.0.0](https://github.com/bazelbuild/bazel/releases/tag/9.0.0) (GA 2026-01-20), while the ruleset's `extensions.bzl` docstring and its rendered crate_universe page still instruct it — so copying from the primary source is exactly what produces a command that hard-fails the day your pin crosses 9.0.0. | The second block grep plus `grep -rn 'bazel sync' . --include='*.md' --include='*.sh' --include='*.yml' --include='*.bzl'`; **empty is the pass.** Then confirm the replacement at your pin: `bazel help fetch --long` must list `--repo`, `--all`, `--configure` and `--force` — all four measured present on 8.7.0 and 9.2.0, so the replacement needs no version branch. **Empty output from `bazel help` for a flag name means the flag does not exist at that version — an answer, not a pass.** | MUST |
| BZL-RUST-04 | Drift-check a `crates_vendor` setup in CI with `bazel run //<pkg>:crates_vendor && git diff --exit-code -- <vendor_path>`, repin it with `bazel run //<pkg>:crates_vendor -- --repin`, and tag the target `tags = ["manual"]`. | `crates_vendor` writes committed source that no ordinary build re-derives, and `determine_repin()`'s fail-fast guards fetch-time repos only — a green build proves nothing about vendored-tree freshness. `CARGO_BAZEL_REPIN=1` on a build step does nothing here, silently. The `manual` tag keeps an `executable` target out of wildcard builds (BZL-TEST-10 owns that tag's semantics). | The two-command pipeline: **exit 0 with an empty diff is the pass**, a non-empty diff is stale vendored output to review before merge. Then `bazel query 'attr(tags, manual, kind(crates_vendor, //...))'` must return the same count as `bazel query 'kind(crates_vendor, //...)'` — **equal counts, including both empty (no such target exists), is the pass.** Same query syntax on 8.7.0 and 9.2.0. | MUST once `crates_vendor` is adopted; N/A otherwise |
| BZL-RUST-05 | Give the file named by `lockfile =` a JSON-aware git merge driver; never `union`, `ours` or any line-based driver, and never point Bazel's own `bazel-lockfile-merge` driver at it. | The lockfile carries an opaque generator-computed `checksum` that a line-based merge desyncs with **no conflict markers at all** — a silently bad merge is strictly worse than a visible conflict, and it defeats conflict-marker detection downstream. Bazel's jq driver parses the `MODULE.bazel.lock` schema, not this one. | `git check-attr merge cargo-bazel-lock.json` (or whatever `lockfile =` names). **Anything other than a JSON-aware driver — `union`, `ours`, `unspecified` — is the finding; there is no reading in which empty passes.** A `.gitattributes` line alone does nothing: confirm `git config merge.<driver>.driver` resolves in the clone too. | MUST |
| BZL-RUST-07 | **pinned** — Default to `crates_repository` / `crate.from_cargo` for a repository nothing else builds against; switch to `crates_vendor` (`mode = "remote"`) the moment another module `bazel_dep`s, `git_override`s or BCR-publishes on this one. | A non-root Bzlmod consumer of `crate.from_cargo` can never repin and must trust a producer-owned lockfile permanently; checked-in BUILD files remove that dependency entirely, since the consumer never fetches `cargo-bazel` or evaluates a repository rule. The switch is not free — the vendor repin job forwards its whole environment (BZL-RUST-36). No primary source ranks the two; this is a project decision an adopter may reverse once, in their own config. | Reading heuristic: does anything outside this repository consume it as a module today or in the stated plan? **Yes, while `crates_repository`/`from_cargo` is in use, is a finding to discuss, never a mechanical failure. No consumer means the default stands as written.** | SHOULD |

```python
# wrong — Cargo's lockfile is not crate_universe's; no freshness gate exists
crate.from_cargo(name = "crates", cargo_lockfile = "//:Cargo.lock")
# right — the generated lockfile is what determine_repin() hashes
crate.from_cargo(name = "crates", cargo_lockfile = "//:Cargo.lock", lockfile = "//:cargo-bazel-lock.json")
```

```bash
CARGO_BAZEL_REPIN=1 bazel sync --only=crates   # wrong — command deleted in Bazel 9.0.0
CARGO_BAZEL_REPIN=1 bazel fetch --repo=@crates # right — flag present on 8.7.0 and 9.2.0
```

## Pinning the Toolchain

The check for this block is one read of `MODULE.bazel` plus the CI matrix's
platform list: `grep -n -A6 'rust.toolchain(' MODULE.bazel`. Empty output in a
module that never uses the `rust` extension means no Rust toolchain here and
nothing to check; empty output in a module that *does* `use_extension` it is
BZL-RUST-31's finding, not a pass.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-RUST-31 | Call `rust.toolchain(versions = [...], edition = "...")` in the root module and pin every entry; give a `beta` or `nightly` entry its `/YYYY-MM-DD` ISO date, and drop the channel you do not select. | The default is hermetic but not pinned and it moves: `DEFAULT_RUST_VERSION` went 1.95.0 → 1.98.0 across tags 0.70.0–0.74.0, changing on four of five releases, so a `bazel_dep` bump alone re-pins your compiler. Worse, omitting the call is not an error — the extension selects `root.tags.toolchain or rules_rust.tags.toolchain` and rules_rust's own non-dev registration backstops it, so the build succeeds with nothing in your `MODULE.bazel` naming a version. The default list also registers and downloads a nightly toolchain that a `channel` setting defaulting `"stable"` will never pick. | The block grep. **A call with no `versions =` is the finding. No `rust.toolchain(` at all in a module that `use_extension`s rules_rust's `rust` extension is the same finding and the more dangerous one — the build works, so nothing else surfaces it.** A bare `"nightly"`/`"beta"` without an ISO date fails at load time on 8.7.0 and 9.2.0 (self-verifying). Empty grep with no `rust` extension anywhere = nothing to check. | MUST |
| BZL-RUST-08 | Set `supported_platform_triples` to exactly the platforms your CI matrix and developer hosts build for: add the ones the curated default omits — Intel macOS (`x86_64-apple-darwin`) and ARM64 Windows (`aarch64-pc-windows-msvc`) — remove any entry no target resolves to, and never reason about this list from `rust.toolchain()`'s `extra_target_triples` or the reverse. | Splicing is `O(N²)` per added triple (the ruleset's own source comment), so a speculative entry is paid on every repin, while a missing one resolves a CI leg wrongly. The two curated lists live in different files with different members — `DEFAULT_EXTRA_TARGET_TRIPLES` carries three wasm triples and no Windows; `SUPPORTED_PLATFORM_TRIPLES` carries neither wasm nor Intel macOS — and adjacent prose calls both "the curated default seven", which is what makes substituting one for the other look safe. | Cross-reference the CI matrix's OS and `--platforms=` list against `supported_platform_triples` in `MODULE.bazel`. **A CI or developer platform absent from the list is the finding; a list entry nothing resolves to is the second; a claim about "the default triples" that does not name which of the two attributes was read is the third.** An unset attribute means the curated seven apply, which is the finding whenever the matrix names a platform outside them. This reads your configuration only — the generated crate-repository `.bzl` and BUILD text is out of a grep's reach. | MUST |
| BZL-RUST-15 | Express host-dependent exec-configuration rustc flags with `extra_exec_rustc_flags_triples` on the single `rust.toolchain()` tag; never combine it with `extra_exec_rustc_flags` on that tag, and never register a second `rust_toolchain` to give proc macros their own. | Setting both attributes on one tag is a hard `fail()` in the extension. And `rust_toolchain` bundles `rustc`, `rustc_lib`, `cargo`, `clippy_driver`, `rustfmt` and `rust_std` on one rule with no supported way to swap a component, so a second registration duplicates the entire asset bundle instead of fixing the layer that is actually wrong — an upstream discussion open since 2022 with no fix. | `grep -n -e extra_exec_rustc_flags -e extra_exec_rustc_flags_triples MODULE.bazel`. **Both attributes on one call already fail the build on 8.7.0 and 9.2.0 (self-verifying); empty output means no exec-flag customisation and is the pass.** The reviewable half is the design: a proposed cross-compilation fix that registers a new toolchain set rather than touching the triples attribute is the finding. | MUST once a second exec platform (Darwin or Windows) is in the matrix; N/A on a single-platform build |

## Build Scripts and Crate Annotations

One grep sweep over first-party build scripts plus one read of the annotation
call sites covers this block:

```bash
grep -rln -e vergen -e git2 -e gix -e 'Command::new("git")' -e 'pkg_config::' -e 'cc::Build' --include=build.rs .
grep -n -e 'crate.annotation(' -e build_script_env -e use_cc_toolchain MODULE.bazel
```

Empty output from the first means no first-party build script reads the
repository or probes the host. Third-party crates' `build.rs` sources are not in
your tree, so the reachable check for them is the annotation list, never their
source. Network reachability inside a build script is BZL-HERM-02's row, not
one of these.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-RUST-12 | Treat any `build.rs` that opens `.git` — directly or through `vergen`, `vergen-gix`, `git2`, `gix` or `Command::new("git")` — as taking its no-git fallback on **every** Bazel build, and require that fallback to be non-panicking. | `CARGO_MANIFEST_DIR` under `cargo_build_script` is a synthesized runfiles-backed path that never contains `.git`, and nothing declares `.git` as an input — declaring it would defeat action caching, since its content changes every commit. A crate whose only git path is `.unwrap()`ed panics on every Bazel build; one with a graceful fallback silently produces a different binary than `cargo build` does from the same commit, with only a `cargo:warning` line in the action log. | The first block grep, then read each hit for a `match` or `?` short-circuit rather than `.unwrap()`/`.expect()`. **A hit with no fallback is the finding; a hit with one is a decision to record under BZL-RUST-13, not a pass by default; empty output is nothing to check.** `root_path` (added at 0.74.0) does not weaken this — it forwards a compile-time crate root in `cargo_build_script_wrapper.bzl` and never touches the `_cargo_build_script_impl` that computes `CARGO_MANIFEST_DIR`. When a rules_rust diff is claimed to affect this rule, read which of those two files it touches: only the latter could. | MUST |
| BZL-RUST-13 | Resolve a provenance field under Bazel through `--workspace_status_command` plus a `rustc_env_files` target with bracketed `{STABLE_...}` placeholders, and set `stamp` explicitly on the target rather than inheriting the rule default. | The status command runs once per build **outside every action sandbox**, with real `.git` access — the only mechanism that can reach it. Rule defaults diverge silently: `rust_binary`'s `stamp` is `-1` (defer to `--stamp`) and `rust_library`'s is `0` (never), so a library and a binary in one crate family behave differently with no warning, and `cargo_build_script` has no `stamp` attribute at all (BZL-HERM-18). | For consumer code reading a `VERGEN_GIT_*`-shaped variable, confirm both a `rustc_env_files` target with a `{STABLE_...}` placeholder and a `build --workspace_status_command=` line in `.bazelrc`. **Neither present is the finding — that field is permanently `None` under Bazel and nobody chose that.** Then `bazel query 'attr(stamp, "-1", kind(rust_binary, //...))'` enumerates every binary deferring to the flag; **empty means none defers, which is the pass; exit 2 with empty stdout is a broken command, never a pass.** Same query syntax on 8.7.0 and 9.2.0. | MUST wherever a provenance field must resolve; N/A where its absence is a recorded decision |
| BZL-RUST-16 | Give every closure crate whose `build.rs` probes the host — `pkg-config`, a `*-sys` system-library lookup, any `PATH` tool search — a `crate.annotation(build_script_env = {...})` naming the exact variable that forces the self-contained path. Never a patched fork. | `use_default_shell_env` defaults `True` so the probe reads the ambient `PATH`, which makes a live hermeticity break look like intended behaviour. What that environment holds is itself version-dependent: `--incompatible_strict_action_env` (the per-version default is BZL-FLAG-21's and the rc pin is BZL-HERM-01's; read it off your own pin), so one build script sees two different environments across a mixed CI matrix (BZL-HERM-01). A fork costs maintenance the annotation does not. | For every closure crate using `pkg_config::` or shelling to `pkg-config`, confirm a matching `crate.annotation` in `MODULE.bazel` or `crates.bzl`. **A probing crate with no matching annotation is the finding; an annotation list covering every probing crate is the pass.** The crate's own source is not in your tree and the generated repository's BUILD and `.bzl` text is out of a grep's reach, so the annotation list is the reachable subject. | MUST |
| BZL-RUST-17 | Never port a `crate.annotation()` call between the WORKSPACE macro form and the Bzlmod tag-class form by substitution — read the declared attribute type on both sides first. | The two are documented as kept "in sync" but do not share value encodings: `build_script_use_cc_toolchain` is an `int` (unset/`1`/`0`) in the macro and a three-valued **string** (`"auto"`/`"on"`/`"off"`, default `"auto"`) in the tag class. A pattern-matched port silently changes the attribute's type, and the "in sync" comment is precisely what makes it look safe. | Named reading heuristic: for each attribute crossing the boundary, read its `attr.*()` declaration in `crate_universe/private/crate.bzl` against the annotation attrs in `crate_universe/extensions.bzl` **at your pinned rules_rust version**. **No grep substitutes and there is no empty case — both encodings are valid Starlark, neither errors, and the wrong one is accepted in silence, so the type diff itself is the check.** | MUST |
| BZL-RUST-14 | On a Rust-only repository set **both** `common --repo_env=BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1` and `--@rules_rust//cargo/settings:use_cc_toolchain=false`; in a mixed Rust/C workspace set `crate.annotation(build_script_use_cc_toolchain = ...)` per crate instead of the global flag. | Core rules mark the C++ toolchain type `mandatory = False`, but `use_cc_toolchain` still defaults `True` ruleset-wide and Bazel's C++ autoconfiguration probe runs at module-extension setup regardless of graph content — so one switch leaves the other cost in place. The global flag is `scope = "universal"`: turning it off breaks every genuinely C-compiling build script (`openssl-sys`, `libsqlite3-sys`) with a late `No binary provided for cc` at execution time, which the per-crate attribute exists to avoid. | `grep -n -e use_cc_toolchain -e BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN .bazelrc* MODULE.bazel` plus `grep -rln -e 'cc::Build' -e 'cmake::Config' -e 'pkg_config::' --include=build.rs .`. **Empty on the second with no override on the first is the finding — autodetection and a toolchain pull paid for nothing. A build.rs hit alongside a global `false` is the finding — that crate fails at execution time.** | SHOULD |

```python
crate.annotation(build_script_use_cc_toolchain = 0)      # wrong under Bzlmod: int is the WORKSPACE encoding
crate.annotation(build_script_use_cc_toolchain = "off")  # right: the tag class declares a three-valued string
```

## Test and Dependency Targets

One `bazel query` pass per package, plus a grep of the crate's own sources.
Query syntax is identical on 8.7.0 and 9.2.0. **An empty query result is the
finding in this block, not the pass** — every rule here exists because a missing
target produces a green build that tests nothing.

```bash
bazel query 'kind(rust_test, //<pkg>:*)'
bazel query 'tests(//<pkg>:<suite>)'
bazel query 'kind(rust_doc_test, //<pkg>:*)'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-RUST-30 | Confirm every `rust_test_suite`'s resolved test list is non-empty before trusting its `srcs` glob. | `test_suite` special-cases an empty `tests` list as "run everything in the package", and `rust_test_suite`'s synthetic `restrict_<name>` tag exists purely to suppress that — so a glob matching nothing (a typo'd path, a renamed directory) yields a target that exists, runs zero tests and reports green. The ruleset ships a regression fixture for exactly this case. | `bazel query 'tests(//<pkg>:<suite>)'`. **Empty output for a suite whose `tests/` directory holds files is the finding — wrong glob path, renamed directory, or never wired to real sources. The expected target list is the pass.** | MUST |
| BZL-RUST-27 | Give every `#[cfg(test)]` module a `rust_test(crate = ...)` and every file under a crate's `tests/` directory its own `rust_test` (direct or `rust_test_suite`-generated), setting `crate_root` explicitly whenever `srcs` holds more than one file; certify migration parity by diffing test **names**, never counts. | Cargo auto-discovers unit and integration tests from one crate layout while Bazel needs an explicit target for each, so a missing one is a permanent, silent coverage gap with no build error. `crate_root` inference covers only `lib.rs`, `main.rs` or a single-file `srcs`, so a test pulling in `tests/common/mod.rs` falls outside it. A generated `<suite>_<path>_test` name never matches Cargo's, so an equal or higher Bazel count can still hide a dropped test. | Per crate, `grep -rn '#\[cfg(test)\]' src/` against `bazel query 'kind(rust_test, //<pkg>:*)'`, and one target per `tests/*.rs` file. **A source pattern with no corresponding target is the finding; an empty grep for a crate holding no unit tests is nothing to check.** Then set-diff `cargo test -- --list` (or `cargo nextest list`) against the query output by normalized name — **a count comparison alone is not the check.** | MUST for any migration claiming test parity with Cargo |
| BZL-RUST-28 | Give every crate whose public items carry runnable fenced doc examples a `rust_doc_test(crate = ...)` target. | `cargo test --doc` is the one Cargo test kind Bazel never generates under any mechanism — no glob, aspect or macro reads doc comments the way `rust_test_suite` reads `tests/`. `rust_doc_test`'s `crate` attribute is mandatory and there is no `srcs` or glob variant, so the examples stop being tested with nothing anywhere to say so. | ````grep -rn -e '^///.*```' -e '^//!.*```' --include='*.rs' src/```` per crate, against `bazel query 'kind(rust_doc_test, //<pkg>:*)'`. **A non-empty grep with an empty query is the finding; an empty grep is nothing to check.** | MUST when doc examples exist; N/A otherwise |
| BZL-RUST-29 | List every fixture a test reads at run time in `data` — never in `compile_data`, never as an ambient relative path. | `data` is what Bazel stages into the test's runfiles; `compile_data` exists for `include_str!()`-style compile-time inclusion only. A test reading a fixture with neither passes locally against the real filesystem and fails hermetically — in the sandbox, on a clean checkout, in CI — with a "file not found" that names nothing useful. | Grep the test sources for `std::fs::read`, `File::open` and relative path literals, and cross-check the owning target's `data` list. **A referenced path absent from `data` is the finding; no runtime file reads is the pass.** | MUST |
| BZL-RUST-18 | Write every edge between first-party workspace crates as a hand-written `deps = ["//crates/<name>"]` entry on the BUILD target; never expect crate_universe to wire a Cargo `path = "..."` dependency. | cargo-bazel's metadata pass filters every workspace-member node out of the crate-rendering set before generating BUILD content, so no code path could wire this even in principle — a path dependency never appears in `Cargo.lock` at all. The failure reads as "crate_universe is broken" rather than "this edge needed declaring". | For each `path = "..."` dependency in a workspace member's `Cargo.toml`, confirm the sibling crate's own label in that target's `deps`, never a generated `@crate_index//` alias. **A path dependency with no matching hand-written entry is the finding; every edge declared is the pass.** Self-verifying at `bazel build` time, worth catching in review because the error misdirects. | MUST |

## Clippy, rustfmt and Coverage

Two reads catch the lint rows — the `.bazelrc` registration and a BUILD-file
grep for an attribute that does not exist — and one query catches the coverage
row. The gate command a repository runs is the aspect build:

```bash
bazel build --aspects=@rules_rust//rust:defs.bzl%rust_clippy_aspect --output_groups=+clippy_checks //...
grep -rn -e '^[[:space:]]*clippy[[:space:]]*=' -e '^[[:space:]]*rustfmt[[:space:]]*=' --include=BUILD.bazel .
grep -n -e rust_clippy_aspect -e rustfmt_aspect .bazelrc*
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-RUST-23 | Gate clippy and rustfmt through `.bazelrc` aspect registration plus `--output_groups=`, and treat the two opt-out tag families as disjoint sets. | No rule declares a `clippy` or `rustfmt` attribute in any `attrs` dict — the mechanism is the aspect plus its output group, applied to every Rust target once registered. Read from the aspects' own sources, clippy's ignore list is `no_clippy`/`no_lint`/`nolint`/`noclippy` and rustfmt's is `no_format`/`no_rustfmt`/`norustfmt`: hyphens and case normalize, but `no-lint` does **not** silence rustfmt and `no-format` does **not** silence clippy. | The BUILD-file grep: **any hit is invalid for these rules on every Bazel version and is the finding.** Then the `.bazelrc` grep: **empty output means the lints are not gated at all**, which is its own finding in a repository claiming to enforce them. For each skip-tagged target, confirm the commit names which aspect it skips; a tag from one family with the other aspect still running is a review finding. | MUST |
| BZL-RUST-24 | Register the rustfmt aspect under a named CI config (`build:ci --aspects=...`, or a CI-only rc file), never under the bare `build` config every local build inherits. | Upstream states the reason directly: enabling it outside CI means formatting issues block a developer's ability to iterate rapidly. Clippy's docs name no equivalent recommendation — silence, not a stated exception — so clippy stays in the default config. | `grep -n rustfmt_aspect .bazelrc*`. **A hit on a bare `build --aspects=` line is the finding; a hit under a named config (`build:ci`, a CI rc file) is the pass; empty output is BZL-RUST-23's finding, not this one.** | SHOULD |
| BZL-RUST-26 | State, wherever a Rust coverage number is reported or reviewed, that `rust_test(crate = ...)` instruments the crate's own `#[cfg(test)]` code even without `--instrument_test_targets`, and budget that test's `size`/`timeout` for the llvm-cov post-processing that runs inside the same spawn. | rules_rust documents the instrumentation half as an intentional break from the Bazel-wide convention — the whole crate compiles as one unit — so a reviewer carrying the C++/Java model misreads test code in the report as a leak. The budget half is Bazel's: the coverage post-processing shares the test's own spawn and budget (BZL-TEST-02 owns the flag and its per-version default). A standalone integration `rust_test` with its own `srcs` follows the normal convention; only the `crate = ...` wrapper diverges. | `bazel query 'attr(crate, ".+", kind(rust_test, //...))'` before citing a percentage. **Non-empty means the number includes test code by design; empty means the normal convention applies. Neither outcome is a defect — a report published without the distinction is.** A `rust_test` that times out only under `bazel coverage` is the shared-budget symptom: raise `timeout`, do not retry. | CONSIDER |

## gazelle_rust

crate_universe has no mechanism for keeping hand-written internal `deps` in sync
with source `use` statements (BZL-RUST-18); gazelle_rust is the only tool that
resolves imports across the first-party and crate_universe namespaces at once.
BZL-ARCH-11 grades Rust generation experimental and BZL-ARCH-13 owns the
generated-file discipline; both apply at full force here. The check is
`bazel query 'kind(gazelle_test, //...)'` plus a grep of the directives. Read
the plugin's current version from the Bazel Central Registry's `gazelle_rust`
metadata rather than from memory: 0.1.0 (2026-05-05) is its only tag.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-RUST-34 | Point gazelle_rust at the crate_universe lockfile with `# gazelle:rust_lockfile cargo-bazel-lock.json` and set `# gazelle:rust_crates_prefix`; never `# gazelle:rust_cargo_lockfile` on a raw `Cargo.lock`. | Parsing a raw `Cargo.lock` includes every transitive dependency, so unused-dependency reporting flags them all and gazelle can add a `deps` entry for a transitive-only crate that has **no corresponding top-level target**, breaking the build — the maintainer's own open bug, [gazelle_rust#15](https://github.com/Calsign/gazelle_rust/issues/15). Direct-dependency parsing from crate_universe's lockfile is the preferred direction and is not shipped. | `grep -rn 'gazelle:rust_cargo_lockfile' .` — **an unexplained hit is the finding; empty is the pass.** Then `grep -rn -e 'gazelle:rust_lockfile' -e 'gazelle:rust_crates_prefix' .` — a lockfile directive with no `rust_crates_prefix` is the second finding, because resolution then has nowhere to point. Presence of *a* lockfile directive is not the check. | MUST wherever gazelle_rust is adopted; N/A otherwise |
| BZL-RUST-35 | Mark every `use` behind a `#[cfg(...)]` whose Bazel dependency resolves through a `select()` with `#[gazelle::ignore]`, and put `# keep` on the corresponding BUILD `deps` line. | gazelle_rust cannot infer a platform-conditional dependency — its README says so and documents exactly this pair as the workaround. Without both halves gazelle rewrites the hand-authored `select()` away on **every** regeneration, so the loss is silent, recurring, and reads as a merge accident rather than a tool limitation. | For each `#[cfg(`-guarded `use` in your sources, confirm the adjacent `#[gazelle::ignore]`; for each `select()`-populated `deps` entry in your committed BUILD files, confirm the `# keep`. **Either half missing is the finding; no platform-conditional dependencies is the pass.** This reads first-party sources and committed BUILD files only — regenerated output is checked by re-running gazelle and diffing. | MUST wherever gazelle_rust is adopted and any dependency is platform-conditional |
| BZL-RUST-19 | Run gazelle_rust behind a `gazelle_test` target you wire yourself — never a bare `gazelle -mode=diff` shell step — and before adopting it confirm the repository needs none of: Rust-protobuf rule generation, dead-target removal when a source file is deleted, `mod`-aware structure inference, `resolve`-into-`proc_macro_deps`, or sources in subdirectories with no per-directory `Cargo.toml`. Otherwise write the drift risk into the onboarding doc and rely on build failures as the only backstop. | The plugin is single-maintainer at its only tag, so semver promises nothing, and its gap list comes from a private tracking issue rather than a roadmap. Its default generation mode is **one target per source file**, not the package grain the Go and Python plugins use — assuming parity across plugins is the mistake. No Gazelle ecosystem ships a `gazelle_test`, its own example included, so the gate's absence is never evidence about the plugin. | `bazel query 'kind(gazelle_test, //...)'`. **A Rust repository with hand-written internal `deps`, no `gazelle_test` and no documented drift note is the finding; either one present is the pass.** Walking the five-item gap list before adoption is a decision to discuss, never a mechanical failure. | SHOULD |

## Protobuf, the IDE Handback and the Repin Job

Three unrelated reads that each happen once per repository: the module list in
`MODULE.bazel`, `git ls-files` over the committed editor config, and the repin
job's `env:` block.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-RUST-20 | Run `bazel run @rules_rust//tools/rust_analyzer:setup -- <editor>` before handing a Rust repository back, commit the generated editor settings, and gitignore the launcher cache directory (`.vscode/.rules_rust_analyzer/`). | Without setup the repository has a working Bazel build and no working IDE; setup is what resolves rust-analyzer, the proc-macro server and rustfmt entirely off the Bazel toolchain with no host Rust install. The settings file is documented as safe to commit — re-runs preserve user keys and comments — while the launcher directory is per-machine regenerated state. | `git ls-files .vscode/settings.json` returns the path (or the onboarding doc carries the Neovim/Helix snippet, which `setup` prints to stdout rather than writing) **and** `git check-ignore .vscode/.rules_rust_analyzer` exits 0. **Either check failing is the finding.** Re-running `setup` after any `MODULE.bazel` edit or a `bazel clean --expunge` is a recurring step named in the contribution docs, not a one-time install. | MUST |
| BZL-RUST-33 | **pinned** — Generate Rust protobuf and gRPC code with `rust_prost_library` on a `proto_library`, add `bazel_dep(name = "rules_rust_prost", version = "<same as rules_rust>")` as its own module, and author a `rust_prost_toolchain` naming this repository's own crate_universe-resolved `prost` and `tonic` versions. Never cite `rust_proto_library`; never keep a `build.rs` running `tonic-build`/`prost-build` as the Bazel path. | At 0.74.0 there is no `rust_proto_library` and no proto directory under `extensions/` — the name survives only as a stale doc link. `rust_prost_library` ships in a **separate BCR module** that `bazel_dep(name = "rules_rust")` does not pull in, and tonic is not a second rule: `tonic_plugin`/`tonic_runtime` sit on the same toolchain and codegen runs through the `protoc-gen-tonic` plugin. A working default toolchain does auto-register, contradicting its own docstring; the reason to override it is type identity — generated code references `tonic::Request<T>` from the toolchain's runtime, so a service crate resolving a different `tonic` through its own graph fails with "expected `tonic::Request`, found `tonic::Request`". No primary source ranks the alternatives; this is a project decision. | `grep -rn -e tonic_build -e prost_build --include=build.rs .` — **any hit in a repository adopting Bazel is a migration item, not a steady state.** Then `grep -n rules_rust_prost MODULE.bazel`: `rust_prost_library` with no such `bazel_dep` does not load (self-verifying), and `rust_prost_library` with no hand-authored `rust_prost_toolchain` is the reviewable finding. On 9.x every underlying `proto_library` needs its own `load("@protobuf//bazel:proto_library.bzl", "proto_library")` — **a missing load is a hard failure on 9.2.0 (the autoload set is empty from 9.0.0) and a silent pass on 8.7.0.** No `.proto` files = N/A. | SHOULD |
| BZL-RUST-36 | Read a `crates_vendor` repin job's environment as if every variable in it were being handed to an arbitrary subprocess, because it is. | The vendor wrapper ends in `exec env -u OUTPUT_BASE "${_BIN}" ...` — full ambient passthrough minus exactly one variable, present unchanged at 0.74.0. It replaced an allowlist that matched no real Cargo variable and so broke private registries and SSH git dependencies outright; the relaxation reads as a pragmatic unblock with no follow-up tightening it, so it is real and unbounded today. This path is `crates_vendor`-only — `crates_repository`'s splice never had the allowlist. | Read the job's `env:` block and the secrets it inherits. **A cache write token, registry credential or cloud key present in that job's environment for any reason other than the vendoring itself is the finding; an environment carrying only what vendoring needs is the pass.** No grep reaches inside the ruleset — the passthrough is unconditional, so the job's environment is the only reachable control. No `crates_vendor` target = N/A. | CONSIDER |

## Gaps

- Every measurement here ran on a Linux host. darwin and Windows sandbox, tag
  and runfiles behaviour is unmeasured, and rules_rust
  [disclaims Windows support](https://github.com/bazelbuild/rules_rust/blob/0.74.0/docs/src/index.md)
  in its maintainers' own words — treat a Windows Rust leg as measured-or-nothing
  and record the disclaimer next to it.
- Rust coverage on a Windows executor, with and without `--enable_runfiles`, has
  never been run; BZL-TEST-24 owns that verification and it is still open.
- No source states a crate-count or line-count threshold at which rust-analyzer
  stops working under Bazel. One practitioner account at ~600k lines is the only
  data point; a number asserted here would be invented.
- An empty `sha256s` on `rust.toolchain()` falls back to the committed
  `known_shas.bzl` table, and a version/target pair absent from that table
  downloads with no integrity check at all. The table's currency against release
  cadence is unmeasured.
- Whether `rust_prost_transform`'s `prost_opts`/`tonic_opts` reproduces a
  `tonic-build` crate's `extern_path`-flattened module layout has not been run
  end to end; it needs a spike, not a source read.

## What Agents Get Wrong Here

1. **Copying `CARGO_BAZEL_REPIN=1 bazel sync --only=<repo>` out of the ruleset's
   own current documentation** — doing the right thing, reading the primary
   source, yields a command deleted in Bazel 9.0.0 (BZL-RUST-03).
2. **Dropping the `lockfile` attribute as "the safe default" when unsure what to
   pass**, which turns off the only freshness gate that exists (BZL-RUST-01).
3. **Inventing a `--crate_lockfile_mode` or `--locked` flag by analogy with
   Bzlmod's `--lockfile_mode=error`**, instead of reading both help surfaces and
   finding the mechanism is an attribute plus an environment variable
   (BZL-RUST-02).
4. **Porting a `crate.annotation()` between WORKSPACE and Bzlmod by
   substitution**, under a comment promising the two are kept in sync
   (BZL-RUST-17).
5. **Assuming a `build.rs` that reads `.git` works under Bazel as it does under
   `cargo build`**, or reading `root_path`'s arrival as a change to the
   build-script sandbox (BZL-RUST-12, BZL-RUST-13).
6. **Treating "whatever `rust.toolchain()` defaults to" as neutral**, or citing
   `extra_target_triples` and `SUPPORTED_PLATFORM_TRIPLES` interchangeably
   because adjacent prose calls both the curated default seven (BZL-RUST-31,
   BZL-RUST-08).
7. **Inventing a per-target `clippy = True` attribute, or assuming one skip tag
   silences both aspects** (BZL-RUST-23).
8. **Assuming `cargo test --doc` happens automatically and that an empty
   `rust_test_suite` glob fails loudly** — both produce green targets that test
   nothing (BZL-RUST-28, BZL-RUST-30).
