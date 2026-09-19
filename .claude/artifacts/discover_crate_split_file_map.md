# Discover: crate-split file → target-crate map

Authoritative file-level assignment for the `ocx_lib` → 17-crate split, derived
from `.claude/artifacts/adr_crate_split_workspace.md` (§ Architecture — the
crate map, § Placement rulings, § API Contract, § Rulings OQ3) and
`.claude/artifacts/system_design_crate_workspace.md` (§ 3.1–3.5), cross-checked
against the tree at `evelynn` HEAD `d8750fd7`. All counts below are script
output (`.tmp/hex/plan-crate-split/scan.py`, `classify.py`, `dep_scope.py`),
never hand-estimated. Scripts and raw JSON kept at
`.tmp/hex/plan-crate-split/` (gitignored, durable for this session).

**Scan method.** `scan.py` walks every `.rs` file under `crates/ocx_lib/src/`,
`crates/ocx_lib/tests/`, `crates/ocx_lib/test/`, counting lines, `#[test]`,
`#[cfg(test)] mod`, `crate::test` sites, `feature = "__testing"` sites and
`schemars`/`JsonSchema` derive sites via plain-text regex (no comment
stripping — see caveat in § 1). `classify.py` assigns each file to a target
crate by top-level module against the ADR's crate-map table, with path-prefix
overrides for the eight files/directories the ADR calls out as intra-file or
intra-directory splits. `dep_scope.py` re-scans each file for
`\b<dep>::` references (catching fully-qualified paths a bare `use`-line scan
misses, e.g. `h2::server::handshake`) and buckets each hit as production or
test-scoped using the position of the file's first `#[cfg(test)] mod
*test*` block as the scope boundary — a heuristic, not a parser; two verified
mispredictions are called out in § 1's footnote.

## 1. Summary table per target crate

Counts include only `crates/ocx_lib/src/**` and `crates/ocx_lib/tests/**` and
`crates/ocx_lib/test/**` files assigned to that crate (§ 2 has the per-file
detail; § 3 has the intra-file splits this table does not subtract). `N/A` is
`lib.rs` itself (dissolves — see § 2 note); `DISSOLVE` is `error.rs` (its
33-variant `Error` enum is deleted per contract E1; its three domain-free
helpers move to `ocx_util`, counted there in § 3, not in this row).

| Crate | Files | LOC | `#[test]` | `cfg(test) mod` | `crate::test` sites (files) | `__testing` sites (files) | `schemars` derive sites | External crates (dev-only marked) |
|---|---|---|---|---|---|---|---|---|
| `ocx_announce` | 23 | 29624 | 229 | 21 | 26 (8) | 10 (3) | 0 | `base64`, `bytes`, `clap_builder`, `futures`, `percent_encoding`, `reqwest`, `serde_json`, `tempfile`, `tokio`, `url` |
| `ocx_cli` | 5 | 1993 | 85 | 2 | 0 (0) | 0 (0) | 0 | `clap_builder`, `tracing_subscriber` |
| `ocx_config` | 17 | 29824 | 514 | 16 | 163 (5) | 5 (3) | 13 | `futures`, `serde`, `tempfile`, `tokio`, `toml_edit` |
| `ocx_console` | 14 | 2496 | 38 | 6 | 0 (0) | 0 (0) | 0 | `clap_builder`, `serde` |
| `ocx_exit` | 2 | 463 | 21 | 2 | 0 (0) | 0 (0) | 0 | `serde` |
| `ocx_index` | 18 | 26655 | 108 | 14 | 7 (2) | 4 (2) | 0 | `async_trait`, `futures`, `serde`, `serde_json`, `tempfile`, `tokio` |
| `ocx_oci` | 42 | 34062 | 463 | 36 | 3 (3) | 10 (3) | 18 | `async_compression`, `async_trait`, `base64`, `bytes`, `docker_credential`, `futures`, `hyper_util`, `oci_client`, `reqwest`, `secrecy`, `serde`, `serde_repr`, `sha2`, `tokio`, `tokio_util`, `url` |
| `ocx_package` | 52 | 31165 | 735 | 42 | 0 (0) | 2 (2) | 69 | `async_trait`, `chrono`, `clap_builder`, `futures`, `regex`, `serde`, `serde_repr`, `tempfile` |
| `ocx_package_manager` | 61 | 70095 | 516 | 57 | 42 (5) | 13 (2) | 25 | `async_trait`, `chrono`, `futures`, `p256`, `packageurl`, `serde`, `serde_json`, `serde_repr`, `tempfile`, `tokio`, `tracing`, `url`, `zeroize` |
| `ocx_project` | 22 | 26499 | 371 | 27 | 40 (5) | 0 (0) | 19 | `async_trait`, `clap_builder`, `regex`, `serde`, `serde_repr`, `sha2`, `tempfile`, `tokio`, `toml_edit` |
| `ocx_script` | 12 | 3175 | 64 | 9 | 0 (0) | 0 (0) | 0 | `allocative`, `starlark`, `tokio` |
| `ocx_setup` | 12 | 12270 | 193 | 11 | 19 (2) | 0 (0) | 0 | `regex`, `sha2`, `toml_edit`, `windows_sys` |
| `ocx_shell` | 15 | 13899 | 354 | 15 | 56 (7) | 3 (1) | 9 | `base64`, `clap_builder`, `indexmap`, `serde`, `sha2` |
| `ocx_sign` | 40 | 34312 | 313 | 34 | 4 (2) | 0 (0) | 3 | `async_trait`, `base64`, `chrono`, `clap_builder`, `p256`, `pki_types`, `reqwest`, `serde`, `serde_json`, `sha2`, `sigstore`, `sigstore_protobuf_specs`, `thiserror`, `tokio`, `url`, `x509_cert`, `zeroize` |
| `ocx_store` | 21 | 16328 | 222 | 17 | 3 (2) | 5 (1) | 0 | `serde`, `sha2`, `tempfile`, `tokio`, `windows_sys` |
| `ocx_test_support` | 5 | 331 | 0 | 0 | 0 (0) | 0 (0) | 0 | `async_trait`, `tempfile` |
| `ocx_trust` | 1 | 3466 | 83 | 1 | 2 (1) | 0 (0) | 9 | `serde` |
| `ocx_util` | 33 | 12389 | 164 | 20 | 2 (2) | 0 (0) | 3 | `clap_builder`, `futures`, `p256`, `regex`, `tempfile`, `tokio`, `tokio_rustls`†, `windows_sys`, `x509_cert`, `zip` |
| DISSOLVE | 1 | 525 | 5 | 1 | 0 (0) | 0 (0) | 0 | — |
| N/A | 1 | 90 | 0 | 0 | 0 (0) | 0 (0) | 0 | — |

**Caveats on the dependency column, stated plainly (Verification Honesty):**

- The scan does not strip comments. Spot-checked and confirmed a false
  positive: `ocx_project`'s `anyhow` hit is `project/compose.rs:495`, inside a
  `///` doc comment (`` `.map_err(anyhow::Error::from)` ``), not real code —
  `anyhow` is not a real dependency of `ocx_project`.
- The scan's prod/test scope boundary (first `#[cfg(test)] mod *test*` in the
  file) mispredicts when a file gates a helper with a standalone
  `#[cfg(test)]` *before* that trailing module (the pattern
  `quality-rust.md` calls "scattered `#[cfg(test)]`", which it flags as a
  smell for this exact reason). Confirmed case: `ocx_oci`'s `h2` hit
  (`oci/transport_policy.rs:1023`, `h2::server::handshake`) lands in the
  script's PROD bucket, but the root `Cargo.toml` comment on `h2` states
  plainly it is "Test-only (ocx_lib `[dev-dependencies]`)" for exactly this
  call site — the retry classifier's h2 branch has no other reachable red
  state. Treat `h2` as dev-only for `ocx_oci`, not the PROD listing the script
  produced.
- Given these two confirmed mispredictions, treat every other TEST-ONLY /
  PROD split in the raw `dep_scope.py` output (`.tmp/hex/plan-crate-split/`)
  as indicative, not authoritative, until re-verified per crate at
  implementation time. The counts in the table above (files, LOC, `#[test]`,
  etc.) are exact — they come from `scan.py`'s direct regex counts, not the
  scope heuristic.
- `ocx_lib`'s `[dev-dependencies]` today are exactly `anyhow`, `tokio`
  (`test-util` feature only), `h2`, `tokio-rustls` — see § 4. Any crate whose
  aggregate above lists one of these should treat it as dev-only unless a
  specific file is shown to need it at runtime.

## 2. Full assignment table

Every `.rs` file under `crates/ocx_lib/src/`, `crates/ocx_lib/tests/`,
`crates/ocx_lib/test/` — 397 rows, no omissions. Sorted by target crate then
path. "Note" cites the ADR section backing the assignment; `[assumed]` marks
the six files/groups the ADR does not name explicitly (reasoning inline).
`[SPLIT-FILE]` marks a file whose *default* row here is its majority
destination — § 3 gives the item-level detail and the LOC that actually moves
elsewhere.

| Path | LOC | Target crate | Note |
|---|---|---|---|
| `crates/ocx_lib/src/announce.rs` | 2168 | `ocx_announce` | ADR crate map: top-level module 'announce' |
| `crates/ocx_lib/src/announce/error.rs` | 545 | `ocx_announce` | ADR crate map: top-level module 'announce' |
| `crates/ocx_lib/src/announce/pipeline.rs` | 2200 | `ocx_announce` | ADR crate map: top-level module 'announce' |
| `crates/ocx_lib/src/announce/request.rs` | 163 | `ocx_announce` | ADR crate map: top-level module 'announce' |
| `crates/ocx_lib/src/claim.rs` | 1627 | `ocx_announce` | ADR crate map: top-level module 'claim' |
| `crates/ocx_lib/src/claim/error.rs` | 495 | `ocx_announce` | ADR crate map: top-level module 'claim' |
| `crates/ocx_lib/src/claim/owners.rs` | 1445 | `ocx_announce` | ADR crate map: top-level module 'claim' |
| `crates/ocx_lib/src/claim/request.rs` | 450 | `ocx_announce` | ADR crate map: top-level module 'claim' |
| `crates/ocx_lib/src/claim/root.rs` | 498 | `ocx_announce` | ADR crate map: top-level module 'claim' |
| `crates/ocx_lib/src/forge.rs` | 465 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/api.rs` | 842 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/credentials.rs` | 1209 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/error.rs` | 788 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/git_command.rs` | 1799 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/git_push_options.rs` | 643 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/git_stderr.rs` | 1403 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/git_workspace.rs` | 3731 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/github.rs` | 2400 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/gitlab.rs` | 5512 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/http.rs` | 123 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/identity.rs` | 279 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/kind.rs` | 695 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/forge/poll.rs` | 144 | `ocx_announce` | ADR crate map: top-level module 'forge' |
| `crates/ocx_lib/src/cli/clap.rs` | 47 | `ocx_cli` | [assumed] clap_builder Command::try_get_matches dispatch + process exit is CLI-entry-point behavior, not presentation vocabulary; clap_builder absent from ocx_console's declared dependency set (ADR X3) |
| `crates/ocx_lib/src/cli/classify.rs` | 1502 | `ocx_cli` | ADR API Contract: classification ladder -> crates/ocx_cli/src/exit/ |
| `crates/ocx_lib/src/cli/error.rs` | 197 | `ocx_cli` | [assumed] UsageError's own doc comment ties its placement to classify_error's location, which moves to ocx_cli; CLI-input-validation domain, not library domain |
| `crates/ocx_lib/src/cli/log_level.rs` | 53 | `ocx_cli` | ADR Placement rulings: subscriber-adjacent log_level -> ocx_cli |
| `crates/ocx_lib/src/cli/log_settings.rs` | 194 | `ocx_cli` | ADR Placement rulings: subscriber-adjacent log_settings -> ocx_cli |
| `crates/ocx_lib/src/config.rs` | 4018 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/edit.rs` | 509 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/error.rs` | 190 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/home.rs` | 78 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/index.rs` | 14 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/insecure.rs` | 478 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/loader.rs` | 7067 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/managed.rs` | 1391 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/managed_config.rs` | 13 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/managed_config/paths.rs` | 192 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/mirror.rs` | 2124 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/patch.rs` | 1283 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/records.rs` | 469 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/registry.rs` | 442 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/shell.rs` | 1927 | `ocx_config` | ADR crate map: top-level module 'config' |
| `crates/ocx_lib/src/config/tls.rs` | 1445 | `ocx_config` | WP-19 (C-043): the configured half of tls.rs — origin, refusal, ladder |
| `crates/ocx_lib/src/env.rs` | 6568 | `ocx_config` | [SPLIT-FILE default] see Split detail: item-level split, package-aware items -> ocx_package_manager |
| `crates/ocx_lib/src/managed_config.rs` | 61 | `ocx_config` | ADR crate map: top-level module 'managed_config' |
| `crates/ocx_lib/src/managed_config/pause.rs` | 229 | `ocx_config` | ADR crate map: top-level module 'managed_config' |
| `crates/ocx_lib/src/managed_config/persistence.rs` | 1392 | `ocx_config` | ADR crate map: top-level module 'managed_config' |
| `crates/ocx_lib/src/managed_config/test_support.rs` | 177 | `ocx_config` | ADR crate map: top-level module 'managed_config' |
| `crates/ocx_lib/src/cli.rs` | 43 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/data_interface.rs` | 503 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/human.rs` | 104 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/options.rs` | 10 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/options/color_mode.rs` | 183 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/options/progress_mode.rs` | 20 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/printer.rs` | 393 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/progress.rs` | 560 | `ocx_console` | [SPLIT-FILE] LogWriter/LogWriterHandle/impl MakeWriter (subscriber wiring, L347-391) -> ocx_cli; rest (ProgressManager etc.) -> ocx_console |
| `crates/ocx_lib/src/cli/styles.rs` | 23 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/theme.rs` | 450 | `ocx_console` | [SPLIT-FILE] impl StyledInk for Digest/Identifier (L279-296) DELETED per phase 1.2; rest -> ocx_console |
| `crates/ocx_lib/src/cli/theme/colorful.rs` | 30 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/theme/mono.rs` | 32 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/cli/user_interface.rs` | 141 | `ocx_console` | ADR crate map: cli minus classification, minus subscriber setup, minus ocx_exit's two files |
| `crates/ocx_lib/src/log.rs` | 4 | `ocx_console` | ADR crate map: top-level module 'log' |
| `crates/ocx_lib/src/cli/error_category.rs` | 217 | `ocx_exit` | ADR Rulings OQ3: ErrorCategory -> ocx_exit |
| `crates/ocx_lib/src/cli/exit_code.rs` | 246 | `ocx_exit` | ADR Rulings OQ3: ExitCode -> ocx_exit |
| `crates/ocx_lib/src/oci/index.rs` | 2072 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/chained_index.rs` | 6485 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/error.rs` | 579 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/file_transport.rs` | 1039 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/index_impl.rs` | 339 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/local_index.rs` | 4681 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/local_index/config.rs` | 14 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/oci_index.rs` | 294 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/oci_index/cache.rs` | 137 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/oci_index/config.rs` | 9 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/ocx_index.rs` | 5238 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/regenerate.rs` | 1020 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/store.rs` | 3041 | `ocx_index` | ADR Placement rulings: oci/index + the index store become one crate (WP-11: one directory too) |
| `crates/ocx_lib/src/oci/index/wire.rs` | 623 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/src/oci/index/wire_writer.rs` | 449 | `ocx_index` | ADR Placement rulings: oci/index -> ocx_index |
| `crates/ocx_lib/tests/dispatch_conformance.rs` | 207 | `ocx_index` | ADR: integration tests move with subject |
| `crates/ocx_lib/tests/index_wire_conformance.rs` | 291 | `ocx_index` | ADR: integration tests move with subject |
| `crates/ocx_lib/tests/live_index_wire.rs` | 104 | `ocx_index` | ADR: integration tests move with subject |
| `crates/ocx_lib/src/auth.rs` | 204 | `ocx_oci` | ADR crate map: top-level module 'auth' |
| `crates/ocx_lib/src/auth/auth_type.rs` | 40 | `ocx_oci` | ADR crate map: top-level module 'auth' |
| `crates/ocx_lib/src/auth/error.rs` | 210 | `ocx_oci` | ADR crate map: top-level module 'auth' |
| `crates/ocx_lib/src/auth/login.rs` | 507 | `ocx_oci` | ADR crate map: top-level module 'auth' |
| `crates/ocx_lib/src/auth/registry_url.rs` | 99 | `ocx_oci` | ADR crate map: top-level module 'auth' |
| `crates/ocx_lib/src/auth/store.rs` | 923 | `ocx_oci` | ADR crate map: top-level module 'auth' |
| `crates/ocx_lib/src/media_type.rs` | 91 | `ocx_oci` | ADR crate map: top-level module 'media_type' |
| `crates/ocx_lib/src/oci.rs` | 125 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/annotations.rs` | 25 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client.rs` | 8249 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client/builder.rs` | 1158 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client/error.rs` | 505 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client/hashing_reader.rs` | 387 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client/mirror_map.rs` | 252 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client/native_transport.rs` | 1860 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client/progress_reader.rs` | 197 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client/test_transport.rs` | 709 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/client/transport.rs` | 1925 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/copy.rs` | 2022 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/digest.rs` | 529 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/digest/error.rs` | 22 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/endpoint.rs` | 1516 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/file_storage.rs` | 4 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/host_capabilities.rs` | 2451 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/identifier.rs` | 1229 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/identifier/error.rs` | 62 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/layer_layout.rs` | 287 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/layer_ref.rs` | 916 | `ocx_oci` | ADR 1.5 (WP-14b): moved from `publisher/layer_ref.rs` — the layer vocabulary belongs beside the client that consumes it; `publisher.rs` keeps a phase-1 `pub use` (C-040), hard-cut by WP-30 |
| `crates/ocx_lib/src/oci/manifest.rs` | 281 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/manifest_builder.rs` | 473 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/pinned_identifier.rs` | 311 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/platform.rs` | 2615 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/platform/architecture.rs` | 280 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/platform/error.rs` | 50 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/platform/operating_system.rs` | 329 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/referrer.rs` | 19 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/referrer/capability.rs` | 762 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/referrer/discovery.rs` | 83 | `ocx_oci` | ADR 1.18: `DiscoveryMethod` is populated by the client's referrer listing; verify only reports it |
| `crates/ocx_lib/src/oci/referrer/manifest.rs` | 379 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/referrer/media_types.rs` | 191 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/repository.rs` | 178 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/resolve_target.rs` | 289 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/ssrf.rs` | 1166 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/oci/tag.rs` | 430 | `ocx_oci` | ADR 1.6 / A.4: the registry-wire tag vocabulary (`__ocx` namespace, legacy keep form, Referrers fallback + cosign sidecar tags, `is_reserved_tag`) |
| `crates/ocx_lib/src/oci/transport_policy.rs` | 1151 | `ocx_oci` | ADR crate map: generic oci sub-modules (12 of 13) + identifier/platform/client/copy/host_capabilities/layer_layout/native/auth/media_type |
| `crates/ocx_lib/src/package.rs` | 19 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/bin_scan.rs` | 1174 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/bundle.rs` | 298 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/cascade.rs` | 1863 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/cascade/apply.rs` | 1108 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/cascade/equivalence.rs` | 710 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/cascade/gather.rs` | 890 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/cascade/graph.rs` | 898 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/cascade/graph/tests.rs` | 1952 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/dependency_pinning.rs` | 777 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/description.rs` | 382 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/description/transport.rs` | 361 | `ocx_package` | ADR 1.5 (WP-14b): the `__ocx.desc` wire shape, moved off `Client` as free functions over its blob/manifest primitives (C-040) |
| `crates/ocx_lib/src/package/error.rs` | 217 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/info.rs` | 161 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/install_info.rs` | 218 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/install_status.rs` | 80 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/libc_lint.rs` | 1569 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata.rs` | 98 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/authoring.rs` | 491 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/authoring/dependency.rs` | 274 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/binary.rs` | 553 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/bundle.rs` | 174 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/dependency.rs` | 761 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/entrypoint.rs` | 718 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env.rs` | 158 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/apply.rs` | 1620 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/conflict.rs` | 95 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/constant.rs` | 24 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/dep_context.rs` | 137 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/entry.rs` | 24 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/list.rs` | 123 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/modifier.rs` | 375 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/path.rs` | 23 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/resolver.rs` | 875 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/env/var.rs` | 190 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/integrations.rs` | 940 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/slug.rs` | 53 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/template.rs` | 1384 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/template/error.rs` | 355 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/template/gate.rs` | 115 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/template/render.rs` | 213 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/template/scanner.rs` | 1394 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/template/scope.rs` | 173 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/validation.rs` | 1990 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/metadata/visibility.rs` | 480 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/resolved_package.rs` | 755 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/tag.rs` | 886 | `ocx_package` | [SPLIT-FILE] Tag/InternalTag stay; referrer/sidecar helpers -> ocx_oci (see Split detail) |
| `crates/ocx_lib/src/package/version.rs` | 1048 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/package/version/build_meta.rs` | 107 | `ocx_package` | ADR crate map: package (minus tag helpers) |
| `crates/ocx_lib/src/publisher.rs` | 849 | `ocx_package` | ADR crate map: top-level module 'publisher' |
| `crates/ocx_lib/src/publisher/copy.rs` | 1617 | `ocx_package` | ADR crate map: top-level module 'publisher' |
| `crates/ocx_lib/src/publisher/publish_gate.rs` | 539 | `ocx_package` | ADR crate map: top-level module 'publisher' |
| `crates/ocx_lib/tests/tag_verdicts.rs` | 80 | `ocx_package` | ADR: exercises package::tag::Tag::is_reserved |
| `crates/ocx_lib/src/launch.rs` | 1203 | `ocx_package_manager` | ADR crate map: top-level module 'launch' |
| `crates/ocx_lib/src/launch/child_process.rs` | 377 | `ocx_package_manager` | ADR crate map: top-level module 'launch' |
| `crates/ocx_lib/src/package_manager.rs` | 729 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/activation.rs` | 4838 | `ocx_package_manager` | WP-11 (inversion 1.16): moved under package_manager/, whose sequencing it is |
| `crates/ocx_lib/src/package_manager/composer.rs` | 8508 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/concurrency.rs` | 104 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/error.rs` | 641 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/launcher.rs` | 49 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/launcher/body.rs` | 2049 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/launcher/env_tests.rs` | 160 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/launcher/generate.rs` | 976 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/launcher/project_selector_tests.rs` | 131 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/launcher/safety.rs` | 163 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/managed_config.rs` | 34 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/managed_config/preview.rs` | 241 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/managed_config/publish.rs` | 1636 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/mutate.rs` | 1048 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks.rs` | 47 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/attest.rs` | 407 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/auto_verify.rs` | 648 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/clean.rs` | 1565 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/common.rs` | 2352 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/deselect.rs` | 74 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/find.rs` | 264 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/find_or_install.rs` | 133 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/find_symlink.rs` | 268 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/garbage_collection.rs` | 660 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/garbage_collection/project_roots.rs` | 42 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/garbage_collection/reachability_graph.rs` | 1575 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/inspect.rs` | 3015 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/install.rs` | 396 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/layer_staging.rs` | 132 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/lazy_advisory.rs` | 724 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/managed_config.rs` | 789 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/materialize_lazy.rs` | 398 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/patch_discovery.rs` | 2698 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/patch_publish.rs` | 162 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/patch_sync.rs` | 2185 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/patch_test.rs` | 829 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/prepare_lazy.rs` | 1119 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/pull.rs` | 1506 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/pull_local.rs` | 1314 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/purge.rs` | 35 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/render_toolchain.rs` | 9678 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/resolve.rs` | 8197 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/sbom.rs` | 703 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/select.rs` | 54 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/resolve_subject.rs` | 262 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/sign.rs` | 625 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/toolchain_names.rs` | 1409 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/uninstall.rs` | 207 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/update_check.rs` | 1615 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/tasks/verify.rs` | 362 | `ocx_package_manager` | ADR crate map: top-level module 'package_manager' |
| `crates/ocx_lib/src/package_manager/test_support.rs` | 4 | `ocx_package_manager` | WP-12: `FakeManifestSource`'s `#[cfg(test)]` home, in-crate by D-063 |
| `crates/ocx_lib/src/package_manager/test_support/manifest_source.rs` | 180 | `ocx_package_manager` | WP-12: was `crates/ocx_lib/test/manifest_source.rs` (D-063) |
| `crates/ocx_lib/src/patch.rs` | 47 | `ocx_package_manager` | ADR crate map: top-level module 'patch' |
| `crates/ocx_lib/src/patch/descriptor.rs` | 842 | `ocx_package_manager` | ADR crate map: top-level module 'patch' |
| `crates/ocx_lib/src/patch/error.rs` | 202 | `ocx_package_manager` | ADR crate map: top-level module 'patch' |
| `crates/ocx_lib/src/patch/matcher.rs` | 354 | `ocx_package_manager` | ADR crate map: top-level module 'patch' |
| `crates/ocx_lib/src/patch/persistence.rs` | 447 | `ocx_package_manager` | ADR crate map: top-level module 'patch' |
| `crates/ocx_lib/src/patch/snapshot.rs` | 559 | `ocx_package_manager` | ADR crate map: top-level module 'patch' |
| `crates/ocx_lib/src/record.rs` | 36 | `ocx_package_manager` | ADR crate map: top-level module 'record' |
| `crates/ocx_lib/src/record/environment.rs` | 300 | `ocx_package_manager` | ADR crate map: top-level module 'record' |
| `crates/ocx_lib/src/record/error.rs` | 107 | `ocx_package_manager` | ADR crate map: top-level module 'record' |
| `crates/ocx_lib/src/record/execution_record.rs` | 2793 | `ocx_package_manager` | ADR crate map: top-level module 'record' |
| `crates/ocx_lib/src/record/name_template.rs` | 529 | `ocx_package_manager` | ADR crate map: top-level module 'record' |
| `crates/ocx_lib/src/record/policy.rs` | 618 | `ocx_package_manager` | ADR crate map: top-level module 'record' |
| `crates/ocx_lib/src/record/purl.rs` | 273 | `ocx_package_manager` | ADR crate map: top-level module 'record' |
| `crates/ocx_lib/src/record/sink.rs` | 706 | `ocx_package_manager` | ADR crate map: top-level module 'record' |
| `crates/ocx_lib/src/activate.rs` | 775 | `ocx_project` | ADR crate map: top-level module 'activate' |
| `crates/ocx_lib/src/ladder.rs` | 263 | `ocx_project` | ADR crate map: top-level module 'ladder' |
| `crates/ocx_lib/src/lazy.rs` | 874 | `ocx_project` | ADR crate map: top-level module 'lazy' |
| `crates/ocx_lib/src/project.rs` | 62 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/compose.rs` | 1024 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/config.rs` | 3430 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/consent.rs` | 2675 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/document.rs` | 685 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/env.rs` | 906 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/error.rs` | 925 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/hash.rs` | 396 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/hook.rs` | 229 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/internal.rs` | 31 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/lock.rs` | 2072 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/mutate.rs` | 1678 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/mutation.rs` | 612 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/project_lock.rs` | 398 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/registry.rs` | 1362 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/registry/error.rs` | 95 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/resolve.rs` | 2781 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/project/toolchain_home.rs` | 376 | `ocx_project` | ADR crate map: top-level module 'project' |
| `crates/ocx_lib/src/script.rs` | 379 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/arch_value.rs` | 133 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/engine.rs` | 434 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/expect_module.rs` | 213 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/guard.rs` | 279 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/host.rs` | 163 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/ocx_module.rs` | 836 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/os_value.rs` | 155 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/platform_value.rs` | 198 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/run_result.rs` | 229 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/sl_error.rs` | 36 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/script/variant_parity.rs` | 120 | `ocx_script` | ADR crate map: top-level module 'script' |
| `crates/ocx_lib/src/setup.rs` | 3085 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/bootstrap.rs` | 551 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/error.rs` | 205 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/profiles.rs` | 577 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/rc_block.rs` | 1057 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/session_path.rs` | 1059 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/session_path/linux.rs` | 1003 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/session_path/macos.rs` | 1252 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/session_path/windows.rs` | 931 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/shell_config.rs` | 560 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/shims.rs` | 1358 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/setup/version_spec.rs` | 632 | `ocx_setup` | ADR crate map: top-level module 'setup' |
| `crates/ocx_lib/src/ci.rs` | 291 | `ocx_shell` | ADR crate map: top-level module 'ci' |
| `crates/ocx_lib/src/ci/annotations.rs` | 340 | `ocx_shell` | ADR crate map: top-level module 'ci' |
| `crates/ocx_lib/src/ci/error.rs` | 34 | `ocx_shell` | ADR crate map: top-level module 'ci' |
| `crates/ocx_lib/src/ci/flavor.rs` | 37 | `ocx_shell` | ADR crate map: top-level module 'ci' |
| `crates/ocx_lib/src/ci/github_flavor.rs` | 714 | `ocx_shell` | ADR crate map: top-level module 'ci' |
| `crates/ocx_lib/src/ci/gitlab_flavor.rs` | 598 | `ocx_shell` | ADR crate map: top-level module 'ci' |
| `crates/ocx_lib/src/shell.rs` | 3692 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/coexistence.rs` | 284 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/error.rs` | 11 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/escape.rs` | 146 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/hook.rs` | 3127 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/reconcile.rs` | 132 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/reconcile/fingerprint.rs` | 725 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/reconcile/ledger.rs` | 1054 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/reconcile/plan.rs` | 2714 | `ocx_shell` | ADR crate map: top-level module 'shell' |
| `crates/ocx_lib/src/shell/tests_path_parity.rs` | 118 | `ocx_shell` | WP-17 (inversion 1.7): the PATH-element parity tests, on the upper half's side of the `ocx_util` edge (D-013) |
| `crates/ocx_lib/src/oci/attest.rs` | 137 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/attest/dsse.rs` | 405 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/attest/pipeline.rs` | 2101 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/attest/predicate.rs` | 581 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/attest/statement.rs` | 570 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign.rs` | 71 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/bundle.rs` | 757 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/error.rs` | 940 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/format.rs` | 145 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/fulcio.rs` | 395 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/key_backend.rs` | 758 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/key_signer.rs` | 384 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/oidc.rs` | 230 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/oidc_ambient.rs` | 41 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/oidc_ambient_inline.rs` | 373 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/oidc_browser.rs` | 48 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/pipeline.rs` | 2253 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/state.rs` | 149 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/referrers.rs` | 126 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/rekor.rs` | 478 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/signer.rs` | 553 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/sign/simplesigning_write.rs` | 895 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/simplesigning.rs` | 253 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify.rs` | 77 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/attestation_sidecar.rs` | 2210 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/candidates.rs` | 398 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/candidates/tests.rs` | 721 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/dsse.rs` | 899 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/error.rs` | 1665 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/identity.rs` | 429 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/pipeline.rs` | 10131 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/signing_instant.rs` | 166 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/simplesigning_read.rs` | 1853 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/tlog.rs` | 426 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/trust_cache.rs` | 407 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/trust_resolve.rs` | 490 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/oci/verify/trust_root.rs` | 714 | `ocx_sign` | ADR crate map: oci/{sign,attest,verify}, oci/simplesigning -> ocx_sign |
| `crates/ocx_lib/src/sbom.rs` | 74 | `ocx_sign` | ADR crate map: top-level module 'sbom' |
| `crates/ocx_lib/src/sbom/cyclonedx.rs` | 340 | `ocx_sign` | ADR crate map: top-level module 'sbom' |
| `crates/ocx_lib/src/codesign.rs` | 610 | `ocx_store` | ADR crate map: top-level module 'codesign' |
| `crates/ocx_lib/src/file_structure.rs` | 400 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/assemble.rs` | 3822 | `ocx_store` | ADR Placement rulings: the assembler is install materialisation -> ocx_store (WP-11: moved beside the stores) |
| `crates/ocx_lib/src/file_structure/blob_store.rs` | 677 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/cas_path.rs` | 384 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/error.rs` | 77 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/layer_store.rs` | 253 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/package_store.rs` | 983 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/shim_bin_store.rs` | 725 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/shim_store.rs` | 556 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/state_store.rs` | 1747 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/symlink_store.rs` | 214 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/temp_store.rs` | 537 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/temp_store/acquire_result.rs` | 36 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/temp_store/stale_entry.rs` | 22 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/temp_store/temp_dir.rs` | 39 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/file_structure/toolchain_store.rs` | 1933 | `ocx_store` | ADR crate map: file_structure minus index_store |
| `crates/ocx_lib/src/hardlink.rs` | 472 | `ocx_store` | ADR crate map: top-level module 'hardlink' |
| `crates/ocx_lib/src/reference_manager.rs` | 1123 | `ocx_store` | ADR crate map: top-level module 'reference_manager' |
| `crates/ocx_lib/src/shim.rs` | 630 | `ocx_store` | ADR crate map: top-level module 'shim' |
| `crates/ocx_lib/test/data.rs` | 16 | `ocx_test_support` | whole test/ dir -> ocx_test_support (ADR: data.rs, env.rs, manifest_source.rs named; mod.rs/fifo.rs [assumed] aggregator+fixture) |
| `crates/ocx_lib/test/env.rs` | 96 | `ocx_test_support` | whole test/ dir -> ocx_test_support (ADR: data.rs, env.rs, manifest_source.rs named; mod.rs/fifo.rs [assumed] aggregator+fixture) |
| `crates/ocx_lib/test/fifo.rs` | 35 | `ocx_test_support` | whole test/ dir -> ocx_test_support (ADR: data.rs, env.rs, manifest_source.rs named; mod.rs/fifo.rs [assumed] aggregator+fixture) |
| `crates/ocx_lib/test/manifest_source.rs` | 176 | `ocx_test_support` | whole test/ dir -> ocx_test_support (ADR: data.rs, env.rs, manifest_source.rs named; mod.rs/fifo.rs [assumed] aggregator+fixture) |
| `crates/ocx_lib/test/mod.rs` | 8 | `ocx_test_support` | whole test/ dir -> ocx_test_support (ADR: data.rs, env.rs, manifest_source.rs named; mod.rs/fifo.rs [assumed] aggregator+fixture) |
| `crates/ocx_lib/src/trust.rs` | 3466 | `ocx_trust` | ADR crate map: top-level module 'trust' |
| `crates/ocx_lib/src/trust/key_ref.rs` | 735 | `ocx_trust` | A.4 map correction (D-024/C-044): the `--key` grammar is signer-identity vocabulary, shared by `signers` and the flag |
| `crates/ocx_lib/src/archive.rs` | 722 | `ocx_util` | ADR crate map: top-level module 'archive' |
| `crates/ocx_lib/src/archive/backend.rs` | 15 | `ocx_util` | ADR crate map: top-level module 'archive' |
| `crates/ocx_lib/src/archive/error.rs` | 75 | `ocx_util` | ADR crate map: top-level module 'archive' |
| `crates/ocx_lib/src/archive/extract_options.rs` | 10 | `ocx_util` | ADR crate map: top-level module 'archive' |
| `crates/ocx_lib/src/archive/tar.rs` | 1341 | `ocx_util` | ADR crate map: top-level module 'archive' |
| `crates/ocx_lib/src/archive/zip.rs` | 1049 | `ocx_util` | ADR crate map: top-level module 'archive' |
| `crates/ocx_lib/src/compression.rs` | 566 | `ocx_util` | ADR crate map: top-level module 'compression' |
| `crates/ocx_lib/src/compression/error.rs` | 54 | `ocx_util` | ADR crate map: top-level module 'compression' |
| `crates/ocx_lib/src/tls.rs` | 2047 | `ocx_util` | ADR crate map: top-level module 'tls' |
| `crates/ocx_lib/src/utility.rs` | 15 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/boolean_string.rs` | 103 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/child_process.rs` | 135 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/env.rs` | 267 | `ocx_util` | WP-12: the env accessor, lifted out of `env.rs` (C-026) |
| `crates/ocx_lib/src/utility/error.rs` | 106 | `ocx_util` | WP-17 (inversion 1.7): the two local error types `utility` raises instead of reaching the wide `Error` (C-042, D-042, DEC-11) |
| `crates/ocx_lib/src/utility/fs.rs` | 723 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/bounded_read.rs` | 267 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/dir_walker.rs` | 630 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/drop_file.rs` | 47 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/empty_or_absent.rs` | 136 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/file_lock.rs` | 200 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/locked_file.rs` | 848 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/path.rs` | 791 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/same_dir.rs` | 185 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/same_filesystem.rs` | 167 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/scoped_lock.rs` | 358 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/fs/symlink.rs` | 1091 | `ocx_util` | WP-11 (inversion 1.7): moved under utility/fs/, so a caller reaching for a link primitive no longer reaches ocx_store |
| `crates/ocx_lib/src/utility/fs/symlink_walk.rs` | 257 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/list.rs` | 183 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/path.rs` | 678 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/result_ext.rs` | 12 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/schema.rs` | 34 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/serde_ext.rs` | 42 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/singleflight.rs` | 545 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/string_ext.rs` | 63 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/tls.rs` | 30 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/utility/vec_ext.rs` | 61 | `ocx_util` | ADR crate map: utility minus fs/assemble.rs |
| `crates/ocx_lib/src/error.rs` | 525 | `DISSOLVE` | ADR crate map: top-level module 'error' |
| `crates/ocx_lib/src/lib.rs` | 90 | `N/A` | crate root — dissolves into per-crate lib.rs roots; not a movable file |
## 3. Split files detail

Item-level (function/type/line-range) split for every file the ADR names as
crossing a crate boundary internally, plus the two DAG conflicts the item-level
read surfaced (carried forward to § 8, not silently resolved here).

### 3.1 `env.rs` (6,568 lines — single file, no `env/` subdirectory exists)

The ADR text ("vocabulary/accessor/validator → `ocx_config`; package-aware
composition → `ocx_package_manager` beside `composer.rs`") is a two-way split
inside one file. Item boundaries (next item's start line − 1 = this item's end
line):

| Lines | Item | Target | Why |
|---|---|---|---|
| 14–425 | `pub mod keys { ... }` (env-var key vocabulary) | `ocx_config` | pure vocabulary |
| 426–580 | `struct OcxConfigView` + `impl OcxConfigView` (541) | `ocx_config` | config accessor view |
| 581–588 | `struct ChildEnv<'a>` | `ocx_package_manager` | fields hold `&[crate::package::metadata::env::entry::Entry]` (line 583, 585) — package-aware |
| 589, 592 | `PATH_SEPARATOR` consts (cfg-gated) | `ocx_config` | platform vocabulary |
| 605–632 | `impl EnvKey` | `ocx_config` | key-comparison primitive, no package reach |
| 633–1627 | `struct Env` + `impl Default` (647) + `impl Env` (653) | **mixed — see below** | the ~975-line `impl Env` block is mostly package-oblivious (`new`, `clean`, `set`, `get`, `add_path`, `add_list`, `resolve_command`, `lookup_path`, `apply_ocx_config`, …) but contains two package-aware methods |
| 1035–1039 | `fn set_forwarded_env` (inside `impl Env`) | `ocx_package_manager` | takes `&[crate::package::metadata::env::entry::Entry]` |
| 1058–~1088 | `fn apply_entries` (inside `impl Env`) | `ocx_package_manager` | takes `&[Entry]`, matches on `crate::package::metadata::env::modifier::ModifierKind` |
| 1628–1660 | `enum ListSeparatorError` + | `ocx_package_manager` | only reachable from `reconcile_list_separators` below |
| 1661–1694 | `impl ClassifyExitCode for ListSeparatorError` | → `ocx_cli` (per contract E3: no library crate keeps a `ClassifyExitCode` impl) | classification always moves to `ocx_cli::exit` regardless of where the error type lives |
| 1695–1798 | `fn reconcile_list_separators` | `ocx_package_manager` | operates on `IntoIterator<Item = &mut Entry>` |
| 1799–1817 | `impl IntoIterator for Env` | `ocx_config` | plain iteration over `Env`'s own map |
| 1818–1939 | `current_dir`, `var`, `flag`, `is_ci`, `string`, `is_valid_env_key`, `is_reserved_ocx_key` | `ocx_config` | plain accessors — ADR names `env::var` explicitly as an ocx-mirror import of `ocx_config` |
| 1940–2086 | `enum CommandResolutionError` + classify impl (2015) | `ocx_config` (type) / `ocx_cli` (classify impl) | trampoline/command-resolution is a pure predicate, used by `ocx_store`, `ocx_project`, `ocx_setup`, `ocx_package_manager` alike — see reasoning below |
| 2087–2279 | `TRAMPOLINE_MARKER`, `TRAMPOLINE_PROBE_BYTES`, `is_ocx_trampoline` | `ocx_config` | pure detector, no package reach |
| 2280–2367 | `enum ForwardedEnvError` | `ocx_package_manager` | only reachable from `forwarded_env` below |
| 2368–2459 | classify impl | `ocx_cli` | contract E3 |
| 2460–2567 | `fn forwarded_env` | `ocx_package_manager` | returns `Vec<crate::package::metadata::env::entry::Entry>` |
| 2568–2579 | `fn records` | `ocx_package_manager` | returns `crate::record::RecordsOptions` (`record` module → `ocx_package_manager`) |
| 2580–2654 | `fn insecure_registries` | `ocx_config` | ADR names it explicitly as an ocx-mirror import of `ocx_config` |
| 2655–2673 | `fn mirrors` | `ocx_config` | returns `config::mirror::MirrorConfig` types |
| 2676–6568 | `#[cfg(test)] mod tests` (3,892 lines) | **split with subject** | not decomposed line-by-line here — tests exercising the `ocx_config`-bound items travel there, tests exercising `apply_entries`/`reconcile_list_separators`/`forwarded_env` travel to `ocx_package_manager`; this is a per-test read at implementation time, not inferable from item boundaries alone |

**Trampoline reasoning.** `TRAMPOLINE_MARKER`/`is_ocx_trampoline`/
`CommandResolutionError` look package-adjacent (they detect an OCX launcher
trampoline script) but are consumed from `activation.rs` (`ocx_project`),
`package_manager/{launcher/body.rs,tasks/render_toolchain.rs,tasks/update_check.rs}`
(`ocx_package_manager`), `setup/session_path.rs` (`ocx_setup`) and
`file_structure/toolchain_store.rs` (`ocx_store`) — verified via
`grep -rl 'TRAMPOLINE_MARKER\|is_ocx_trampoline\|CommandResolutionError' crates/ocx_lib/src`.
`ocx_store` and `ocx_project` sit **below** `ocx_package_manager` in the ADR's
dependency map and may not depend on it, so these three items cannot travel
with the package-aware half — they stay in `ocx_config`, which every one of
those four crates already depends on.

**Unresolved by the item-level read — see § 8 items U1.** `apply_entries` and
`reconcile_list_separators` are called directly, in production code, from
`activation.rs:797` (`ocx_project`) and `shell/reconcile/plan.rs:503,885`
(`ocx_shell`) — verified by reading each call site, none inside
`#[cfg(test)]`. Neither `ocx_project` nor `ocx_shell` may depend on
`ocx_package_manager` per the ADR's own map. Moving these methods to
`ocx_package_manager` as the ADR's one-line rule states breaks those two
callers.

### 3.2 `cli/progress.rs` (560 lines)

| Lines | Item | Target | Why |
|---|---|---|---|
| 1–345 | `LOG_INTERVAL`, `PARENT_BAR`, `ProgressManager`, `Guard`, `BytesBar`, `Spinner`, `open_controlling_terminal`, etc. | `ocx_console` | presentation vocabulary |
| 347–350 | `struct LogWriter` | `ocx_cli` | subscriber wiring (ADR: "subscriber wiring in progress" → `ocx_cli`) |
| 352–355 | `struct LogWriterHandle` | `ocx_cli` | same |
| 357–366 | `impl tracing_subscriber::fmt::MakeWriter<'a> for LogWriter` | `ocx_cli` | same — names `tracing_subscriber::fmt`, a subscriber-configuration type |
| 368–391 | `impl std::io::Write for LogWriterHandle`, `impl Drop for LogWriterHandle` | `ocx_cli` | same |
| 393–560 | `#[cfg(test)] mod span_free_tests` | travels with `ProgressManager` → `ocx_console` (exercises bar creation/drop, not `LogWriter`) | verified by reading the module's doc comment (ADR `adr_progress_architecture` regression spec, no `LogWriter` reference) |

**Unresolved by the item-level read — see § 8 item U2.** `ProgressManager::writer()`
(line 195) returns a `LogWriter` — a type this split moves to `ocx_cli`, a
crate `ocx_console` cannot depend on (wrong direction: `ocx_cli` depends on
every library crate, never the reverse). The method's home (`ocx_console`)
and its return type's new home (`ocx_cli`) are on opposite sides of the split.

### 3.3 `cli/theme.rs` (450 lines)

Per ADR phase 1.2 ("`ocx_console` must not name `Digest`/`Identifier`"):

| Lines | Item | Disposition |
|---|---|---|
| 1–278 | `Theme`, `UnknownTheme`, `StyledInk` trait | → `ocx_console` |
| 279–283 | `impl StyledInk for Digest` | **deleted** |
| 285–296 | `impl StyledInk for Identifier` | **deleted** |
| 298–450 | `#[cfg(test)] mod tests` | → `ocx_console` (tests over `Theme`/`StyledInk` only — spot-checked, no `Digest`/`Identifier` construction found at the top of the module) |

`Theme` becomes render-`&str`-plus-style-token per the ADR's own replacement
text; the two deleted impls are not migrated anywhere — call sites that today
do `digest.ink(theme)` / `identifier.ink(theme)` need a same-crate
replacement at the call site (`ocx_oci`/`ocx_package`, wherever `Digest`/
`Identifier` values are rendered) — that redesign is implementation work, not
a file-mapping question, and is out of this artifact's scope.

### 3.4 `package/tag.rs` (886 lines)

| Lines | Item | Target | Why |
|---|---|---|---|
| 22–103 | `enum InternalTag` + `impl` + `Display` | `ocx_package` | `Tag` machinery |
| 105–180 | `enum Tag` | `ocx_package` | ADR: "`Tag` itself stays in `ocx_package`" |
| 187–196 | `fn referrer_fallback_tag` | `ocx_oci` | ADR: OCI/cosign tag convention |
| ~198–260 | `fn sbom_sidecar_tag` | `ocx_oci` | same |
| ~261–281 | `fn is_referrer_fallback_tag` (private) | `ocx_oci` | same — must become `pub` (or `pub(crate)` re-exported) since `Tag::is_reserved` (line 292, in `ocx_package`) calls it cross-crate after the move; `ocx_package` may depend on `ocx_oci` per the map, so this is legal, just requires a visibility bump |
| 282–370ish | `impl Tag` (including `is_reserved` at 292), `impl From<String> for Tag`, `Display`, `impl From<Tag> for String`, `impl Serialize for Tag` | `ocx_package` | calls into `ocx_oci::is_referrer_fallback_tag` |
| 581–886 | `#[cfg(test)] mod tests` | split with subject: tests of `sbom_sidecar_tag`/`referrer_fallback_tag` → `ocx_oci`; tests of `Tag::is_reserved` and other `Tag` behavior → `ocx_package` (both exist inline per line references at 588–649) |

Note: the separate integration test `crates/ocx_lib/tests/tag_verdicts.rs`
exercises `Tag::is_reserved` against a vendored upstream fixture and moves
whole to `ocx_package` (§ 6) — it is not part of this inline split.

### 3.5 `oci.rs` (91 lines — the `oci` module root)

Not an item-level split in the same sense as the four above — it is the
module-declaration file that currently does `pub mod sign; pub mod verify;
pub mod attest; pub mod simplesigning; pub mod index;` alongside every
generic-`oci` `pub mod`. When `sign/verify/attest/simplesigning` move to
`ocx_sign` and `index` moves to `ocx_index`, their five `pub mod` / `pub use`
lines are deleted from `oci.rs`; the remaining ~80 lines (re-exports of
`oci_client`/`docker_credential` under `pub mod native`, `layer_layout`,
`client`, `copy`, `ssrf`, `transport_policy`, `manifest`, `manifest_builder`,
`referrer`, `resolve_target`, `endpoint`, `identifier`, `host_capabilities`,
`platform`, `digest`, `pinned_identifier`, `repository`, `file_storage`) stay
as `ocx_oci`'s own `lib.rs`-adjacent root. No line-range table needed — it is
a mechanical deletion of five `pub mod` blocks, not a decision.
## 4. Manifests today (verbatim)

### 4.1 `crates/ocx_lib/Cargo.toml`

```toml
[package]
name = "ocx_lib"
version.workspace = true
edition = "2024"
license.workspace = true
publish = false
exclude = ["src/test_data/**"]

[package.metadata.cargo-machete]
ignored = ["liblzma"]

[features]
__testing = []

[dependencies]
schemars.workspace = true
packageurl.workspace = true
tokio.workspace = true
oci-client.workspace = true
reqwest.workspace = true
hyper-util.workspace = true
http.workspace = true
webpki-root-certs.workspace = true
lzma-rust2.workspace = true
serde_json = { workspace = true, features = ["preserve_order", "raw_value"] }
serde_json_canonicalizer.workspace = true
serde.workspace = true
sha2.workspace = true
hex.workspace = true
indexmap.workspace = true
toml.workspace = true
toml_edit.workspace = true
flate2.workspace = true
zstd.workspace = true
bzip2.workspace = true
tar.workspace = true
tempfile.workspace = true
serde_repr.workspace = true
serde_yaml_ng.workspace = true
serde_ignored.workspace = true
bytes.workspace = true
futures.workspace = true
fs4.workspace = true
chrono.workspace = true
regex.workspace = true
strsim.workspace = true
async-trait.workspace = true
percent-encoding.workspace = true
docker_credential.workspace = true
secrecy.workspace = true
base64.workspace = true
symlink.workspace = true
dunce.workspace = true
dirs.workspace = true
sysinfo.workspace = true
clap_builder.workspace = true
colored_json.workspace = true
console.workspace = true
clap_complete.workspace = true
tracing.workspace = true
tracing-log.workspace = true
tracing-subscriber.workspace = true
which.workspace = true
zip.workspace = true
indicatif.workspace = true
thiserror.workspace = true
rpassword.workspace = true
async-compression.workspace = true
liblzma.workspace = true
tokio-util.workspace = true
url.workspace = true
zeroize.workspace = true
pem.workspace = true
starlark.workspace = true
starlark_syntax.workspace = true
starlark_map.workspace = true
starlark_derive.workspace = true
allocative.workspace = true
elf.workspace = true
sigstore = { workspace = true, features = [
  "sign",
  "verify",
  "fulcio",
  "rekor",
  "sigstore-trust-root",
] }
sigstore_protobuf_specs.workspace = true
pki-types.workspace = true
p256.workspace = true
x509-cert.workspace = true
ed25519-dalek.workspace = true

[target.'cfg(unix)'.dependencies]
libc.workspace = true

[target.'cfg(windows)'.dependencies]
junction.workspace = true
windows-sys.workspace = true

[dev-dependencies]
anyhow.workspace = true
tokio = { workspace = true, features = ["test-util"] }
h2.workspace = true
tokio-rustls = { workspace = true, default-features = false }

[lints]
workspace = true
```

(Comments elided for brevity — see `crates/ocx_lib/Cargo.toml` for the
rationale attached to each dependency, several of which are load-bearing for
the crate split: the `serde_json` `preserve_order`/`raw_value` features
"unify additively" across whichever crate ends up owning
`oci/index/wire_writer.rs` (→ `ocx_index`) and the attestation predicate splice
(→ `ocx_sign`); `liblzma` is a link-config-only phantom dependency exempted
from `cargo-machete`.)

**`[package]`**: `name = "ocx_lib"`, `version.workspace = true`,
`edition = "2024"`, `license.workspace = true`, `publish = false`,
`exclude = ["src/test_data/**"]` (this exclude glob matches nothing on disk
today — the ADR's own § Out of scope names this as a pre-existing tidy-up
item, not part of the split).

**`[features]`**: `__testing = []` — forwarded per-crate to the seven crates
that carry a `feature = "__testing"` seam (§ 1 shows the per-crate file
counts; `ocx_cli` must forward all seven, per ADR contract A4).

**`[lints]`**: `workspace = true` — every new crate manifest carries the same
single line (LINT-01) plus `publish = false` (ADR § Public-surface growth).

### 4.2 Root `Cargo.toml`

**`[workspace]`**:
```toml
[workspace]
resolver = "3"
members = ["crates/ocx_cli", "crates/ocx_lib", "crates/ocx_schema", "crates/ocx_shim"]
exclude = ["external/rust-oci-client", "external/docker_credential", "external/sigstore-rs"]
```

**`[workspace.package]`** — used today (crates reference it via `.workspace = true`):
```toml
[workspace.package]
version = "0.6.2"
license = "Apache-2.0"
description = "The simple package manager"
repository = "https://github.com/ocx-sh/ocx"
homepage = "https://ocx.sh"
```

**`[patch.crates-io]`**:
```toml
[patch.crates-io]
oci-client = { path = "external/rust-oci-client" }
docker_credential = { path = "external/docker_credential" }
sigstore = { path = "external/sigstore-rs" }
```

**Profiles** — `[profile.dist]` (`inherits = "release"`, `lto = "fat"`,
`codegen-units = 1`, `opt-level = "s"`, `strip = "symbols"`) and
`[profile.shim]` (`inherits = "release"`, `opt-level = "z"`, `lto = true`,
`codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`). Both are
workspace-root profiles (Cargo does not support per-crate `[profile]`
overrides) and are crate-count-agnostic — the split does not touch them.

**`[workspace.dependencies]`** — used today, extensively (every crate
manifest references entries here via `<name>.workspace = true`; 90+ entries
covering async runtime, serde/formats, CLI, tracing, OCI/HTTP, crypto/
encoding, compression/archive, filesystem, data structures/utility, terminal/
UI, system info, platform, and the exact-pinned Starlark family). Full text
is 240+ lines; see `Cargo.toml:56–298` for the verbatim block — reproducing it
here would duplicate § 1's per-crate dependency columns without adding
information. Two internal-crate entries of note for the split:

```toml
ocx_lib = { path = "crates/ocx_lib" }
ocx = { path = "crates/ocx_cli" }
```

Both will need one new line per extracted crate as phase 2 proceeds (the ADR
does not currently name whether new crates are also declared in
`[workspace.dependencies]` or referenced by bare path per member manifest —
review item, not a file-mapping question).

**`[workspace.lints.rust]`** — used today:
```toml
[workspace.lints.rust]
warnings = "deny"
```

**`[workspace.lints.clippy]`** — present but empty (a placeholder comment
only, no entries):
```toml
[workspace.lints.clippy]
# Placeholder for future workspace-wide clippy overrides.
```

**Inheritance summary, stated plainly:** `[workspace.dependencies]`,
`[workspace.lints]`, and `[workspace.package]` are **all three actively used**
today — every crate manifest inherits version/license from `workspace.package`,
every dependency line is `.workspace = true`, and every crate's `[lints]`
block is exactly `workspace = true`. A new crate manifest under the split
follows the same three patterns with zero new inheritance mechanism needed.

### 4.3 `ocx_cli`'s and `ocx_schema`'s `ocx_lib` dependency lines

`crates/ocx_cli/Cargo.toml`:
```toml
[dependencies]
ocx_lib.workspace = true
```
(plus `chrono`, `tokio`, `clap`, `clap_complete`, `anyhow`, `tempfile`,
`futures`, `tracing`, `serde`, `serde_json`, `quick-junit`, `schemars`,
`serde_repr`, `console`, `dunce`, `thiserror`, `which`, `secrecy`,
`async-trait`, `zeroize`, and `libc` under `cfg(unix)`). Feature:
`__testing = ["ocx_lib/__testing"]` — this line becomes one forwarded feature
per crate that carries the seam (ADR contract A4, seven crates today).

`crates/ocx_schema/Cargo.toml`:
```toml
[dependencies]
ocx.workspace = true
ocx_lib.workspace = true
schemars.workspace = true
serde_json.workspace = true
```
Post-split, per ADR § 3.4: `ocx_schema`'s edges re-point to `ocx_config`
(`Config`), `ocx_package` (`AuthoringMetadata`), `ocx_project`
(`ProjectConfig`, `ProjectLock`) and `ocx_package_manager`
(`PatchDescriptor`, `ExecutionRecord`), in addition to keeping `ocx` (for the
`api/data/**` report types) and `schemars`/`serde_json`.
## 5. `lib.rs` re-export surface

Every `pub use` at the lib root of `crates/ocx_lib/src/lib.rs` (90 lines total)
— what an `ocx_lib::X` consumer sees today, and which module each item comes
from. `lib.rs` itself has no successor crate (`N/A` in § 1/§ 2): each of these
re-exports becomes a plain public item in the crate the source module moved
to, and any consumer reaching `ocx_lib::X` today must re-point to
`<target_crate>::X` (or the module-qualified path within that crate).

| Re-export | Source module | Target crate |
|---|---|---|
| `ConfigError` (aliased from `config::error::Error`) | `config::error` | `ocx_config` |
| `allows_plain_http`, `insecure_hosts` | `config::insecure` | `ocx_config` |
| `ConfigInputs`, `ConfigLoader` | `config::loader` | `ocx_config` |
| `ManagedConfig`, `ManagedConfigError`, `ManagedSnapshotState`, `RefreshPolicy`, `ResolvedManagedConfig`, `check_locked_managed_override`, `enforce_required_snapshot`, `parse_interval`, `resolve_managed_config`, `resolve_managed_target`, `snapshot_matches_source` | `config::managed` | `ocx_config` |
| `MirrorConfig`, `MirrorConfigError`, `MirrorValueShape`, `ParsedMirror`, `ResolvedMirrors`, `parse_mirror_value`, `parse_url`, `resolve_mirror_map` | `config::mirror` | `ocx_config` |
| `PatchConfig`, `PatchConfigError`, `ResolvedPatchConfig`, `expand_patch_path`, `patches_from_env`, `resolve_patch_config` | `config::patch` | `ocx_config` |
| `ConsentPatternError`, `ConsentScopeSpec`, `EntryDefect`, `OCX_CONSENT_NAMESPACES`, `OCX_CONSENT_PATHS`, `ShellConfig`, `ShellConsent`, `consent_entry_defect`, `consent_path_matches`, `effective_consent`, `env_channel`, `normalize_consent_pattern`, `validate_consent_pattern` | `config::shell` | `ocx_config` |
| `Config`, `ConfigTier`, `RegistryConfig`, `RegistryDefaults`, `ToolchainRoot`, `ToolchainRootError`, `ToolchainRootTier` | `config` (root) | `ocx_config` |
| `Error`, `Result` | `error` | **dissolves** — `Error` deleted per contract E1; no crate re-exports a replacement, each crate defines its own error type(s) |

**Every re-export at the lib root is a `config::*` item.** `error::{Error,
Result}` is the only exception, and it dissolves rather than moving. This
means the entire `pub use` surface of today's `ocx_lib` maps onto exactly one
target crate, `ocx_config` — a fact worth stating plainly since it means an
external consumer reaching `ocx_lib::Config`, `ocx_lib::ConfigLoader`, etc.
(the pattern the doc comment on `ConfigError` names — "so crates that depend
on `ocx_lib` can reference `ocx_lib::ConfigError`") re-points wholesale to
`ocx_config::*` with no fan-out to other crates.

The `pub mod prelude` block (`pub use crate::error::{Error, Result}` plus four
`utility::*_ext` traits: `ResultExt`, `SerdeExt`, `StringExt`, `VecExt`) has no
single successor either — `Error`/`Result` dissolve, and the four extension
traits live in `ocx_util` (part of `utility` minus `fs/assemble.rs`, § 2).
Whether a `prelude` module is worth re-creating in `ocx_util` alone (dropping
the now-dissolved `Error`/`Result` half) is a design question for the
execution plan, not a file-mapping fact — noted here as a gap, not resolved.
## 6. Integration tests

`crates/ocx_lib/tests/` (4 files, no `mod.rs` — each `.rs` file directly under
`tests/` is its own Cargo integration-test binary) plus `tests/fixtures/`
(directories, not `.rs` files, listed for completeness).

| File | LOC | Target crate | Why |
|---|---|---|---|
| `crates/ocx_lib/tests/index_wire_conformance.rs` | 291 | `ocx_index` | ADR: "Integration tests move with their subject … the byte-parity gate for a format this ADR promises is unchanged" |
| `crates/ocx_lib/tests/dispatch_conformance.rs` | 207 | `ocx_index` | same — cross-repo decode parity for OCI dispatch objects, exercises `LocalIndex::stage_dispatch_bytes` |
| `crates/ocx_lib/tests/live_index_wire.rs` | 104 | `ocx_index` | same — conformance against bytes `index.ocx.sh` actually served |
| `crates/ocx_lib/tests/tag_verdicts.rs` | 80 | `ocx_package` | ADR: "exercises `package::tag::Tag::is_reserved` against a vendored upstream fixture" |

**Fixtures (directories, move with the test that reads them):**

| Directory | Target crate | Contents |
|---|---|---|
| `tests/fixtures/index_wire/` (17 files, minus `tag_verdicts.json` below: `README.md`, `SOURCE_COMMIT`, `catalog/normal.json`, `config/normal.json`, `cpython/{codepoint_escapes.txt,generate.py,layout.json,top_level_array.json}`, `dispatch/expected_platforms.json`, `dispatch/sha256/*.json` ×4, `root/{full-fields,minimal,with-source,with-variants}.json`) | `ocx_index` | vendored `ocx-sh/index` vectors pinned by `SOURCE_COMMIT`, re-vendored by `test/scripts/sync_index_conformance.sh` (path unchanged per ADR) |
| `tests/fixtures/index_wire/tag_verdicts.json` | `ocx_package` | verified by `grep -rl tag_verdicts.json crates/ocx_lib/tests/*.rs` — read only by `tests/tag_verdicts.rs`, not by the index-wire conformance tests despite living in the `index_wire/` fixture directory. Moves with its reader, not with its directory-mates. |
| `tests/fixtures/live_index_ocx_sh/` (3 files: `c-index.json`, `config.json`, `PROVENANCE.md`) | `ocx_index` | captured production `index.ocx.sh` responses |
## 7. Counts check — measured vs. the ADR's numbers

The ADR states: 364 files, 278,777 LOC, 3,818 `#[test]`, 241 `crate::test`
sites in 22 files, 11 `__testing` files (all `crates/ocx_lib/src/` only, per
the ADR's own phrasing "364 files across 35 top-level modules"). Measured here
via `scan.py` against the same tree (`evelynn` HEAD `d8750fd7`, 2026-09-16):

| Metric | ADR (2026-09-06/16) | Measured now (`src/` only) | Delta |
|---|---|---|---|
| Files | 364 | **388** | +24 |
| LOC | 278,777 | **348,648** | +69,871 (+25.1%) |
| `#[test]` | 3,818 | **4,460** | +642 |
| `crate::test` sites | 241 | **367** | +126 |
| `crate::test` files | 22 | **44** | +22 |
| `__testing` files | 11 | **17** | +6 |

Including `tests/` (4 files) and `test/` (5 files) — not part of the ADR's
cited 364/278,777 baseline, but part of this artifact's scope per the task
brief: **397 files, 349,661 LOC, 4,478 `#[test]`.**

**This is a large mismatch, stated as fact, not judgment.** The ADR's own
Context section computed its baseline from the dossier's recon
(`.claude/artifacts/research_crate_split_recon.md`), dated before the "entry
gate" — `feat/lazy-package-loading`,
[#420](https://github.com/ocx-sh/ocx/pull/420) and
[#426](https://github.com/ocx-sh/ocx/pull/426) — merged. The ADR's own phase 1
entry gate explicitly anticipates this: "re-run the phase-0.2 edge inventory
after those merges and diff the map against it… every `claim` edge must be
assigned before phase 1 starts." `claim/` (4 files, 2,882 LOC by itself) is
one visible piece of the drift — the ADR's Implementation Plan section
confirms `claim.rs` was "absent from `origin/main` today" as of its own
writing and is present now. The remaining ~67,000 LOC and ~600 `#[test]` of
drift is not accounted for by `claim/` alone (2,882 LOC) and was not tracked
down file-by-file in this pass — it reflects three weeks of ordinary feature
work (2026-08-27 dossier ratification date referenced elsewhere in the ADR to
2026-09-16 acceptance) landing on `evelynn` in the interim, consistent with
the commit log's density (five commits shown in the session's git-status
snippet alone, on top of whatever landed before the ADR's own re-validation
pass noted in its Changelog). **Whoever executes phase 0.2's edge inventory
should treat 388/348,648/4,460 as the working baseline, not the ADR's cited
364/278,777/3,818** — the ADR's own phase-1 entry gate already requires a
re-run of this measurement before phase 1 starts, and this artifact's numbers
are that re-run for the file/LOC/test-count axis (not yet for the edge-count
axis, which needs the phase-0.2 script, not this one).

**`__testing` files, cross-checked against ADR contract A4's list of seven
crates** (`ocx_announce`, `ocx_config`, `ocx_store`, `ocx_oci`, `ocx_package`,
`ocx_package_manager`, `ocx_shell`) — § 1's per-crate table shows `__testing`
sites landing in exactly those seven target crates (`ocx_announce`: 10 sites/3
files; `ocx_config`: 5/3; `ocx_oci`: 10/3; `ocx_package`: 2/2;
`ocx_package_manager`: 13/2; `ocx_shell`: 3/1; `ocx_store`: 5/1) plus zero
elsewhere — **the seven-crate list in contract A4 is confirmed unchanged** by
this file-level scan, even though the ADR's own file-count baseline (11) is
now 17 files (the count of *files*, not crates, grew — consistent with more
feature work landing inside the same seven subsystems, not a new subsystem
gaining the seam).
## 8. Undecidable — needs orchestrator ruling

Both items are DAG conflicts the item-level read in § 3 surfaced, not
ordinary file-assignment ambiguity — the ADR's one-line placement rule and the
ADR's own "may depend on" table cannot both hold for these items.

### U1 — `Env::apply_entries` / `Env::set_forwarded_env` / `reconcile_list_separators` / `ListSeparatorError` / `ForwardedEnvError` / `forwarded_env()` (`env.rs`, § 3.1)

- **ADR's rule:** "the package-aware composition half … goes to
  `ocx_package_manager` beside `composer.rs`" (§ Placement rulings).
- **Candidate A — move to `ocx_package_manager` as stated.** Breaks two
  confirmed production call sites: `activation.rs:797`
  (`crate::env::reconcile_list_separators(entries.iter_mut())?;`, inside
  `ocx_project`) and `shell/reconcile/plan.rs:503,885`
  (`probe.apply_entries(&folded)`, `env.apply_entries(&plan.sets)`, inside
  `ocx_shell`). Neither `ocx_project` nor `ocx_shell` may depend on
  `ocx_package_manager` per the ADR's crate map (`ocx_package_manager`
  depends on `ocx_project`, not the reverse; `ocx_shell` is a dependency of
  `ocx_project`, sitting below `ocx_package_manager` too). This is not a
  hypothetical — both call sites are in production code, verified by reading
  the surrounding function bodies (neither is behind `#[cfg(test)]`).
- **Candidate B — keep these items in `ocx_config` beside `Env`.** They
  reach `crate::package::metadata::env::{entry::Entry, modifier::ModifierKind}`,
  types that live in `ocx_package`. `ocx_config`'s allowed dependency set
  (`ocx_trust`, `ocx_oci`, `ocx_util`, `ocx_exit`) does not include
  `ocx_package` — and `ocx_package` already depends on `ocx_config`
  (`ocx_package`'s "may depend on" row lists it directly), so adding
  `ocx_config → ocx_package` closes a two-cycle, the exact class of edge this
  ADR exists to eliminate (63 reciprocated pairs counted in § Context).
- **What neither candidate is:** a simple move. Resolving this needs either
  (a) a phase-1 inversion at `activation.rs:797` and
  `shell/reconcile/plan.rs:503,885` — restructuring so `ocx_project`/
  `ocx_shell` no longer call these functions directly (not currently named
  among the ADR's 14 phase-1 boundary tests, 1.1–1.14), or (b) relocating
  `Entry`/`ModifierKind` themselves to a lower crate so the functions can stay
  with `Env` in `ocx_config` without creating the cycle, or (c) accepting that
  `Env`'s package-aware methods live in `ocx_package_manager` as free
  functions / an extension trait and that `activation.rs`/`shell.rs` reach
  them through a narrower interface passed down from a caller that already
  sits in `ocx_package_manager`'s call chain (i.e., the composition result
  reaches `ocx_project`/`ocx_shell` as already-applied data, never as a call
  they make themselves) — each is a design decision, not a file move.

### U2 — `ProgressManager::writer()` return type vs. `LogWriter`'s new home (`cli/progress.rs`, § 3.2)

- **ADR's rule:** "Subscriber initialisation travels with neither[…] moves to
  `ocx_cli`" (§ Placement rulings), which the item-level read resolves as
  `LogWriter`/`LogWriterHandle`/`impl MakeWriter` (lines 347–391).
  `ProgressManager` (the rest of the file) stays in `ocx_console` per the
  same ruling.
- **The conflict:** `ProgressManager::writer(&self) -> LogWriter` (line 195)
  is a method on the type that stays in `ocx_console`, returning a type that
  moves to `ocx_cli`. `ocx_console` cannot depend on `ocx_cli` — `ocx_cli` is
  the application layer that depends on every library crate, never the
  reverse (§ 2's whole crate map has no arrow pointing into `ocx_console`
  from above except through `ocx_oci`/`ocx_shell`/`ocx_package_manager`, all
  of which are themselves below `ocx_cli`).
- **What this needs:** either `writer()` is deleted from `ocx_console`'s
  `ProgressManager` and `ocx_cli` constructs `LogWriter` itself from a field
  `ProgressManager` exposes some other way (e.g. a getter returning the
  `Arc<indicatif::MultiProgress>` handle, which is `pub(crate)` today per line
  ~... and would need a visibility widening), or `LogWriter`/`LogWriterHandle`
  stay in `ocx_console` after all and only the subscriber *registration* call
  (wherever `tracing_subscriber::registry().with(...)` is invoked) moves to
  `ocx_cli` — narrower than "the whole `MakeWriter` impl moves." Either
  resolution is an implementation decision the ADR's one-line ruling does not
  settle by itself.
