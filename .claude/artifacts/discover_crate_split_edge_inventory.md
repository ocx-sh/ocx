# Discover: crate-split mechanical edge inventory (ADR phase 0.2, run ahead of phase 1)

Produced by `.tmp/hex/plan-crate-split/edge_inventory.py` (stdlib Python 3,
`--json` flag, no third-party deps), against `evelynn` HEAD `d8750fd7`. Every
count in this document is quoted script output — none estimated. Script and
raw JSON kept at `.tmp/hex/plan-crate-split/` (gitignored, durable for this
session): `edge_inventory.py`, `edge_inventory.json`, `report.md` (the raw
run this document is built from).

## 1. Entry-gate facts (Part A)

| Check | Command | Result |
|---|---|---|
| PR #420 | `gh pr view 420 --json state,mergedAt` | `{"state":"MERGED","mergedAt":"2026-09-06T20:17:15Z"}` |
| PR #426 | `gh pr view 426 --json state,mergedAt` | `{"state":"MERGED","mergedAt":"2026-09-07T06:53:06Z"}` |
| PR #169 | `gh pr view 169 --json state,mergedAt` | `{"state":"CLOSED","mergedAt":null}` — closed unmerged, not part of the gate |
| `claim.rs` on HEAD | file check | **exists** |
| `lazy.rs` on HEAD | file check | **exists** |
| `activation.rs` on HEAD | file check | **exists** |
| `activate.rs` on HEAD | file check | **exists** |
| `ladder.rs` on HEAD | file check | **exists** |
| Open PRs | `gh pr list --state open --json number,title` | [#153](https://github.com/ocx-sh/ocx/pull/153) "chore(deps): bump the rust-deps group…", [#146](https://github.com/ocx-sh/ocx/pull/146) "ci(deps): bump the actions group…" — both dependency bumps, unrelated to the crate-split gate |
| `origin/main` tip | `/usr/sbin/git log --oneline -1 origin/main` | `3538b755 release: v0.6.2` |
| HEAD contains `origin/main` tip | `/usr/sbin/git merge-base --is-ancestor <sha> HEAD` | **YES** |

**Verdict: entry gate part 1 (feat/lazy-package-loading, #420, #426 merged) is satisfied.** Part 2 of the gate ("re-run the phase-0.2 edge inventory after those merges and diff the map against it") is this document — it is the re-run, produced on the tree that already contains `claim`/`lazy`/`activate`/`ladder`/the rewritten `activation.rs`. Section 4 is the diff against the ADR's phase-1 inversion table.

## 2. Per-crate disallowed-edge totals (ADR extraction order)

"Disallowed" = a measured reference from crate X's files to crate Y's files where Y is not in X's ADR "May depend on" set and Y != X. `DISSOLVE` (crate-wide `Error`/`Result`/`ArcError`, §5) and `ocx_test_support` (dev-only edge, §6) are reported separately, not counted here.

| Crate | Disallowed refs (today) |
|---|---|
| `ocx_exit` | **0** |
| `ocx_util` | **67** |
| `ocx_console` | **3** |
| `ocx_oci` | **82** |
| `ocx_trust` | **27** |
| `ocx_config` | **175** (largest — dominated by `env.rs`'s still-unsplit package-aware half, `env.rs → ocx_package` = 72 refs, §4 U1) |
| `ocx_store` | **15** |
| `ocx_index` | **40** |
| `ocx_package` | **43** |
| `ocx_sign` | **46** |
| `ocx_shell` | **2** |
| `ocx_project` | **52** |
| `ocx_package_manager` | **33** |
| `ocx_announce` | **14** |
| `ocx_setup` | **22** |
| `ocx_script` | **0** (confirms the ADR's "in-degree 0 today" claim for the phase-0.5 leaf extraction) |
| `ocx_test_support` | **0** (n/a — dev-only, not walked as a "from" crate; its content lives under `crates/ocx_lib/test/`, outside this script's `src/**` scope per the task brief) |

(`ocx_cli`, the application-layer crate that already owns 5 files per the file map — `cli/clap.rs`, `cli/classify.rs`, `cli/error.rs`, `cli/log_level.rs`, `cli/log_settings.rs` — has **0** disallowed refs, consistent with its "may depend on every crate above" row.)

Files scanned: **388** (matches `discover_crate_split_file_map.md`'s own `src/`-only count exactly); **0** unmapped files.

## 3. Full disallowed-edge list

Capped at 5 example sites per edge; file counts are exact. Full site lists are in `edge_inventory.json` under `disallowed_by_crate`.

| From | To | Count | Files | Example sites |
|---|---|---|---|---|
| ocx_util | ocx_cli | 10 | 3 | archive/error.rs:6; compression/error.rs:6; tls.rs:15; tls.rs:1661; tls.rs:1670 |
| ocx_util | ocx_config | 7 | 3 | tls.rs:16; tls.rs:16; tls.rs:541; utility/boolean_string.rs:95; utility/path.rs:245 |
| ocx_util | ocx_console | 11 | 5 | archive/tar.rs:13; archive/zip.rs:15; archive.rs:6; tls.rs:17; utility/fs/dir_walker.rs:216 |
| ocx_util | ocx_exit | 11 | 5 | archive/error.rs:7; compression/error.rs:7; tls.rs:15; utility/fs/empty_or_absent.rs:12; utility/fs/path.rs:104 |
| ocx_util | ocx_oci | 5 | 2 | compression.rs:6 (×3); compression.rs:383; tls.rs:19 |
| ocx_util | ocx_shell | 4 | 1 | utility/path.rs:46; utility/path.rs:391; utility/path.rs:414; utility/path.rs:452 |
| ocx_util | ocx_store | 19 | 2 | archive/tar.rs:255; archive/tar.rs:256; archive/tar.rs:1136; archive/zip.rs:332; archive/zip.rs:333 |
| ocx_console | ocx_oci | 2 | 1 | cli/theme.rs:23 (×2 — `Digest`, `Identifier`) |
| ocx_console | ocx_package | 1 | 1 | cli/theme.rs:24 (`Visibility`) |
| ocx_oci | ocx_cli | 23 | 3 | auth/error.rs:5; oci/client/error.rs:6; oci/client/error.rs:494; oci/client/error.rs:500; oci/client/native_transport.rs:1752 |
| ocx_oci | ocx_config | 10 | 5 | auth/store.rs:205; auth.rs:4; oci/client/builder.rs:404; oci/client/mirror_map.rs:15; oci/client.rs:3483 |
| ocx_oci | ocx_package | 34 | 1 | oci/client.rs:4 (×3); oci/client.rs:1522; oci/client.rs:2502 |
| ocx_oci | ocx_package_manager | 5 | 1 | oci/client.rs:1882; oci/client.rs:1912; oci/client.rs:1921; oci/client.rs:7711; oci/client.rs:7717 |
| ocx_oci | ocx_sign | 3 | 2 | oci/client/transport.rs:15; oci/referrer/manifest.rs:20; oci/referrer/manifest.rs:198 |
| ocx_oci | ocx_store | 7 | 3 | auth/store.rs:208; oci/client.rs:2506; oci/client.rs:1401; oci/host_capabilities.rs:1234; oci/host_capabilities.rs:1235 |
| ocx_trust | ocx_cli | 2 | 1 | trust.rs:2005; trust.rs:2010 |
| ocx_trust | ocx_console | 1 | 1 | trust.rs:72 |
| ocx_trust | ocx_exit | 1 | 1 | trust.rs:2006 |
| ocx_trust | ocx_sign | 23 | 1 | trust.rs:73; trust.rs:1084; trust.rs:896; trust.rs:1026; trust.rs:1052 (+18 more, all `trust.rs`) |
| ocx_config | ocx_cli | 28 | 3 | config/edit.rs:30; config/error.rs:6; config/loader.rs:2601; config/loader.rs:5268; config/loader.rs:5437 |
| ocx_config | ocx_console | 11 | 5 | config/loader.rs:21; config/managed.rs:540; config/mirror.rs:29; config/patch.rs:324; config/shell.rs:21 |
| ocx_config | ocx_index | 5 | 1 | config/loader.rs:309; config/loader.rs:2232; config/loader.rs:2330; config/loader.rs:2462; config/loader.rs:2466 |
| ocx_config | ocx_package | 72 | 1 | env.rs:1059; env.rs:1698; env.rs:2461; env.rs:2462; env.rs:2463 (all `env.rs` — the package-aware half, §4 U1) |
| ocx_config | ocx_package_manager | 15 | 1 | config/loader.rs:6650; config/loader.rs:6706; config/loader.rs:7006; config/loader.rs:7007; config/loader.rs:7049 |
| ocx_config | ocx_project | 4 | 1 | config/loader.rs:5446; config/loader.rs:5448; config/loader.rs:5450; config/loader.rs:5548 |
| ocx_config | ocx_setup | 1 | 1 | config/edit.rs:307 |
| ocx_config | ocx_shell | 10 | 1 | env.rs:780; env.rs:2682 (×4 more) |
| ocx_config | ocx_sign | 5 | 1 | managed_config/publish.rs:322; :679; :688; :1078; :1310 |
| ocx_config | ocx_store | 24 | 1 | config/loader.rs:334; :335; :1785; :1797; :3713 (+19 more, all `config/loader.rs`) |
| ocx_store | ocx_cli | 3 | 2 | file_structure/error.rs:4; file_structure/toolchain_store.rs:94; file_structure/toolchain_store.rs:823 |
| ocx_store | ocx_console | 12 | 5 | codesign.rs:8; file_structure/blob_store.rs:6; file_structure/layer_store.rs:6; file_structure/package_store.rs:7; file_structure/shim_bin_store.rs:220 |
| ocx_index | ocx_cli | 23 | 2 | file_structure/index_store.rs:1180; oci/index/chained_index.rs:5473 (×4 more) |
| ocx_index | ocx_console | 14 | 3 | file_structure/index_store.rs:503; :514; oci/index/chained_index.rs:14; oci/index/local_index.rs:13; :1512 |
| ocx_index | ocx_package | 3 | 3 | oci/index/local_index.rs:13; oci/index/oci_index.rs:9; oci/index.rs:5 |
| ocx_package | ocx_cli | 35 | 2 | package/bin_scan.rs:485; package/dependency_pinning.rs:27; :521; :551; :656 |
| ocx_package | ocx_console | 8 | 5 | package/cascade/apply.rs:34; package/cascade/gather.rs:39; package/cascade.rs:29; package/dependency_pinning.rs:34; package/install_status.rs:6 |
| ocx_sign | ocx_cli | 14 | 3 | oci/attest/pipeline.rs:616; :1862; oci/sign/error.rs:14 (×2); oci/sign/fulcio.rs:318 |
| ocx_sign | ocx_config | 5 | 4 | oci/sign/key_backend.rs:127; oci/sign/key_ref.rs:117; oci/sign/pipeline.rs:2073; :2141; oci/verify/pipeline.rs:4659 |
| ocx_sign | ocx_console | 2 | 2 | oci/sign/key_backend.rs:25; oci/sign/simplesigning_write.rs:256 |
| ocx_sign | ocx_index | 16 | 1 | oci/attest/pipeline.rs:67; :634; :646; :657; :670 (+11 more, same file) |
| ocx_sign | ocx_package | 3 | 1 | oci/verify/simplesigning_read.rs:193; :194; :205 |
| ocx_sign | ocx_store | 6 | 5 | oci/attest/pipeline.rs:64; oci/sign/pipeline.rs:28; oci/sign/referrers.rs:16; oci/verify/pipeline.rs:52; oci/verify/trust_cache.rs:27 |
| ocx_shell | ocx_cli | 1 | 1 | ci/error.rs:4 |
| ocx_shell | ocx_setup | 1 | 1 | shell/reconcile/fingerprint.rs:209 |
| ocx_project | ocx_cli | 21 | 2 | activation.rs:91; project/config.rs:1230; :1754; :2186; :2597 |
| ocx_project | ocx_console | 17 | 3 | activate.rs:121; :210; activation.rs:366; lazy.rs:87; :173 |
| ocx_project | ocx_package_manager | 13 | 1 | activation.rs:46; :51; :752 (×2); :3437 |
| ocx_project | ocx_setup | 1 | 1 | activation.rs:327 |
| ocx_package_manager | ocx_cli | 18 | 3 | launch.rs:55; package_manager/composer.rs:5571; package_manager/error.rs:4; :423; :433 |
| ocx_package_manager | ocx_setup | 1 | 1 | package_manager/launcher/generate.rs:203 |
| ocx_package_manager | ocx_shell | 1 | 1 | package_manager/tasks/resolve.rs:7947 |
| ocx_package_manager | ocx_trust | 13 | 1 | package_manager/tasks/auto_verify.rs:50 (×2); :309; :311 (×2) |
| ocx_announce | ocx_cli | 12 | 3 | announce/error.rs:303; :237; announce.rs:937; claim/error.rs:27 (×2) |
| ocx_announce | ocx_console | 2 | 2 | claim/owners.rs:245; claim.rs:41 |
| ocx_setup | ocx_cli | 12 | 5 | setup/bootstrap.rs:473; setup/error.rs:13; setup/session_path/linux.rs:474; :macos.rs:825; :windows.rs:580 |
| ocx_setup | ocx_console | 5 | 2 | setup/bootstrap.rs:27; setup.rs:810; :823; :885; :984 |
| ocx_setup | ocx_index | 4 | 1 | setup.rs:1362 (×2); :1365 (×2) |
| ocx_setup | ocx_package | 1 | 1 | setup/bootstrap.rs:352 |

## 4. Delta against the ADR's phase-1 inversion table (1.1–1.14)

Every named site re-verified directly against source (`command grep -nE` on the exact identifier, comment-stripped by hand-reading — several apparent hits turned out to be rustdoc cross-reference links (`` [`x`](crate::a::b::x) `` inside `///`/`//` comments), which the script correctly excludes and which naive `grep` does not; that distinction is called out below wherever it mattered).

| # | Inversion | Status today | Evidence |
|---|---|---|---|
| 1.1 | Move classification ladder + 58+3 impls to `ocx_cli/exit` | **NOT DONE** | 64 `ClassifyExitCode` + 4 `ClassifyErrorKind` impls scattered outside `ocx_cli` today (§7); only `cli/error.rs`'s own 2 already sit in `ocx_cli`'s file-map allocation |
| 1.2 | Delete `impl StyledInk for Digest`/`Identifier` in `theme.rs` | **NOT DONE** | `cli/theme.rs:23` still does `use crate::oci::{Digest, Identifier};`, `:24` still does `use crate::package::metadata::visibility::Visibility;` — both disallowed edges present (§3) |
| 1.3 | Move subscriber init out of `cli/{log_level,log_settings,progress}` | **NOT DONE** | Files not yet split; `cli/progress.rs`'s `LogWriter`/`LogWriterHandle`/`impl MakeWriter` (lines 347–391 per the file map) are still physically inside the file the script attributes to `ocx_console` by majority — the split itself, not a reference count, is the remaining work |
| 1.4 | `oci`/`auth` stop reading `config`/`env`/`project` | **NOT DONE** | Named site confirmed **at the exact same call**, only the line drifted (165→205): `auth/store.rs:205` — `crate::env::var("DOCKER_CONFIG")`. `auth.rs:4` also imports `env` (`use crate::{env, log, oci, prelude::*};`) |
| 1.5 | `oci::client` stops reaching `announce`, `publisher`, `patch`, `managed_config`, package `description` | **PARTIALLY DONE** | `announce`/`managed_config`: **0 refs measured anywhere in the `oci` module** — already clear. `publisher`/`patch`: still present — `oci/client.rs` has 15 module-level refs to `publisher` (e.g. `crate::publisher::LayerRef`, real code, lines 1419+) and 5 real refs to `patch::` (lines 1882, 1912, 1921, 7711, 7717 — confirmed **not** doc comments; 3 more textual hits at 1859/1861/1865 are rustdoc links and correctly excluded) |
| 1.6 | Move `referrer_fallback_tag`/`sbom_sidecar_tag` out of `package::tag` into `ocx_oci` | **NOT DONE (mapping pre-applied)** | Both named sites confirmed **byte-for-byte**: `oci/client/transport.rs:16` and `oci/verify/pipeline.rs` (now 4 sites: 2976, 7912, 8052, 8093, drifted from the ADR's single citation). Because the file map already assigns these two functions to `ocx_oci` (§3.4), this script's classifier treats them as already-home — so they do **not** appear in §3's disallowed table even though the code has not physically moved yet. This is by design (the map is the target state), not a false negative — flagged here so the gap is visible |
| 1.7 | `utility` stops constructing other modules' errors, stops serving `shell` | **NOT DONE** | Named site confirmed **at the exact same line**: `utility/boolean_string.rs:95` constructs `crate::config::error::Error::InvalidBooleanString`. `utility/path.rs` → `shell::` confirmed at 4 real code sites (46, 391, 414, 452); 4 more textual hits (12, 27, 155, 183) are rustdoc links, correctly excluded |
| 1.8 | `trust` stops reaching `config`, `cli`, `managed_config`; `resolve_tiered` stays | **PARTIALLY DONE / regressed on `cli`** | `config`/`managed_config`: **0 real-code refs** — every textual `crate::config::`/`crate::managed_config::` hit in `trust.rs` (6 of them) is a rustdoc link, not code; already clear. `cli`: **NOT clear** — `trust.rs:2005` (`crate::cli::classify_error`), `:2006` (`crate::cli::ExitCode::ConfigError`), `:2010` (`crate::cli::ClassifyErrorKind::kind_detail`) are real `#[cfg(test)]` code, a new edge the ADR's citation for 1.8 does not mention |
| 1.9 | Signing stack (`StateStore` injection; sign/verify take resolved target) | **NOT DONE** | All 4 named sites confirmed **at the exact same lines**: `oci/sign/referrers.rs:16`, `oci/verify/trust_cache.rs:27`, `oci/verify/trust_resolve.rs:43`, and `trust_resolve.rs:156`'s `state.tuf_cache_dir()` call — zero drift. `StateStore` usage is far wider than the ADR's citation (13 sites in `trust_resolve.rs` alone), consistent with the ADR's own caveat that the security review's grep-based citation undercounted |
| 1.10 | Split `env` (vocabulary/accessor → `config`; composition → `package_manager`) | **NOT DONE** | `env.rs` is still one 6,568-line file; `ocx_config → ocx_package` = 72 refs (all in `env.rs`, §3) is exactly the still-unsplit package-aware half. See U1 below — the ADR's own one-line rule for this split does not resolve cleanly (file map §8, item U1) |
| 1.11 | Relocate `file_structure/index_store` under `oci/index` | **NOT DONE (mapping pre-applied)** | File not moved; already logically assigned `ocx_index` by the file map, same "target-state pre-applied" caveat as 1.6 |
| 1.12 | Invert `shell -> activation` (ADR cites 5 refs) | **DONE / already moot** | **0 real code refs measured.** All 5 textual `crate::activation` hits in `shell/reconcile/{ledger,plan}.rs` and `shell/reconcile.rs` are rustdoc cross-reference links inside `///`/`//!` doc comments, not code. The named boundary test (`shell_does_not_import_activation`) would already pass today |
| 1.13 | `config` stops reaching `package_manager`, `project`, `record` | **NOT DONE** | Module-level (the ADR's own unit): `config → project` = 4 real refs (all `config/loader.rs`: 5446, 5448, 5450, 5548 — 5 more textual hits in `config.rs`/`edit.rs`/`patch.rs`/`shell.rs` are all rustdoc links, correctly excluded); `config → record` = 4 (config.rs:175,1686; loader.rs:6650,6706); `config → package_manager` = 4 (loader.rs:7006,7007,7049,7050). Crate-level `ocx_config → ocx_package_manager` is 15 because `env.rs`'s package-aware half (also inside the `ocx_config`-assigned `env.rs`) contributes the remainder |
| 1.14 | `host_capabilities` becomes a pure detector; cache moves to store | **NOT DONE** | `oci/host_capabilities.rs:1234-1235` (`record_path()`) still calls `crate::file_structure::default_ocx_root()` and `crate::file_structure::StateStore::new(...)` directly — real code, confirmed |

**Uncovered disallowed edges — new since the ADR's dossier (not named by 1.1–1.14):**

1. **`ocx_trust → ocx_cli` (2 refs, `trust.rs:2005,2010`)** — new, inside a `#[cfg(test)]` block added since the dossier. `[proposal]` fold into 1.1: when the classification ladder moves to `ocx_cli::exit`, this test either moves with the type under test or gets a same-crate assertion helper; no separate boundary test needed if 1.1's `cli_defines_no_classification` scope also excludes `ocx_trust`'s test code (it already would, since the check scans `ocx_trust`, not `ocx_cli`).
2. **`ocx_announce → ocx_cli` (12 refs) and `ocx_announce → ocx_console` (2 refs)**, sourced from `claim/`, `forge/`, and `announce.rs` — **entirely new modules/edges** (`claim` did not exist in the ADR's own baseline tree, per the ADR's Implementation Plan section). `[proposal]` add an inversion **1.15**: "`claim`/`announce`/`forge` stop constructing `ClassifyExitCode`/`ClassifyErrorKind` locally and stop reaching presentation types" — boundary test `announce_does_not_import_cli`, alongside the existing table.
3. **`ocx_project → ocx_cli`/`ocx_console`/`ocx_package_manager`/`ocx_setup` edges sourced from `activate.rs`/`ladder.rs`/`lazy.rs`** (new modules, e.g. `activation.rs:3437`, `lazy.rs:87,173`, `activate.rs:121,210`) — not named by any of 1.1–1.14 since these modules postdate the dossier. `[proposal]` add an inversion **1.16**: "`activate`/`ladder`/`lazy` stop reaching `package_manager` and presentation types" alongside 1.12's existing `shell_does_not_import_project`/`shell_does_not_import_activation` pair — boundary test `project_does_not_import_package_manager` (the mirror of the existing A-45 pair, since `ocx_package_manager` already depends on `ocx_project`, not the reverse).
4. **`ocx_config → ocx_package` (72 refs) and `ocx_config → ocx_shell` (10 refs)**, entirely from `env.rs` — this is exactly the U1 conflict the file map already surfaced (§8): the ADR's one-line env-split rule breaks two confirmed production callers (`activation.rs:797`, `shell/reconcile/plan.rs:503,885`) if followed literally. **Not a new finding, but this inventory is independent confirmation of U1's scale**: 72+10 = 82 of `env.rs`'s references are the package-aware half this script routes to `ocx_package_manager` by the file map's own item list, yet `env.rs` stays whole (assigned to `ocx_config`) until U1 is resolved by the orchestrator.

**Total newly-uncovered disallowed edges (not named by 1.1–1.14): 3 named clusters (2+14+31 = 47 individual reference sites) plus U1's already-known 82.**

## 5. Crate-wide `Error`/`Result`/`ArcError` (DISSOLVE) usage census

Sizes the E1 dissolution (per-crate count of files/refs that must gain their own error type instead of naming the crate-wide `Error`).

| Crate | Refs | Files |
|---|---|---|
| ocx_package_manager | 234 | 33 |
| ocx_index | 160 | 11 |
| ocx_store | 45 | 15 |
| ocx_project | 43 | 6 |
| ocx_package | 52 | 12 |
| ocx_util | 51 | 12 |
| ocx_sign | 34 | 3 |
| ocx_cli | 13 | 1 |
| ocx_config | 11 | 3 |
| ocx_oci | 9 | 4 |
| ocx_shell | 10 | 5 |
| ocx_announce | 3 | 1 |
| ocx_setup | 2 | 1 |
| ocx_console | 1 | 1 |

`ocx_package_manager` and `ocx_index` dominate — consistent with `package_manager`'s 43.6K LOC (largest crate by far) and `oci/index/*`'s heavy use of the workspace-wide `Result` alias.

## 6. `crate::test::` (→ `ocx_test_support`, dev edge) usage per crate

| Crate | Refs | Files |
|---|---|---|
| ocx_config | 158 | 5 |
| ocx_shell | 57 | 7 |
| ocx_package_manager | 41 | 6 |
| ocx_project | 38 | 5 |
| ocx_announce | 16 | 7 |
| ocx_setup | 19 | 2 |
| ocx_sign | 4 | 2 |
| ocx_index | 5 | 2 |
| ocx_store | 3 | 2 |
| ocx_oci | 3 | 3 |
| ocx_trust | 2 | 1 |
| ocx_util | 2 | 2 |

## 7. `ClassifyExitCode`/`ClassifyErrorKind` impl census

Multi-line-aware, comment-stripped `impl(?:<...>)?\s+(?:path::)*ClassifyExitCode\s+for\b` / `...ClassifyErrorKind...` scan.

**`ClassifyExitCode`: 64 total (ADR baseline: 58).** Per target crate: `ocx_config` 13 (incl. `config.rs` — new since the ADR's "51 files" citation), `ocx_package` 10, `ocx_oci` 9, `ocx_util` 8, `ocx_package_manager` 6, `ocx_project` 4, `ocx_announce` 3, `ocx_cli` 2, `ocx_setup` 2, `ocx_sign` 2, `ocx_store` 2, `ocx_shell` 1, `ocx_index` 1, plus **1 in `error.rs`** (dissolves — `DISSOLVE` bucket, not attributable to a surviving crate).

**`ClassifyErrorKind`: 4 total (ADR baseline: 3).** `ocx_sign` 2 (`oci/sign/error.rs`, `oci/verify/error.rs` — matches the ADR's cited 2 of 3 exactly), `ocx_package` 1 (`publisher/copy.rs` — matches the ADR's 3rd exactly), **`ocx_announce` 1 (`claim/error.rs` — new, from the `claim` module absent at ADR-authoring time)**.

**`try_downcast!` entries in `cli/classify.rs`: 59 (ADR baseline: 55).**

All three counts drifted upward by roughly the same proportion as the file map's own file/LOC drift (§7 of `discover_crate_split_file_map.md`, +25% LOC) — consistent with ordinary feature work landing in the ~10-day gap between the dossier's numbers and this run, not a methodology disagreement.

## 8. Caveats

1. **Not a real Rust parser.** Comment/string stripping is a single-pass state machine (`code`/`string`/`line_comment`/`block_comment`) that deliberately resets `"..."` string-tracking at every newline rather than carrying it across the whole file. **This was not a starting design choice — it is the fix for a real bug found and corrected during this run**: an earlier two-pass version (block-comments stripped globally, then line-comments) tracked `"` continuously across an entire file, and a single doc-comment line with an odd literal-quote count desynced it; the very first stray `/*`-look-alike afterward then looked "unterminated" and silently swallowed the rest of the file as a comment. Observed and fixed on two files: `crates/ocx_lib/src/codesign.rs` (a naive, non-string-aware brace matcher used for `mod tests { ... }` detection) and, far more seriously, `crates/ocx_lib/src/trust.rs`, where a raw-string test fixture — `` r#"scope = { includ = ["ghcr.io/acme/*"] }"# `` — collapsed 156,784 raw bytes to 10,145 cleaned bytes (94% of the file silently discarded) before this was caught by a whole-corpus `cleaned/raw` ratio sanity sweep. The current version also gives raw strings (`r"..."`, `r#"..."#`, `br"..."`) a dedicated `#`-counted opener/closer scan rather than treating their content as an ordinary string. Residual gap: nested `/* */` block comments (Rust allows nesting; this script does not) and a raw-string prefix appearing inside another raw string's content (pathological, not observed).
2. **Post-fix corpus sweep**: every file's `cleaned/raw` byte-length ratio was checked; the only two below 15% (`project/internal.rs` at 9.3%, `package_manager/tasks/garbage_collection/project_roots.rs` at 12.5%) were manually confirmed to be genuinely doc-comment-heavy short files, not further instances of the bug above.
3. **`use`-tree bracket expansion** is a hand-rolled recursive descent over `{...}` groups (handles `crate::a::{b, c::{d}}`, `as` aliases, `*` globs) — not a full grammar; a macro-generated `use` (none observed in this tree) would not be recognised. A related bug found and fixed during this run: the group-prefix (text before `{`) was not being re-split on `::`, so `use crate::oci::{Digest, Identifier};` silently produced zero references instead of two — this affected every multi-segment group import in the corpus before the fix (a very common pattern) and was caught by manually verifying `cli/theme.rs`'s known `oci::{Digest, Identifier}` import against the tool's output.
4. **`super::` resolution** tracks inline `mod NAME { ... }` nesting (the ubiquitous `#[cfg(test)] mod tests { use super::*; }` pattern) via brace-matching on the comment-stripped text, so `super` inside a test module resolves to the *file's own* module, not its parent — this was also a bug found and fixed (an earlier version always resolved `super` one level too high). Residual: 2 of the corpus's several hundred `super::*` glob imports still resolve to a bare, module-less reference (`activate.rs:218`, `trust.rs:1509`), both reported as `UNRESOLVED` rather than silently miscounted — a strictly bounded, visible failure mode, not a silent one.
5. **Split files** (`env.rs`, `cli/progress.rs`, `cli/theme.rs`, `package/tag.rs`) are attributed to their file map *default* (majority) crate for the FROM side; the TO side (e.g. `crate::env::apply_entries`) is classified via an explicit item-name table taken from `discover_crate_split_file_map.md` §3, not by re-deriving line ranges.
6. **A `crate::prelude::*` glob import** cannot tell which of the 4 prelude items are actually used at that call site; attributed to `ocx_util` (3 of 4 non-dissolved items) with a `prelude_glob_approx` tag — **13 such glob imports** counted separately, not folded into any crate's disallowed total.
7. **`crate::cli::clap` and `crate::cli::error`** (whole modules, not the classification ladder) are file-map-assigned to `ocx_cli` (`discover_crate_split_file_map.md` §2, "[assumed]" rows) but this script's reference classifier buckets *references* to unlisted `cli::` sub-paths as `ocx_console` per the plan's literal instruction ("other `crate::cli::` → `ocx_console`"). A handful of edges the file map would attribute to `ocx_cli` are therefore folded into `ocx_console` counts instead (e.g. `cli/classify.rs:111`'s two `cli::error::{MetadataResolutionError,UsageError}` references).
8. **Verification method for §4**: every named ADR site was re-checked with `command grep -nE` against the exact identifier and then hand-read for doc-comment context, because this codebase's rustdoc convention (`` [`x`](crate::a::b::x) ``) makes naive `grep` systematically overcount — a majority of the raw textual hits for several §4 rows (e.g. all 6 of `trust.rs`'s `config`/`managed_config` mentions, all 5 of `config.rs`'s `project` mentions) turned out to be doc-comment cross-references, not code. The script's own comment-stripping already excludes these; the manual re-check was to build confidence in the stripper after finding bugs 1–4 above, not because the script's own numbers were in doubt going into §4.
9. **`unmapped_files: 0`** — every file in `discover_crate_split_file_map.md`'s Section 2 table and every file walked from disk matched 1:1 (388 rows, 388 files), so the file map is current against this exact HEAD.
