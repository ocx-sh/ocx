# Research: Repo archaeology — relocatability, launchers, index snapshot, rules_ocx store model

## Metadata

**Date:** 2026-11-08
**Domain:** packaging
**Triggered by:** `.agents/discussions/bazel-hermetic-package-bundles.md` (repo-archaeology lane)
**Expires:** 2027-02-08 (code-anchored; re-verify lines before citing)

## Direct Answer

[ocx-sh/ocx#23](https://github.com/ocx-sh/ocx/issues/23) (relative symlinks) was never implemented — closed 2026-08-09: "deprecated, we opted for hard-links, requires dedicated import/export command". Every install and ref symlink is absolute; launchers bake absolute `pkg_root`. A relocatable home subset is already documented: `blobs/` (manifests), `layers/` (extracted archives), `index/`; `packages/` is reassembled offline. The home keeps no compressed layer blobs. The index snapshot is already a portable plain directory.

## Relocatability in ocx

- #23 close comment: <https://github.com/ocx-sh/ocx/issues/23#issuecomment-5232481641>; hard-link design = `.claude/artifacts/adr_three_tier_cas_storage.md:597` ("Revised D6", 2026-04-10). #25 still lists #23 as dependency; #99 is auto-verify, unrelated.
- `packages/…/content/` = hard-link assembly of `layers/` (`crates/ocx_store/src/hardlink.rs`); cross-volume → `CrossesDevices`.
- Absolute symlinks: `symlink::update/create` pass targets unchanged (`crates/ocx_util/src/fs/symlink.rs:87-120`); candidate/current + `refs/symlinks/` back-ref via `ReferenceManager::link` (`crates/ocx_store/src/reference_manager.rs:41-110`); `refs/deps/` (`:~190`), `refs/blobs/` (`:229-270`), `refs/layers/` (`crates/ocx_package_manager/src/tasks/pull.rs:883-905`) all absolute.
- Back-ref names = first 16 hex of SHA-256 over the absolute forward path (`reference_manager.rs:23-26`) — a moved home's back-refs no longer match (inferred: `broken_refs`/GC would flag them).
- `test_zip_and_move_warm_home_offline_identical` (`test/tests/test_managed_config.py:751`) asserts only managed-config registry resolution offline — not packages, links or launchers.
- Documented relocatable subset: `website/src/content/docs/guides/work-offline.mdx:55-63`, `explanation/indices.md:107` — `blobs/` (manifests), `layers/` (extracted archives), index; `ocx package install` rebuilds `packages/` offline. Missing content fails closed (exit 81 under `--offline`).
- Contradiction: `analysis_issue_triage_2026-08-29.md:140` claims "prerequisite #23 landed" — false per the close comment.

## Launchers

- Unix `.sh` entrypoint bakes absolute `pkg_root` (`crates/ocx_package_manager/src/launcher/body.rs:15-22`, computed post-rename `tasks/pull.rs:452-458`): `exec "${OCX_BINARY_PIN:-ocx}" launcher exec '<pkg_root>' -- "${0##*/}" "$@"`. `OCX_BINARY_PIN` read at run time.
- Windows: hard-linked `ocx-shim-*.exe` (`shim_bin_store.rs`) + `<name>.shim` sidecar with the same absolute `pkg_root` (`crates/ocx_shim/src/core.rs:46-58`).
- Lazy shim bakes the pinned identifier, not a path (`body.rs:~30`).
- Toolchain trampoline (`<home>/toolchain/active/bin/<name>`, `body.rs:60-96`) bakes `--project '<abs root>'` or `--global` and `__ocx_binary='<abs ocx>'` (`generate.rs:175-200`). `OCX_BINARY_PIN` value: `generate.rs:82-130`.
- `OCX_HOME` itself is not baked into ocx entrypoints (rules_ocx's eager launchers do bake it).

## `adr_three_tier_cas_storage.md`

- `:51` OCI Image Layout surveyed; `:497` rejected only as a sharding option (D11). Never decides to emit/consume a layout.
- `:559-561` "Import/Export Consideration" — a constraint: hard links survive `tar`/`cp -a`/`rsync -H`; `refs/layers|blobs|deps` need `-L` or separate archival for a single-package export.
- `:568` "Hardlinks survive `mv $OCX_HOME /new/path`" — unchecked.
- `research_oci_metadata_cache_tech.md:25` — `ocidir` crate "Not recommended": OCX blobs are registry-namespaced; single-repo OCI layout does not model that.
- Open per `plan_issue_batches_2026-09-04.md:2040-2075`: archive format, verb placement.

## Index snapshot

- Plain tree, no symlinks, no absolute paths (`explanation/local-index.md:~55`): `config.json`, `c/index.json`, `p/<ns>/<pkg>.json`, `p/<ns>/<pkg>/o/sha256/<hex>.json`, per source under `index/<registry>/`; same wire format as index.ocx.sh.
- Produced by `ocx index update`, `ocx index sync` (`crates/ocx_cli/src/command/index_sync.rs`), `ocx index regenerate` — merge-only. Consumed via `--index <dir>` / `OCX_INDEX`; `--frozen` → exit 81 on unknown tag.
- Holds root documents and dispatch objects only — no manifests/layers. `adr_servable_index_snapshot.md` (Accepted 2026-08-09) adds `file://` index URLs; no blob bundling.

## rules_ocx store model

- Shared absolute store chosen in the first extension commit `02ba1a7` (2026-07-01): unsandboxed repo rules, store "fetched once per machine", launchers reference absolute paths "as nixpkgs does", RBE "a non-goal" (`docs/concepts/how-it-works.md:20-30`, `README.md:270`, `AGENTS.md:36`). No separate ADR.
- `isolated_home` also in `02ba1a7`: store inside the repo; costs a per-repo re-download, drops `$OCX_HOME`-rooted config tiers and the sigstore trusted-root rung; offline "fails outright" — no embedded trust root (`AGENTS.md:207-212`, `docs/concepts/trust-and-config.md:83`).
- `bins` incompatible with `isolated_home` (`ocx/private/package.bzl:60-62`, commit `5423507`): "the store must be resolvable on whatever machine executes the launcher". `bins` also forbids `index`, requires digest pins; Windows lazy launcher bakes absolute paths.
- Air-gap docs: `docs/guides/offline-mirror.md` = mirrors only (`OCX_INSTALL_DIST_URL`, `OCX_INSTALL_MIRROR_URL`, `OCX_MIRRORS`, auth/CA/insecure); `index = "//:index"` snapshot support. No shipped/exported store documented.

## Platforms

- `ocx.lock` `lock_version = 3` (commit `9978cc22b`, closes #212): per `[[tool]]` a `[tool.platforms]` map canonical key → per-platform manifest digest; root `ocx.lock` shows up to six platforms per tool.
- `--platform`/`-p` (`crates/ocx_cli/src/options/platforms.rs`) single-valued, host default; a repeat is a usage error (`adr_platform_model_unification.md` D3/D4). Present on `pull`, `lock`, `env`; not found on `add`, `upgrade`, `exec`.
- The flag selects what to materialize/render, not what the lock records (`lock.rs:83-109`). #138 (closed) predates lock v3 and asked for a repeatable flag; the shipped flag is single-valued.

## negative

- No `ocx export` in `crates/` or docs. No rules_ocx doc on copying a store or RBE beyond "non-goal".

## leads

- `crates/ocx_util/src/archive` tar/zip backends; `ocx package create` stores hard links once (commit `e88de45a9`).
- `state/projects/<key>` keyed by 16-hex of the canonical project dir — machine-path-specific; check for the move case.
- Throwaway-home experiment to settle whether `ocx clean` treats moved absolute refs as broken.
