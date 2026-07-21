# Plan: inspect closure reshape + read-only index fix

## Status
- **State:** in-progress — Phase 1 DONE (green); Phase 2 Rust DONE (green); docs + pytest delegated to sonnet subagents (running); rules DONE.
- **Branch:** feat/inspect-binaries-closure
- **Owner decisions (2026-07-20):** locked via dialogue
- **Green so far:** ocx_lib 2866 unit, ocx bins 295 unit, clippy clean; `test_inspect_no_index_growth.py` passes; live render + index-empty verified.

## Decisions (owner-confirmed)
1. Flag: `--deps` → `--closure` (drop `--deps`, no alias/vestige — pre-1.0).
2. JSON: nest under one `closure` object — `closure: { deps: [TCO nodes + visibility], surface: { interface, private } }`.
3. Plain tree: flat transitive-closure-order list + visibility (no nested re-tree, no root repetition).
4. Bug: read-only `inspect` must NOT grow the permanent `OCX_INDEX`. Warm blobs + resolve content-addressed (index → blobs → source). GC contract confirmed: **index permanent, blobs GC-able**.

## Phase 1 — Bugfix: read-only inspect never writes the local index (`fix:`)

RCA: `inspect` resolves via `IndexOperation::Resolve` → `ChainedIndex::fetch_and_persist_chain`, which (a) `persist_dispatch` writes the image-index dispatch object to `o/`, (b) grows the root doc/tag. `suppress_tag_commit` only gates (b); the dispatch write and the `recover_absent_leaf` self-heal write are ungated.

Index-write sites in the resolve path (all must be gated for read-only):
- `chained_index.rs:291` — `recover_absent_leaf` self-heal `stage_dispatch_bytes`
- `chained_index.rs:452` — `persist_dispatch` (dispatch object)
- `chained_index.rs:472-490` — root growth (`commit_root_tag` / `persist_published_root`)

Design: replace `ChainedIndex.suppress_tag_commit: bool` with `write_policy: LocalWritePolicy`:
- `Full` — dispatch object + tag pointer (default resolve; `new`)
- `NoTag` — dispatch object, no tag (update family; `new_lock_scoped`)
- `ReadOnly` — nothing (inspect; via `read_only_view`)

Wiring:
- `LocalIndex::fetch_dispatch_only` — persist_dispatch minus the stage write (fetch + decode + return).
- `ChainedIndex`: gate the 3 sites on `write_policy`; carry it through `box_clone`.
- `IndexImpl::read_only_view(&self) -> Box<dyn IndexImpl>` default = `box_clone`; `ChainedIndex` override flips policy to `ReadOnly`.
- `Index::read_only_view(&self) -> Index`.
- `PackageManager::read_only_index()` → `self.index().read_only_view()`.
- `inspect.rs`: route root fetch + closure `fetch_manifest`/`select` through `read_only_index()`. Blob writes (`stage_leaf_manifest`, `fetch_blob` config) stay — blobs are the GC-able cache (desired).

Tests:
- pytest (fails on current code): `inspect` (plain + `--closure`) leaves `OCX_INDEX` empty; leaf manifests only in blobs.
- Rust unit: `ChainedIndex` ReadOnly resolves but writes nothing to the local index (dispatch + tag).
- Rewrite `test_inspect_deps_offline_after_warm_matches_online`: offline tag now needs prior `ocx index update` (or use digest refs); blob-warmed digest content still resolves offline.

Docs: `subsystem-oci.md` routing/write-path note (add ReadOnly row).

## Phase 2 — Reshape: --closure + closure:{deps,surface:{interface,private}} + flat tree (`feat!`/`refactor`)

Lib (`inspect.rs`):
- Extract `Surface { binaries, entrypoints, env, binaries_complete }`.
- `InspectClosure { nodes (TCO), interface: Surface, private: Surface, conflicts }`.
- `ClosureNode` carries its full env (`key, kind, visibility`); aggregates filter per-axis (`has_interface` / `has_private`).
- Shared aggregate fn parameterized by admission + env-visibility predicate; interface = has_interface, private = has_private.
- Rename `InterfaceEnvVar` → neutral (`SurfaceEnvVar`).
- `InspectOptions.deps` → `.closure`.

Wire (`api/data/package_inspect.rs`):
- Top-level `closure: { deps: [...], surface: { interface, private } }` (was flat `closure[]` + `interface_surface`).
- `deps` = non-root nodes in TCO order, each `{ identifier, digest, effective_visibility, binaries, entrypoints, env, dependencies }`.
- Flat plain render: `closure` branch lists each dep once (TCO) with visibility; `surface` branch with `interface` + `private` sub-branches. No root repetition.

CLI (`command/package_inspect.rs`): rename flag `--deps` → `--closure`; help text.

Docs: `website/src/docs/reference/command-line.md`, `test/manual/README.md`.
Rules: `subsystem-cli-commands.md`, `subsystem-package-manager.md`.
Tests: rename/rewrite `test/tests/test_package_inspect_deps.py` → new shape; Rust unit tests in `package_inspect.rs`.

## Verification
`task verify` green; 22 inspect acceptance tests updated; clippy clean; rebuild `test/bin/ocx` before pytest.
