# Impl Map — Infrastructure Patches

Companion to [`adr_infrastructure_patches.md`](./adr_infrastructure_patches.md). Grounding pass (2026-06-20) confirmed **all ADR anchors accurate — no drift**. This file pins the exact signatures builders need; for everything else read the ADR §Implementation Plan + the cited source.

## Confirmed live signatures

| Symbol | Location | Signature / note |
|---|---|---|
| `compose` | `crates/ocx_lib/src/package_manager/composer.rs:66` | `compose(roots, store, self_view) -> Vec<Entry>`; surface-gates each TC entry via `has_interface()`/`has_private()`; emits interface vars then root vars then entrypoint synth-PATH. **Phase 4 adds `&site_patches` param + Phase-2 overlay.** |
| `Entry` | `crate::package::metadata::env::entry::Entry` | The emitted env-var representation (`Modifier::Constant` replace / `Modifier::Path` prepend, last-wins). |
| `resolve_env` | `crates/ocx_lib/src/package_manager/tasks/resolve.rs:288` | `async fn resolve_env(&self, packages: &[Arc<InstallInfo>], self_view: bool) -> Result<Vec<Entry>>`. **Builds `SitePatchSet` here (Phase 4), threads into compose.** |
| `MirrorConfig` / `resolve_mirror_map` | `crates/ocx_lib/src/config/mirror.rs` + `lib.rs` re-export | Precedent for `PatchConfig` + `resolve_patch_config`. Copy structure exactly. |
| `OcxConfigView` / `apply_ocx_config` | `crates/ocx_lib/src/env.rs` | Add `patches: Option<PatchConfig>` + `OCX_PATCHES` JSON forwarding (mirror `OCX_MIRRORS`). `OCX_INDEX` selector pattern → `OCX_PATCH_SNAPSHOT`. |
| `to_relaxed_slug` | `crates/ocx_lib/src/utility/string_ext.rs:31` | `fn to_relaxed_slug(&self) -> String`; keeps `[a-zA-Z0-9._-]`, rest → `_`. Path-template slugify. |
| `Identifier` Display | `crates/ocx_lib/src/oci/identifier.rs:264` | Canonical `registry/repository:tag[@digest]` string the unified matcher globs over. |
| `pull_description` / `push_description` | `crates/ocx_lib/src/oci/client.rs` (~1030 / 943-1024) | `pull_description` reads layer blobs to memory, **does not persist** — Phase 2 persists via `BlobStore::write_blob`. `push_description` = template for `__ocx.patch` publish. |
| `ManifestBuilder::artifact_type` | `crates/ocx_lib/src/oci/manifest_builder.rs` | Build descriptor manifest: `artifact_type("application/vnd.sh.ocx.patch.v1")` + single layer `…descriptor.v1+json` + empty `{}` config. |
| `ReachabilityGraph::build` | `…/tasks/garbage_collection/reachability_graph.rs:71` | `build(file_structure, project_roots)` today → Phase 5 `build(file_structure, project_roots, patch_roots)`. |
| `collect_project_roots` | `…/tasks/clean.rs:214` | Config-free (symlink store). Phase 5: `clean` also builds `SitePatchSet` (has Context/config) → `patch_roots`. |
| `Tag::is_internal_str` | `crates/ocx_lib/src/package/tag.rs` | `__ocx.` prefix → `__ocx.patch` auto-hidden, zero work. |
| `Publisher::push` | `crates/ocx_lib/src/publisher.rs:24-137` | Companion push + cascade for `ocx patch publish`. |
| `package test` pipeline | `crates/ocx_cli/src/command/package_test.rs:98-202` | materialize→compose→exec; reuse for `ocx patch test`. |
| `expect_module` / `ScriptOutcome` | `crates/ocx_lib/src/script/expect_module.rs`, `script.rs:64` | Starlark `expect.*` assertions for maintainer test. |
| `index update` | `crates/ocx_cli/src/command/index_update.rs` | Piggyback target for `ocx patch sync`. |
| launcher exec | `crates/ocx_cli/src/command/launcher/exec.rs:54` | calls `resolve_env(&[pkg], self_view=true)` — entry-point inheritance path (C2/C5). |
| `glob` dep | absent | Confirmed NOT a dependency → custom ~20-line flat matcher (spans `/`/`:`/`@`), no new dep. |
| tripwire test | `crates/ocx_lib/src/config.rs` | `parse_unknown_future_patches_section_silently_ignored` → flip to positive parse tests in Phase 1. |

## Media types

- Manifest `artifactType`: `application/vnd.sh.ocx.patch.v1`
- Descriptor layer: `application/vnd.sh.ocx.patch.descriptor.v1+json`

## New surfaces (net-new code)

- `PatchConfig` / `resolve_patch_config` (config tier).
- `PatchDescriptor { version: u32, rules: Vec<PatchRule> }`, `PatchRule { match_pattern, packages: Vec<Identifier>, required: Option<bool> }`.
- `glob_match(pattern, identifier) -> bool` (flat matcher).
- `SitePatchResolver` → `SitePatchSet` (manager/Context layer).
- `ocx patch {sync,freeze,publish,test}` command group.
- `patches.snapshot.json` (freeze) + `OCX_PATCH_SNAPSHOT` selector.
