# Crates of Record

The one crate this family uses for each job, and the superseded crate a
model reaches for instead. Read this before adding, replacing, or
upgrading a dependency — the failure it prevents is not a bad crate, it is
a *confidently* named dead one.

Every "superseded" entry below was, at some point, the correct answer. A
model trained before the change still emits it with no hedge, and the code
compiles.

Contents: [Selection Rules](#selection-rules) · [The Table](#the-table) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

## Selection Rules

| ID | Rule | Verification | Severity |
|---|---|---|---|
| DEP-01 | Check liveness against the crates.io JSON API (`https://crates.io/api/v1/crates/<name>`), not the rendered page and not recall. The SPA is JavaScript-rendered, so a fetch of the HTML returns a shell and a model summarising it invents the numbers. | Per crate, before adding it, with the crate under consideration in place of the worked example: `jq -r '.crate.updated_at, .crate.max_version' <(curl -sSL -A dep-check https://crates.io/api/v1/crates/oci-client)`. Pasting the bare template instead is a shell syntax error, not a clean run; the API 403s without a `-A` | MUST |
| DEP-02 | Judge liveness by `updated_at` and by what the newest release *contains*, never by download count. Downloads are a lagging popularity metric that stays high for years after abandonment, and a fresh release can be a deprecation notice or a deliberate compile error. | The justification recorded in the PR names a date and a release's content, not a download figure | MUST |
| DEP-03 | Treat a `+deprecated` version suffix, an archived repository, or a crates.io description saying "deprecated in favor of X" as a hard stop. These are the author's own machine-readable signal and they are unambiguous. | `rg -nF -e '-deprecated' -e '+deprecated' Cargo.lock` returns nothing — the lockfile is the resolved graph, and the suffix survives into it | MUST |
| DEP-04 | A new dependency must not duplicate a job an existing dependency already does. Two glob crates, two datetime crates, two decoders for one format, or a second async runtime are correctness hazards, not just weight: their semantics diverge on the edge cases nobody tests. | `cargo tree -e normal -d` — every duplicate is either justified in the PR body or removed | MUST |
| DEP-05 | Before adding a crate, check whether it is already in the graph transitively and reachable. Adding a direct dependency on something a dependency already links is free weight and a second version to keep in step. | Per crate, before `cargo add`, with the crate under consideration in place of the worked example: `cargo tree -e normal -i oci-client`. A `did not match any packages` error is the not-in-the-graph answer; a printed inverted tree means it is already reachable | SHOULD |
| DEP-06 | Every entry in the superseded column of [the table](#the-table) is denied at the manifest level, so the check does not depend on a reviewer noticing. `cargo deny`'s `bans.deny` list is the mechanism. | `cargo deny check bans` in CI; the deny list and the table below do not drift apart | MUST |
| DEP-07 | A crate whose only role is to pin a version for another crate — no `use` site anywhere — is deleted, and the version constraint expressed on the crate that is actually used. A phantom dependency reads as a real one and survives every unused-dependency scan that trusts the manifest. | Two steps, and step 2 is per crate — there is no one-shot form. 1. `cargo shear` lists the manifest-unused crates. 2. For each surviving direct dependency, search its name with hyphens turned to underscores; for `oci-client` that is `rg -l --type rust --glob '!external/**' 'oci_client::' .`. An absence assertion read backwards: empty output is the finding, a file list is the pass. Adjudicate each empty result, because a crate reached only through a `derive` macro has no path use site and is not a phantom | SHOULD |
| DEP-08 | An advisory ignore in `deny.toml` carries a machine-checkable removal condition in a comment — the command that will one day return empty — not a date and not "revisit later". | Every `[advisories] ignore` entry has a comment naming a command; run them in CI and fail when one goes green | SHOULD |

## The Table

The right-hand column is the point: *what changed, and when*. A rule that
only says "use X" loses an argument with a model that has seen ten
thousand lines using Y.

| Job | Crate of record | An agent reaches for | What changed |
|---|---|---|---|
| CLI parsing | `clap` (derive) | `structopt` | Folded into clap 3.0; archived, last release 2022-01 |
| Completions, man pages | `clap_complete`, `clap_mangen` | hand-written completion scripts | Nushell needs `clap_complete_nushell` separately |
| Terminal styling | `console` | `ansi_term`, `colored`, `owo-colors`, `termcolor` | `ansi_term` unmaintained since 2019-09. `anstream`/`anstyle` arrive transitively via `clap_builder` — check before "adding" them |
| Colour resolution | an explicit `NO_COLOR`/`CLICOLOR_FORCE`/`CLICOLOR`/`TERM=dumb` chain, resolved once | assuming the styling crate reads the environment | `console` 0.16 has an independent global switch and reads none of those variables. The application wires them |
| TUI | `ratatui` 0.30 | `tui`, and `Frame<'_, B: Backend>` | `tui` last released 2022-08. `Frame` has carried no backend generic since 0.25 — that signature is the wrong crate, not an old version. 0.30 renamed `Alignment` → `HorizontalAlignment` |
| Progress | `indicatif` | hand-rolled ANSI | `ProgressDrawTarget::stderr()` already hides on a non-TTY and under `TERM=dumb`; no manual gate |
| Progress under logging | `MultiProgress::suspend()` around the log writer | `tracing-indicatif::IndicatifLayer` | The span-attached model was rejected here over a `tracing_subscriber` sharded-registry concurrency failure — a decision, not an oversight |
| Async runtime | `tokio` | `async-std`, `smol` | async-std's own crates.io description reads "Deprecated in favor of `smol`". A second runtime in one binary is never right |
| HTTP client | `reqwest` | hyper 0.14-style `Client`/`Server` | hyper 1.0 (2025-01) removed the high-level client and server most training data assumes |
| TLS | `rustls` with one explicit provider | `native-tls`, the `ring` provider, a reflexive `install_default()` | Providers became pluggable at rustls 0.23 and the default moved `ring` → `aws-lc-rs`. Where the feature graph already resolves one provider, adding `install_default()` is the error |
| Root certificates | bundled roots merged as *extra* roots | replacing the platform verifier to "fix" a corporate proxy | Replacing silently disables `SSL_CERT_FILE`, the OS store, and every corporate MITM path |
| OCI registry | `oci-client` | `oci-distribution`, `dkregistry-rs` | `oci-distribution` was renamed `oci-client` |
| Secrets in memory | `secrecy::SecretString`, `Zeroizing` for intermediates | `String`, and `#[derive(Debug)]` on a credential struct | secrecy's own docs disclaim intermediate heap copies from clones and conversions |
| OS keychain | `keyring-core` 1.0 plus a per-platform backend crate | `keyring = "2"` / `"3"` with feature-selected backends | Split in 2026: `keyring-core` holds the API, `keyring` 4.x is a compat wrapper, backends ship as separate crates |
| JSON | `serde_json` | `simd-json` | Different API, mutates the input buffer; earn it with a measurement |
| Canonical JSON | `serde_json_canonicalizer` | hand-sorting a `serde_json::Map` | Canonicalisation is a spec, not a sort |
| TOML (read) | `toml` | — | `toml::map::Map` is `BTreeMap`-backed unless `preserve_order` is on |
| TOML (write) | `toml_edit` `DocumentMut` | `toml::to_string_pretty` on a domain struct, or `writeln!` templating | Only `toml_edit` preserves comments, spacing and order. Known gaps: dotted-key order, CRLF normalised to LF, BOM dropped |
| YAML | a maintained fork (`serde_yaml_ng` here) | `serde_yaml` | `serde_yaml` carries a literal `+deprecated` suffix since 0.9.34 (2024-03) and is archived. The fork landscape is unsettled — pick one per family and hold it |
| JSON Schema | `schemars` 1.x, with the output pinned by a fixture | trusting the generated shape across upgrades | schemars states verbatim that generated-schema structure may change between versions and that this is not a breaking change |
| Binary serialization | `postcard`, `rmp-serde` | `bincode` | `RUSTSEC-2025-0141`: development permanently ceased. Its 3.0.0 is a docs-only release containing a deliberate compile error — a fresh timestamp meaning the opposite of health |
| Errors | `thiserror` in libraries, `anyhow` at the top | `miette` | Presentation is already owned by the error and exit-code rules; a third layer is churn |
| Diagnostics | `tracing`, `tracing-subscriber`, `tracing-log` | `log` + `env_logger` in new code | Bridge legacy dependencies through `tracing-log` instead of running two loggers |
| Atomic write | `tempfile`'s `NamedTempFile::persist` | the `atomicwrites` crate | Temp-then-rename needs no dedicated crate, and `atomicwrites` has no Windows data flush |
| File locking | `fs4` | `fs2` | `fs2`'s last release is 2018-01; `fs4` forked it onto `rustix` and added async support |
| Windows canonicalization | `dunce` | bare `std::fs::canonicalize` | Avoids leaking a `\\?\` verbatim prefix into displayed and re-joined paths |
| Base directories | one of `dirs` / `directories`, chosen once | picking per call site | Both are maintained by the same org; the failure is inconsistency, not the choice |
| Glob matching | one crate per family (`globset` here) | adding a second | `glob` and `globset` disagree on pattern semantics; two in one workspace is a correctness bug |
| gzip | `flate2` on `miniz_oxide` | `flate2` with a `zlib`/`zlib-ng` feature | Pure Rust as configured. The regression to watch is a transitive default-features union pulling the C backend back in |
| xz | a pure-Rust decoder | `liblzma`, `xz2` | A CVE rationale scoped to the multithreaded decoder does not cover index-parsing advisories, which apply at any thread count |
| tar | `tar` ≥ 0.4.45 | ≤ 0.4.44 | `RUSTSEC-2026-0067`: `unpack` followed symlinks via `fs::metadata()`, permitting a chmod outside the root |
| zip | `zip` 8.x with an explicit extraction loop | `ZipArchive::extract()` | `RUSTSEC-2025-0168` (CVSS 7.3): symlink zip-slip that `enclosed_name()` alone did not stop. The convenience method looks like the obviously right answer |
| Registry digests | `sha2` | `blake3` | OCI digests are contractually `sha256`. `blake3` only for an internal content-addressed layer |
| Date and time | `chrono` | `jiff` | jiff is the direction of travel for calendar arithmetic, not a migration mandate. One datetime crate per graph either way |
| Retry | `backon` | `backoff`, `tokio-retry` | Where the ecosystem converged for sync and async in one API |
| Rate limiting | `governor` | hand-rolled sleeps | Adopt on observed throttling, not pre-emptively |
| Layered config | explicit precedence over `toml`/`toml_edit` | `figment`, `config` | Both are read-and-merge only: no format-preserving write-back at all |
| `.env` | `dotenvy` if any | `dotenv` | `dotenv` unmaintained since 2019-10 |
| Random, UUIDs | `rand` 0.10, `uuid` v7 for anything sortable | `rng.gen()`, reflexive `new_v4()` | `rand` renamed `gen` → `random` and `gen_range` → `random_range` at 0.9; `gen()` is two majors stale |
| HTTP test doubles | `wiremock` | `mockito` | wiremock is async-only and Tokio-native, which is the fit for a fully async suite |
| Test runner | `cargo-nextest` | `cargo test` | Process-per-test isolation, and the only runner these gates are written against |

## What Agents Get Wrong Here

1. **Reads download count as health.** It is the one number visible without
   an API call, and it stays high for years after a crate dies. DEP-02.
2. **`cargo install <short-name>`.** The binary name and the crate name are
   not the same string, and the short one is a squatting target — install
   by the crate name the tool's own docs give.
3. **Adds `install_default()`, `.unwrap()`d, because a panic message
   mentioned it.** The message names the ambiguous-provider case; in a
   graph with one provider the call is unreachable ceremony that will
   panic if a second is ever linked.
4. **"Modernises" a pure-Rust codec to a C-backed one** by enabling a
   feature that reads as a performance win. Check what `flate2`, `liblzma`
   and `zstd-sys` actually link after the change.
5. **Calls the convenience extractor.** `ZipArchive::extract()` and
   `Archive::unpack()` are the shortest correct-looking line in the file
   and both have CVEs against exactly that use.
6. **Upgrades a crate and keeps the phantom pinning sibling.** The version
   requirement now lives in two places and one of them has no call site.
7. **Trusts a fresh release date.** `bincode` 3.0.0 is the counterexample:
   recent, and a deliberate compile error.

## Sources

- [crates.io API](https://crates.io/api/v1/crates) — the only machine-readable liveness source; the web UI is client-rendered
- [RustSec advisory database](https://rustsec.org/advisories/) — `RUSTSEC-2025-0141` (bincode), `RUSTSEC-2025-0168` (zip), `RUSTSEC-2026-0067` (tar)
- [cargo-deny bans](https://embarkstudios.github.io/cargo-deny/checks/bans/index.html) / [cargo-shear](https://github.com/Boshen/cargo-shear) — the two mechanisms behind DEP-06 and DEP-07
- [rustls provider docs](https://docs.rs/rustls/latest/rustls/crypto/struct.CryptoProvider.html) — when `install_default()` is and is not required
- [schemars stability statement](https://graham.cool/schemars/) — generated-schema shape is explicitly not covered by semver
- Full evidence and the per-crate reasoning: [`.agents/research/rust-ecosystem.md`](../../.agents/research/rust-ecosystem.md). That file carries its own 54-rule `ECO-*` set — a **separate namespace** from the `DEP-*` rules above, which is why these were renumbered rather than kept as `ECO-*`. An `ECO-nn` citation always means the research file; a `DEP-nn` citation always means this one.
