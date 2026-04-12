# Plan: Multi-Layer Packages (Issue #20)

<!--
Implementation Plan
Filename: artifacts/plan_multi_layer_packages.md
Owner: Builder (/builder)
Handoff to: Builder (/builder), QA Engineer (/qa-engineer)
Related Skills: builder, qa-engineer, architect
-->

## Overview

**Status:** Implemented (with post-review revision: positional `Vec<LayerRef>`, no `--layer` flag, no backward compat for `content`, user guide documentation added)
**Author:** Claude (swarm-plan)
**Date:** 2026-04-12
**Scope:** Medium (1-2 weeks) | One-Way Door (Medium)
**GitHub Issue:** ocx-sh/ocx#20 (Supporting Layers)
**Related PR:** ocx-sh/ocx#22 (Multi-Layer Packages design)
**Related ADR:** [adr_three_tier_cas_storage.md](./adr_three_tier_cas_storage.md)
**Depends on:** [plan_multi_layer_assembly.md](./plan_multi_layer_assembly.md) (completed)

## Objective

Complete multi-layer package support across the full lifecycle: push (create + publish multi-layer packages to registries) and pull (install packages with multiple layers). The core assembly walker is already implemented; this plan covers the remaining pipeline-level integration, CLI refactoring, and mirror adaptation.

## Scope

### In Scope

- **Pull-side**: Remove the `manifest.layers.len() > 1` guard in `pull.rs` and switch to multi-layer assembly
- **`LayerRef` type**: New enum representing a layer as either a file path (upload) or an OCI digest (reuse existing), living in the `publisher` module
- **Publisher refactoring**: `push_package` (and its internal `push_image_manifest` helper) refactored to accept `&[LayerRef]`, uploads new layers, verifies digest-referenced layers exist
- **CLI `package push`**: Accept multiple layers via `--layer` flag(s), each parsed as `LayerRef`
- **Mirror adaptation**: Update mirror pipeline to use the new Publisher API (single-layer backward compat)
- **Acceptance tests**: Multi-layer push + pull round-trip, layer reuse via digest, overlap detection

### Out of Scope

- **Mirror multi-layer config**: Defining multiple layers in mirror YAML specs (future enhancement)
- **Layer optimization tool**: Automated dedup analysis across package variants (mentioned in issue #20 "further notes")
- **Layer shadowing / whiteouts**: Only overlap-free layers; OCI changeset semantics remain unsupported
- **`package create` changes**: `package create` continues to bundle a single directory into one archive; multi-layer assembly happens at push time
- **Dependency cross-layer ordering**: Layer ordering within a package is manifest-order; cross-package dependencies are orthogonal

## Research

**Research artifact:** [research_oci_layers_and_composition.md](./research_oci_layers_and_composition.md) (from PR #22), [research_content_addressed_storage.md](./research_content_addressed_storage.md) (from ADR)

**Key findings from prior research:**
- Per-file symlinks break `@loader_path`/`$ORIGIN` resolution; hardlinks are safe and established (pnpm, uv pattern)
- Non-overlapping subtree constraint balances dedup power with assembly correctness
- OCI wire format natively supports multi-layer manifests; no registry changes needed
- Existing `assemble_from_layers` walker is fully tested (41 tests, 10 categories)

**Push-side design insight:** ZIP/tar creation is non-deterministic (timestamps, compression entropy). If a user re-bundles the same content, they get a different digest. By allowing `LayerRef::Digest`, users can reference an already-uploaded layer by its stable OCI digest, avoiding re-upload and preserving dedup. This is the key enabler for the layer reuse workflow described in issue #20.

**OCI blob media types:** OCI distribution does not store media types per blob — the `Content-Type` on a blob HEAD is always `application/octet-stream`. Media types are manifest-descriptor metadata only. For `LayerRef::Digest`, we cannot derive the original media type from the blob. OCX's archive extraction already detects format from magic bytes, so the manifest media type is informational, not functional. We will use a standard fallback for digest-referenced layers.

## Technical Approach

### Architecture Changes

```
BEFORE (single-layer only):

  CLI: ocx package push ID FILE -p platform
    → Publisher::push(info, &Path)
      → client.push_image_manifest(info, file)  [one layer descriptor]
        → client.update_image_index(...)

  Pull: setup_impl()
    → extract_layers()     [N=1 enforced by guard]
    → assemble_from_layer(single_layer, dest)

AFTER (multi-layer):

  CLI: ocx package push ID --layer REF1 --layer REF2 -p platform
    → Publisher::push(info, &[LayerRef])
      → client.push_multi_layer_manifest(info, &[LayerRef])
        → for each LayerRef::File: upload blob, create descriptor
        → for each LayerRef::Digest: HEAD to verify + get size, create descriptor
        → build manifest with N layer descriptors
        → client.update_image_index(...)

  Pull: setup_impl()
    → extract_layers()     [N≥1, parallel, already implemented]
    → assemble_from_layers(&[all_layer_contents], dest)  [already implemented]
```

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| `LayerRef` enum with `File`/`Digest` variants | The user specified layers should be "either a path to file or a digest like the OCI digest." This maps directly to the existing `oci::Digest` type. Digest-referenced layers avoid re-upload and enable layer reuse. |
| `--layer` repeatable flag (not positional) | Clap makes repeatable flags ergonomic (`Vec<LayerRef>`). Keeps the identifier as the only positional arg. Clear intent: `--layer sha256:abc --layer ./file.tar.xz`. |
| Backward-compatible `content` positional as sugar | Keep `content` positional as optional — if provided without `--layer`, treated as `LayerRef::File(content)`. Existing scripts keep working. Mutex: `content` and `--layer` cannot both be specified. |
| HEAD for digest verification before manifest creation | Before creating the manifest, HEAD each `LayerRef::Digest` blob to verify it exists in the registry and retrieve its `Content-Length`. Fail fast with a clear error if a referenced layer is missing. |
| Standard media type fallback for digest layers | Use `application/vnd.oci.image.layer.v1.tar+gzip` for digest-referenced layers. OCX's pull-side extracts by magic bytes, not media type. This is informational only. |
| Two workstreams (pull + push) in parallel | Pull-side is a 3-line change plus tests. Push-side is a larger refactoring. They are independent and can be developed and tested in parallel. |
| Mirror gets single-layer wrapper, not multi-layer config | Mirror adaptation wraps the existing single bundle path in `vec![LayerRef::File(path)]`. Multi-layer mirror config is a separate future enhancement. |

## Component Contracts

### `LayerRef` (`crates/ocx_lib/src/publisher/layer_ref.rs`)

```rust
/// A reference to a layer in a multi-layer package.
///
/// Layers are ordered: index 0 is the base layer, index N is the top layer.
/// With overlap-free semantics, order doesn't affect the assembled result,
/// but it determines error messages and manifest descriptor order.
#[derive(Debug, Clone)]
pub enum LayerRef {
    /// An archive file to upload as a new layer.
    File(PathBuf),
    /// An existing layer already present in the registry, referenced by digest.
    /// The layer blob must exist in the target registry — verified before
    /// manifest creation via a HEAD request.
    Digest(oci::Digest),
}
```

**Parsing from CLI string (`FromStr` / `TryFrom<&str>`):**
- Calls `oci::Digest::try_from(s)`. If it returns `Ok(digest)`, produce `LayerRef::Digest(digest)`.
- If `Digest::try_from` returns `Err(DigestError::Invalid(_))` — regardless of whether the prefix looked like an algorithm — treat the entire string as a file path → `LayerRef::File(PathBuf::from(s))`.
- This means `"sha256:tooshort"` (valid algorithm prefix but invalid hex length) becomes `LayerRef::File("sha256:tooshort")`, not an error. File existence is validated at execution time, not parse time.

**Display:** `LayerRef::File(p)` displays as the path; `LayerRef::Digest(d)` displays as the digest string.

### `Publisher::push` (refactored)

```rust
/// Push a package with one or more layers to the registry.
///
/// Each `LayerRef::File` is uploaded as a new blob. Each `LayerRef::Digest`
/// is verified to exist via HEAD. The manifest contains one descriptor per
/// layer in the order provided.
pub async fn push(&self, info: Info, layers: &[LayerRef]) -> Result<()>

/// Push a package with cascade tag management.
pub async fn push_cascade(&self, info: Info, layers: &[LayerRef], existing_versions: BTreeSet<Version>) -> Result<()>
```

**Error cases:**
- Empty `layers` slice → error (package must have at least one layer)
- `LayerRef::File` path doesn't exist → IO error
- `LayerRef::File` unsupported archive format → `ClientError::InvalidManifest`
- `LayerRef::Digest` blob not found in registry → `ClientError::BlobNotFound` (new variant)
- Registry push failure → existing `ClientError` variants

### `Client::push_multi_layer_manifest` (new internal method)

```rust
/// Pushes config blob + N layer blobs + image manifest.
///
/// For `LayerRef::File` layers: reads file, computes digest, uploads blob.
/// For `LayerRef::Digest` layers: HEADs blob to verify existence and get size.
/// Returns the manifest, its serialized bytes, and its SHA-256 digest string.
pub(crate) async fn push_multi_layer_manifest(
    &self,
    package_info: &Info,
    layers: &[LayerRef],
) -> std::result::Result<(oci::ImageManifest, Vec<u8>, String), ClientError>
```

**Layer descriptor construction:**
- `LayerRef::File(path)`: `media_type` from `media_type_from_path(path)`, `digest` from SHA-256 of file content, `size` from file length (as `u64`)
- `LayerRef::Digest(d)`: `media_type` = `MEDIA_TYPE_LAYER_TAR_GZIP` (standard fallback — see deferred finding D1 below), `digest` = `d.to_string()`, `size` from HEAD `Content-Length` (as `u64`)

### `Client::head_blob` (new method)

```rust
/// HEAD a blob by digest to verify existence and retrieve size.
///
/// Returns `(content_length, content_type)` if the blob exists.
/// Returns `Err(ClientError::BlobNotFound)` if the blob does not exist.
pub(crate) async fn head_blob(
    &self,
    identifier: &oci::Identifier,
    digest: &oci::Digest,
) -> std::result::Result<BlobHead, ClientError>

pub(crate) struct BlobHead {
    pub size: u64,
}
```

### CLI `PackagePush` (refactored)

```rust
#[derive(Parser)]
#[clap(group = clap::ArgGroup::new("layer_input").required(true))]
pub struct PackagePush {
    #[clap(long = "cascade", short = 'c')]
    cascade: bool,

    #[clap(long = "new", short = 'n')]
    new: bool,

    #[clap(short, long)]
    metadata: Option<std::path::PathBuf>,

    #[clap(short, long, required = true)]
    platform: oci::Platform,

    identifier: options::Identifier,

    /// Archive file to push as a single layer (backward-compatible shorthand).
    /// Mutually exclusive with --layer.
    #[clap(group = "layer_input")]
    content: Option<std::path::PathBuf>,

    /// Layer reference: file path or OCI digest (sha256:...).
    /// Can be repeated for multi-layer packages. Order is preserved.
    /// Mutually exclusive with positional content argument.
    #[clap(long = "layer", group = "layer_input")]
    layers: Vec<String>,
}
```

**Behavior:**
- If `content` is provided: `vec![LayerRef::File(content)]`
- If `layers` is provided: parse each string → `Vec<LayerRef>`
- Neither provided: clap `ArgGroup("layer_input").required(true)` rejects with clear error message
- Both provided: clap `ArgGroup` enforces mutual exclusion

### Pull pipeline changes (`pull.rs`)

```rust
// REMOVE: lines 272-277 (multi-layer guard)

// REPLACE: lines 318-326 (single-layer assembly)
// FROM:
let layer_content = fs.layers.content(pinned.registry(), &layer_digests[0]);
crate::utility::fs::assemble_from_layer(&layer_content, &pkg.content()).await...

// TO:
let layer_contents: Vec<PathBuf> = layer_digests.iter()
    .map(|d| fs.layers.content(pinned.registry(), d))
    .collect();
let sources: Vec<&Path> = layer_contents.iter().map(AsRef::as_ref).collect();
crate::utility::fs::assemble_from_layers(&sources, &pkg.content()).await...
```

### Mirror pipeline changes

```rust
// push.rs: push_and_cascade — change Publisher::push signature
// Before: publisher.push(info, bundle_path).await?;
// After:  publisher.push(info, &[LayerRef::File(bundle_path.to_path_buf())]).await?;

// Same for push_cascade:
// Before: publisher.push_cascade(info, bundle_path, versions).await?;
// After:  publisher.push_cascade(info, &[LayerRef::File(bundle_path.to_path_buf())], versions).await?;
```

### Cascade module changes

```rust
// cascade.rs: push_with_cascade — pass through layers
// Before: pub async fn push_with_cascade(client, info, file, versions, version)
// After:  pub async fn push_with_cascade(client, info, layers: &[LayerRef], versions, version)
```

## Implementation Steps

> **Contract-First TDD**: Stub -> Verify -> Specify -> Implement -> Review.

## Execution Model

Follows the **Swarm Workflow** from [workflow-feature.md](../../.claude/rules/workflow-feature.md) with contract-first TDD. Worker assignments per [workflow-swarm.md](../../.claude/rules/workflow-swarm.md). The Review-Fix Loop (Phase C) is the self-introspective loop where reviewer subagents find issues, builder subagents fix them on the fly, and the loop iterates until clean — bounded to 3 rounds max.

The plan has two parallel workstreams that converge in a shared acceptance test phase.

### Workstream A: Pull-Side Multi-Layer Support

**Minimal change — the assembly walker, layer extraction, and refs/layers bookkeeping already handle N layers. The only single-layer assumption left is the guard and the assembly call site.**

#### Phase A1: Implement (no stubs needed)

**Worker:** `worker-builder` (focus: `implementation`)

- [ ] **Step A1.1:** Remove multi-layer guard in `pull.rs`
  - File: `crates/ocx_lib/src/package_manager/tasks/pull.rs`
  - Remove the guard block at ~line 272 (`if manifest.layers.len() > 1 { ... }`) and its preceding comment (~lines 265-271)
  - Note: `extract_layers()` (~line 520) already handles N layers in parallel; `link_layers_in_temp()` (~line 733) already iterates the full `layer_digests` slice. No changes needed in those functions.

- [ ] **Step A1.2:** Switch to multi-layer assembly in `pull.rs`
  - File: `crates/ocx_lib/src/package_manager/tasks/pull.rs`
  - Replace the single-layer assembly block (~lines 318-326: the comment and `assemble_from_layer(&layer_digests[0])` call) with multi-layer assembly:
    ```rust
    let layer_contents: Vec<PathBuf> = layer_digests.iter()
        .map(|d| fs.layers.content(pinned.registry(), d))
        .collect();
    let sources: Vec<&Path> = layer_contents.iter().map(AsRef::as_ref).collect();
    crate::utility::fs::assemble_from_layers(&sources, &pkg.content())
        .await
        .map_err(PackageErrorKind::Internal)?;
    ```

**Gate:** `cargo check` passes. Existing tests pass (`task rust:verify`).

#### Phase A2: Specify (acceptance tests for multi-layer pull)

**Worker:** `worker-tester` (focus: `specification`)

Tests in `test/tests/test_install.py` (or new `test_multi_layer.py`).

| Test | Invariant |
|------|-----------|
| `test_install_multi_layer_package` | Push a 2-layer package to local registry, install it, verify both layers' files present in content dir |
| `test_install_multi_layer_shared_directory` | Push a 2-layer package where layers share a directory (e.g., `bin/`), install, verify merged content |
| `test_install_multi_layer_overlap_fails` | Push a 2-layer package with overlapping files, install fails with clear error |
| `test_install_single_layer_still_works` | Existing single-layer install is unaffected (regression) |
| `test_exec_multi_layer_package` | Push 2-layer package, exec a binary from layer B that loads a library from layer A |

**Note:** These tests require the push-side to be implemented first (Workstream B), OR they can use a pre-built multi-layer OCI artifact pushed directly via `oras` or the OCI client. The simpler approach is to make these tests depend on Workstream B completion and test the full round-trip.

**Gate:** Tests compile and are skipped/marked pending until push-side is ready.

### Workstream B: Push-Side Multi-Layer Support

#### Phase B1: Stub

**Worker:** `worker-builder` (focus: `stubbing`)

- [ ] **Step B1.1:** Create `LayerRef` type
  - File: `crates/ocx_lib/src/publisher/layer_ref.rs`
  - Public API: `LayerRef` enum (File/Digest), `Display`, `FromStr` (for CLI parsing)
  - Refactor `publisher.rs` into `publisher/mod.rs` + `publisher/layer_ref.rs` (if currently a single file), or add `layer_ref.rs` as a submodule
  - Re-export `LayerRef` from the `publisher` module
  - **Rationale:** `LayerRef` is a push-side concept consumed by `Publisher` and `Client`. It has no relationship to the `package` module (metadata, cascade, version). Placing it in `publisher` respects SRP and the existing module boundaries.

- [ ] **Step B1.2:** Add `BlobHead` struct and `head_blob` stub to Client
  - File: `crates/ocx_lib/src/oci/client.rs`
  - Public API: `pub(crate) struct BlobHead { pub size: u64 }`, `pub(crate) async fn head_blob(&self, identifier: &oci::Identifier, digest: &oci::Digest) -> Result<BlobHead, ClientError>`
  - Body: `unimplemented!()`

- [ ] **Step B1.3:** Add `BlobNotFound` variant to `ClientError`
  - File: `crates/ocx_lib/src/oci/client/error.rs`
  - Add: `BlobNotFound { registry: String, digest: String }` with appropriate Display

- [ ] **Step B1.4:** Stub `push_multi_layer_manifest` on Client
  - File: `crates/ocx_lib/src/oci/client.rs`
  - Public API: `pub(crate) async fn push_multi_layer_manifest(&self, package_info: &Info, layers: &[LayerRef]) -> Result<(oci::ImageManifest, Vec<u8>, String), ClientError>`
  - Body: `unimplemented!()`

- [ ] **Step B1.5:** Update `Publisher` signatures
  - File: `crates/ocx_lib/src/publisher.rs`
  - Change: `push(info, file: &Path)` → `push(info, layers: &[LayerRef])`
  - Change: `push_cascade(info, file: &Path, versions)` → `push_cascade(info, layers: &[LayerRef], versions)`
  - Bodies: `unimplemented!()` (temporarily breaks existing callers — fix in B1.6)

- [ ] **Step B1.6:** Update cascade module signature
  - File: `crates/ocx_lib/src/package/cascade.rs`
  - Change: `push_with_cascade(client, info, file, versions, version)` → `push_with_cascade(client, info, layers: &[LayerRef], versions, version)`
  - Body: `unimplemented!()`

- [ ] **Step B1.7:** Update CLI `PackagePush` args
  - File: `crates/ocx_cli/src/command/package_push.rs`
  - Change `content: PathBuf` to `content: Option<PathBuf>`, add `layers: Vec<String>` with `conflicts_with`
  - Stub the layer resolution logic in `execute()`

- [ ] **Step B1.8:** Update Mirror push call sites
  - File: `crates/ocx_mirror/src/pipeline/push.rs`
  - Change `publisher.push(info, bundle_path)` → `publisher.push(info, &[LayerRef::File(bundle_path.to_path_buf())])`
  - Same for `push_cascade` calls

**Gate:** `cargo check` passes (all call sites updated, stubs compile).

#### Phase B2: Architecture Review

**Worker:** `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`)

Review stubs against this design record. Verify:
- `LayerRef` enum matches documented contract (File/Digest variants, Display, FromStr)
- `Publisher` signature changes are backward-compatible (single-layer callers can wrap in slice)
- `ClientError::BlobNotFound` covers the "digest layer doesn't exist" failure mode
- CLI args handle mutual exclusion between `content` and `--layer`
- Mirror call sites correctly wrap bundle path in `LayerRef::File`
- Cascade module passes layers through without unwrapping

**Gate:** Architecture review passes.

#### Phase B3: Specify

**Worker:** `worker-tester` (focus: `specification`)

##### Unit tests (`crates/ocx_lib/src/publisher/layer_ref.rs`)

| Test | Invariant |
|------|-----------|
| `parse_file_path` | `"./archive.tar.xz"` → `LayerRef::File(PathBuf)` |
| `parse_digest_sha256` | `"sha256:abc..."` (64 hex chars) → `LayerRef::Digest(Digest::Sha256(...))` |
| `parse_digest_sha512` | `"sha512:def..."` (128 hex chars) → `LayerRef::Digest(Digest::Sha512(...))` |
| `parse_absolute_path` | `"/tmp/layer.tar.gz"` → `LayerRef::File` |
| `parse_windows_path` | `#[cfg(windows)]` — `"C:\\layers\\layer.tar.gz"` → `LayerRef::File` |
| `display_file` | `LayerRef::File` displays as path string |
| `display_digest` | `LayerRef::Digest` displays as `sha256:abc...` |
| `parse_invalid_digest_falls_back_to_file` | `"sha256:tooshort"` → `LayerRef::File("sha256:tooshort")` — `Digest::try_from` fails, entire string becomes a path |

##### Unit tests (`crates/ocx_lib/src/oci/client.rs` — push tests)

| Test | Invariant |
|------|-----------|
| `push_single_file_layer` | `push_multi_layer_manifest` with `[LayerRef::File]` uploads one blob, creates manifest with one layer descriptor |
| `push_two_file_layers` | `[File, File]` → two blob uploads, manifest with two layer descriptors in order |
| `push_digest_layer_verified` | `[Digest]` → HEAD request verifies blob, manifest uses provided digest |
| `push_mixed_file_and_digest` | `[File, Digest, File]` → upload 2, HEAD 1, manifest has 3 descriptors in order |
| `push_digest_not_found_fails` | `[Digest]` where HEAD returns 404 → `BlobNotFound` error |
| `push_empty_layers_fails` | `&[]` → error |

##### Acceptance tests (`test/tests/test_multi_layer.py`)

| Test | Invariant |
|------|-----------|
| `test_push_multi_layer_files` | Push 2-layer package from files, verify manifest has 2 layer descriptors |
| `test_push_single_layer_backward_compat` | Push single file via positional `content` arg — existing syntax works |
| `test_push_layer_flag_single` | `--layer file.tar.xz` works for single layer |
| `test_push_digest_layer_reuse` | Push layer A as part of pkg v1, then push pkg v2 with `--layer sha256:{A}` + `--layer file_B.tar.xz` |
| `test_push_digest_layer_not_found` | `--layer sha256:nonexistent` fails with clear error |
| `test_push_content_and_layer_conflict` | `ocx package push ... content.tar.xz --layer x.tar.xz` fails (mutual exclusion) |
| `test_push_no_layer_fails` | `ocx package push ID -p linux/amd64` (no content, no --layer) fails with "one of 'content' or '--layer' required" |
| `test_round_trip_multi_layer` | Push 2-layer package, install it, verify all files from both layers present |
| `test_round_trip_shared_directory` | Push 2-layer package with shared `bin/`, install, verify merged content |
| `test_round_trip_layer_overlap_fails` | Push 2-layer package with overlapping files, install fails with overlap error |
| `test_cascade_multi_layer` | Push 2-layer package with `--cascade`, verify rolling tags exist |
| `test_layer_dedup_across_packages` | Push pkg A with layers [X, Y], push pkg B with layers [X, Z] (X by digest). Verify local store has one copy of layer X |

**Gate:** Tests compile/parse and fail against stubs.

#### Phase B4: OCI Transport — `head_blob` support

**Prerequisite for B5 (implement).** `push_multi_layer_manifest` calls `head_blob` for digest-referenced layers, so the transport layer must be in place first.

The `OciTransport` trait (`crates/ocx_lib/src/oci/client/transport.rs`) may need a new `head_blob` method. If the trait doesn't already support HEAD requests:

- [ ] **Step B4.1:** Add `head_blob` to `OciTransport` trait
  - File: `crates/ocx_lib/src/oci/client/transport.rs`
  - Add: `async fn head_blob(&self, image: &Reference, digest: &str) -> Result<BlobHead, ClientError>`

- [ ] **Step B4.2:** Implement in `NativeTransport`
  - File: `crates/ocx_lib/src/oci/client/native_transport.rs`
  - Use `oci_client` HEAD blob API or raw HTTP HEAD

- [ ] **Step B4.3:** Implement in `TestTransport`
  - File: `crates/ocx_lib/src/oci/client/test_transport.rs`
  - Mock HEAD responses for unit testing

**Gate:** `cargo check` passes.

#### Phase B5: Implement

**Worker:** `worker-builder` (focus: `implementation`)

- [ ] **Step B5.1:** Implement `LayerRef` with `FromStr` and `Display`
  - File: `crates/ocx_lib/src/publisher/layer_ref.rs`
  - Parse logic: try `oci::Digest::try_from(s)` first; on any `DigestError::Invalid`, treat as file path

- [ ] **Step B5.2:** Implement `head_blob` on Client
  - File: `crates/ocx_lib/src/oci/client.rs`
  - Delegate to transport layer: HEAD request on `/v2/{repo}/blobs/{digest}`
  - Parse `Content-Length` header for size (as `u64`)
  - Map 404 → `BlobNotFound`, other errors → existing error variants

- [ ] **Step B5.3:** Implement `push_multi_layer_manifest` and refactor `push_package`
  - File: `crates/ocx_lib/src/oci/client.rs`
  - Refactor: `push_package(info: Info, file: impl AsRef<Path>)` → `push_package(info: Info, layers: &[LayerRef])`
  - The internal `push_image_manifest` helper is replaced by `push_multi_layer_manifest`:
    - For each `LayerRef::File`: read file, compute SHA-256, upload blob with progress, build descriptor
    - For each `LayerRef::Digest`: call `head_blob`, build descriptor with size from HEAD and fallback media type
    - Build manifest with all layer descriptors in order
    - Push config blob + manifest (same pattern as current `push_image_manifest`)
  - `push_package` then calls `push_multi_layer_manifest` + `update_image_index` (same flow as before)
  - Progress reporting: each `LayerRef::File` gets its own progress span (loop over file layers)
  - **Note:** Add a code comment at the media type fallback site documenting the assumption that OCX extracts by magic bytes, not media type. If OCX ever adds media-type-based routing, this becomes a latent bug.

- [ ] **Step B5.4:** Implement updated `Publisher` methods
  - File: `crates/ocx_lib/src/publisher.rs`
  - `push(info, layers)` → delegates to `client.push_package(info, layers)` (refactored to accept `&[LayerRef]`), which internally calls `push_multi_layer_manifest` then `update_image_index`
  - `push_cascade(info, layers, versions)` → same pattern as before but passing layers through

- [ ] **Step B5.5:** Implement cascade passthrough
  - File: `crates/ocx_lib/src/package/cascade.rs`
  - `push_with_cascade` passes `layers` to `client.push_multi_layer_manifest` instead of single file

- [ ] **Step B5.6:** Implement CLI layer resolution
  - File: `crates/ocx_cli/src/command/package_push.rs`
  - Resolve `content` / `layers` into `Vec<LayerRef>` per `ArgGroup` contract:
    - If `content` present: `vec![LayerRef::File(content)]`
    - If `layers` present: parse each string via `LayerRef::from_str`
    - Neither: clap rejects (ArgGroup required)
  - Validate file-type layers exist on disk before calling publisher

- [ ] **Step B5.7:** Update mirror push call sites (already done in B1.8, verify they compile)
  - File: `crates/ocx_mirror/src/pipeline/push.rs`

**Gate:** All unit tests and acceptance tests pass. `task verify` succeeds.

### Phase C: Convergence (Review-Fix Loop)

Per [workflow-feature.md](../../.claude/rules/workflow-feature.md) steps 9-11 and [workflow-swarm.md](../../.claude/rules/workflow-swarm.md) review protocol.

**Diff-scoped, bounded iterative review (max 3 rounds).** During the loop, run `task rust:verify` (subsystem gate for Rust changes) — NOT full `task verify`. Full `task verify` is the final gate before commit.

**Round 1 — all perspectives (parallel):**
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`) — design record <-> tests <-> implementation traceability
- `worker-reviewer` (focus: `quality`) — code review checklist per [quality-core.md](../../.claude/rules/quality-core.md) and [quality-rust.md](../../.claude/rules/quality-rust.md)
- `worker-reviewer` (focus: `security`) — blob verification, digest validation, no path traversal in LayerRef

Each reviewer classifies findings as:
- **Actionable** — `worker-builder` fixes automatically, then re-runs affected perspectives in Round 2
- **Deferred** — needs human judgment, surfaced in commit summary

**Round 2+ (selective):** Re-run only perspectives that had actionable findings. Loop exits when no actionable findings remain or after 3 rounds total.

**Cross-model adversarial pass** (per workflow-feature.md step 10): After the loop converges, run a single Codex adversarial review against the full diff. Actionable findings fold into one final `worker-builder` pass. Deferred findings go to the completion summary. One-shot — no looping. Skipped gracefully if Codex is unavailable. Flag: `--no-cross-model` on `/swarm-execute` to opt out.

**Commit** (per workflow-feature.md step 11): All changes committed on feature branch with conventional commit message. Deferred findings from review-fix loop and adversarial pass printed as summary. Human decides when to push.

**Gate:** No actionable findings remain. `task verify` passes on final state. Deferred findings documented.

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ocx_lib/src/publisher/layer_ref.rs` | Create | `LayerRef` enum with `FromStr`, `Display` |
| `crates/ocx_lib/src/publisher.rs` → `publisher/mod.rs` | Modify | Refactor to module dir, add `pub mod layer_ref;`, re-export `LayerRef` |
| `crates/ocx_lib/src/oci/client.rs` | Modify | Add `push_multi_layer_manifest`, `head_blob`; retire `push_image_manifest` |
| `crates/ocx_lib/src/oci/client/error.rs` | Modify | Add `BlobNotFound` variant |
| `crates/ocx_lib/src/oci/client/transport.rs` | Modify | Add `head_blob` to trait (if not present) |
| `crates/ocx_lib/src/oci/client/native_transport.rs` | Modify | Implement `head_blob` |
| `crates/ocx_lib/src/oci/client/test_transport.rs` | Modify | Mock `head_blob` |
| `crates/ocx_lib/src/publisher.rs` | Modify | Change `push`/`push_cascade` to accept `&[LayerRef]` |
| `crates/ocx_lib/src/package/cascade.rs` | Modify | Pass layers through `push_with_cascade` |
| `crates/ocx_lib/src/package_manager/tasks/pull.rs` | Modify | Remove guard, switch to multi-layer assembly |
| `crates/ocx_cli/src/command/package_push.rs` | Modify | Add `--layer` flag, resolve to `Vec<LayerRef>` |
| `crates/ocx_mirror/src/pipeline/push.rs` | Modify | Wrap bundle path in `LayerRef::File` |
| `test/tests/test_multi_layer.py` | Create | Acceptance tests for multi-layer push/pull |

## Dependencies

### Code Dependencies

No new crate dependencies. Uses existing:
- `oci::Digest` — for `LayerRef::Digest` variant
- `clap` — `conflicts_with` for mutual exclusion
- `oci_client` (patched) — may need HEAD blob support in transport

### Transport Layer Dependency

Check whether `oci_client`'s transport already supports HEAD blob requests. If not, this requires a patch to `external/rust-oci-client`. This is the only potential blocker.

## Testing Strategy

> Tests are the executable specification, written from this design record.

### Unit Tests (from component contracts)

| Component | Count | Focus |
|-----------|-------|-------|
| `LayerRef` parsing/display | 8 | FromStr, Display, edge cases |
| `push_multi_layer_manifest` | 6 | File/digest/mixed layers, error paths |
| `head_blob` | 3 | Found, not found, network error |
| **Total** | **17** | |

### Acceptance Tests (from user experience)

| Scenario | Count | Focus |
|----------|-------|-------|
| Push (file, digest, mixed, backward compat, error) | 7 | CLI layer arg handling |
| Pull (multi-layer install, shared dirs, overlap) | 3 | Assembly integration |
| Round-trip (push + pull end-to-end) | 3 | Full lifecycle |
| Dedup (layer reuse, cascade) | 2 | Storage efficiency |
| **Total** | **15** | |

## Risks

| Risk | Mitigation |
|------|------------|
| `oci_client` doesn't support HEAD blob | Check first; if missing, add to patched fork (small change) |
| Media type mismatch for digest layers | OCX extracts by magic bytes, not media type. Use standard fallback. Document the convention. |
| Backward compatibility for `package push` | Keep positional `content` arg as optional, mutual exclusion with `--layer` via clap |
| Multi-layer overlap errors confuse users | Error message includes layer indices, conflicting path, and guidance to fix |
| Mirror breaks on Publisher signature change | Mirror adaptation is part of this plan (step B1.8); tested by existing mirror tests |
| Large multi-layer packages hit progress reporting issues | Each file layer gets its own progress bar (existing pattern); digest layers are instant (HEAD only) |

## Verification

```sh
cargo nextest run -p ocx_lib layer_ref           # LayerRef unit tests
cargo nextest run -p ocx_lib push_multi           # Push unit tests
cargo nextest run -p ocx_lib assemble             # Assembly tests (existing)
cd test && uv run pytest tests/test_multi_layer.py -v  # Acceptance tests
task verify                                        # Full quality gate
```

---

## Dependency Graph

```
Workstream A (Pull):                    Workstream B (Push):
  A1: Remove guard + multi-layer          B1: Stubs (LayerRef, Publisher, CLI, Mirror)
          ↓                                       ↓
          │                               B2: Architecture review
          │                                       ↓
          │                               B3: Specify (unit + acceptance tests)
          │                                       ↓
          │                               B4: Transport head_blob (prerequisite for B5)
          │                                       ↓
  A2: Pull acceptance tests ←───────── B5: Implement (all components)
      (depends on push being done)                ↓
                              ────────── Convergence (Phase C) ──────────
                                          Review-Fix Loop
                                          Adversarial Pass
                                          Commit
```

**Critical path:** B1 → B2 → B3 → B4 → B5 → C. Workstream A1 is a quick win that can land independently (no push-side dependency). A2 acceptance tests require B5 completion for the full push+pull round-trip.

**GC safety note:** When two packages share a layer via digest reuse, each package creates its own `refs/layers/` forward-ref symlink to the shared layer. Uninstalling one package removes only its forward-ref; GC sweeps the layer only when no forward-refs remain. This is the existing GC mechanism — no changes needed.

## Deferred Findings (require human judgment)

| # | Finding | Why human judgment is needed |
|---|---------|---------------------------|
| D1 | **Media type fallback for digest layers** — Using `MEDIA_TYPE_LAYER_TAR_GZIP` for digest-referenced layers is correct given magic-byte extraction. However, third-party OCI tools (cosign, Syft, Harbor GC) may misinterpret layers with incorrect media types. | Policy decision: accept the interop risk or probe blob content. Mitigated by documenting the convention in code comments. |
| D2 | **SHA-384/512 parsing tests** — Unit tests include non-SHA-256 variants. In practice OCI registries almost universally use SHA-256. | Low-risk either way. Include for completeness or drop to reduce test noise. |
| D3 | **Progress reporting for multi-layer push** — Each file layer should get its own progress span, but the current single-file code uses one span. Implementation must create per-layer spans. | Noted in step B5.3; implementation detail, not a design decision. |
