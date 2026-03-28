# ADR: Package Layers

## Metadata

**Status:** Proposed
**Date:** 2026-03-28
**Deciders:** mherwig, architect
**Beads Issue:** ocx-sh/ocx#20
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** data | api | infrastructure
**Supersedes:** N/A
**Superseded By:** N/A

## Context

OCX currently treats each package as a single OCI layer — one compressed archive containing all files. The OCI manifest's `layers` array always has exactly one entry (enforced at `client.rs:298`). This works well for simple packages but creates inefficiency when multiple packages share common content.

### The problem: redundant storage for shared content

Consider a cross-compilation toolchain like GCC. The headers, specs, and plugin files are identical across all target architectures — only the compiler binary differs per platform. Today, each platform variant stores a complete copy of everything:

```
gcc:13 (linux/amd64)  →  [headers + specs + plugins + gcc-amd64]   ~200 MB
gcc:13 (linux/arm64)  →  [headers + specs + plugins + gcc-arm64]   ~200 MB
gcc:13 (darwin/arm64)  →  [headers + specs + plugins + gcc-darwin]  ~200 MB
                                                            Total:  ~600 MB
```

With layers, the shared content is stored once:

```
layer-gcc-common      →  [headers + specs + plugins]                ~150 MB (stored once)
layer-gcc-amd64       →  [gcc-amd64]                                ~50 MB
layer-gcc-arm64       →  [gcc-arm64]                                ~50 MB
layer-gcc-darwin      →  [gcc-darwin]                                ~50 MB
                                                            Total:  ~300 MB
```

This matters at three levels:
1. **Registry storage** — shared layer digest stored once (OCI dedup is by blob digest)
2. **Network transfer** — mirrors and consumers download shared layers once
3. **Local disk** — content-addressed object store stores each layer digest once

### The problem: self-contained applications with shared runtimes

Applications requiring a runtime (Python scripts, Java JARs, .NET assemblies) face a similar issue. Today these must either: (a) bundle the entire runtime in the package (wasteful), (b) declare a dependency on the runtime (evelynn branch — loose coupling via env vars, no structural guarantee), or (c) use a separate packaging step.

Layers offer option (d): the runtime is a shared layer, the application files are another layer, and the package manifest binds them as a single structural unit with integrity guarantees.

### Layers vs. dependencies: structural integrity vs. environment composition

This distinction is fundamental to understanding why both mechanisms are needed:

| Dimension | Dependencies (evelynn) | Layers (this ADR) |
|-----------|----------------------|-------------------|
| **Coupling** | Loose — separate packages composed via env vars | Tight — parts of one package merged into one content tree |
| **Identity** | Each dep has its own OCI identity (digest, tag) | Layers share one package identity (one manifest) |
| **Filesystem** | Each package in its own `content/` directory | Layers merge into one `content/` directory |
| **Who controls it** | Publisher declares which other packages are needed | Publisher decides how to partition their own files |
| **`${installPath}`** | Each dependency resolves independently | Single `${installPath}` for the assembled package |
| **Integrity** | No structural guarantee — deps may be independently updated | Manifest is the contract — all layers verified together |
| **Use case** | "My app needs Java 21" | "GCC headers shared across architecture-specific builds" |

Dependencies compose **independent** packages. Layers compose **a single** package from **structurally coupled** parts.

### Layers vs. patches: publisher vs. operator control

Infrastructure patches (sion branch) let operators overlay configuration onto packages. Layers let publishers partition their own package content. These are complementary:

| Dimension | Patches (sion) | Layers (this ADR) |
|-----------|---------------|-------------------|
| **Who controls it** | Infrastructure operator | Package publisher |
| **Registry** | Separate operator-controlled registry | Same registry as the package |
| **Composition** | Env var overlay (last-writer-wins) | File tree merge (no overlap allowed) |
| **Coupling** | Zero — operator doesn't modify the package | Total — layers are the package |
| **Use case** | "All Java installs need our corporate CA certs" | "My package's runtime and app code are separable" |

### Layers vs. variants: internal structure vs. build-time choices

Variants (already on main) encode build-time characteristics in tags (`python:pgo-3.12` vs `python:debug-3.12`). Layers and variants are synergistic — variants that share common content can use shared layers:

```
python:pgo-3.12    = [layer-cpython-stdlib, layer-cpython-pgo]
python:debug-3.12  = [layer-cpython-stdlib, layer-cpython-debug]
```

The stdlib layer (identical across optimization profiles) is stored once.

### How the four composition mechanisms relate

```
Loose coupling                                              Tight coupling
├── Patches (sion)      — infra operator overlays env vars onto packages
├── Dependencies (evelynn) — publisher declares required external packages
├── Variants (main)     — publisher offers alternative builds (tag selection)
└── Layers (this ADR)   — publisher partitions internal package structure ──┘
```

All four are orthogonal and compose: a package can have variants, layers, dependencies, and be patched by an operator simultaneously.

### Scope — what this ADR covers and what it does not

| Concern | Status | Relationship to layers |
|---------|--------|----------------------|
| **Multi-layer pull/extract** | This ADR | Download, validate, extract, assemble |
| **Multi-layer push/create** | This ADR | Partitioning, bundling, publishing |
| **Layer-level GC** | This ADR | Reference tracking for shared layers |
| **Dedup optimization tool** | Future work | Analyzes packages to suggest optimal layering |
| **`zstd:chunked` lazy-pull** | Future work | Cross-layer file-level dedup at the registry |
| **Dependencies** | Separate ADR (evelynn) | Orthogonal — external package references |
| **Patches** | Separate ADR (sion) | Orthogonal — operator env overlays |
| **Variants** | Implemented (main) | Synergistic — shared layers across variants |

## Decision Drivers

- **Storage efficiency**: Shared content across platform variants and package versions should be stored once at the registry, mirror, and local level.
- **Structural integrity**: A multi-layer package must be self-describing and verifiable — the manifest is the contract for which layers compose the package.
- **Parallel extraction**: Layers should be downloadable and extractable concurrently for performance.
- **OCI compatibility**: Must use standard OCI manifest `layers` array with standard media types. Any conformant registry must be able to store and serve these images without modification.
- **GC correctness**: Layer objects shared across packages must not be garbage collected while any package referencing them is still installed.
- **Backward compatibility**: Existing single-layer packages must continue to work unchanged.
- **Simplicity**: Prefer the simplest model that achieves the goals. Avoid OCI complexities (whiteouts, overlayfs) that don't serve binary package distribution.

## Industry Context & Research

**Research artifact:** [research_oci_layers_and_composition.md](./research_oci_layers_and_composition.md)

### Cross-ecosystem survey

| System | Layer model | Overlap handling | Dedup granularity |
|--------|-----------|-----------------|-------------------|
| **Docker/OCI** | Sequential changesets with whiteouts | Upper shadows lower (by design) | Whole layer blob |
| **Nix** | No layers — isolated store paths per derivation | Impossible (unique hash paths) | Store path (package version) |
| **Flatpak** | OSTree runtimes shared across apps | Separate addressable units | File content hash |
| **Ollama** | Per-component layers with distinct `mediaType` | No overlap (distinct component types) | Whole layer blob |
| **Bazel rules_oci** | One layer per build target | No overlap (build system prevents it) | Whole layer blob |
| **dpkg** | File ownership database | Conflicts/Replaces declarations | N/A |
| **overlayfs** | Multiple lower dirs, one upper dir | Upper always shadows lower | N/A |

### Key insights

1. **No production system enforces overlap-free layers at the filesystem level.** Enforcement is always at a higher level: build system (Bazel), package metadata (dpkg), or structural naming (Nix). OCX must enforce at the package manager level during extraction.

2. **The Ollama pattern is the closest precedent** for binary package dedup on OCI. Separate layers with distinct `mediaType` annotations, raw content-addressed blobs, no tar wrapping overhead. Registry-level dedup by digest.

3. **OCI whiteouts solve a problem OCX doesn't have.** Whiteouts enable incremental container image builds where each Dockerfile step adds a changeset. Binary packages are not built incrementally — the publisher knows the full content at publish time. Whiteouts add complexity with no benefit.

4. **Tar format nondeterminism is a real concern** (Cyphar's OCIv2 analysis). Identical file content can produce different layer digests depending on `readdir` order. Publishers must use deterministic archiving tools to realize dedup benefits.

5. **Layer-granularity dedup is coarse but sufficient** for the stated use cases (shared headers, shared runtimes). Cross-package file-level dedup (`zstd:chunked`) is a future optimization, not a prerequisite.

**Trending:** `zstd:chunked` layers (containerd support maturing), Nydus/Dragonfly (CNCF incubating).
**Established:** OCI tar-based layers, Nix store paths, overlayfs.
**Declining:** eStargz (superseded by `zstd:chunked` and Nydus).

## Considered Options

The three options are as proposed in issue #20, plus a fourth "do nothing" option.

### Option 0: Do Nothing — Single-Layer Only

**Description:** Keep the current `layers.len() == 1` enforcement. Shared content is handled via dependencies (evelynn) or duplicated across packages.

| Pros | Cons |
|------|------|
| Zero implementation cost | No storage dedup for shared content across platform variants |
| No GC complexity increase | Publishers must duplicate headers/runtimes in every package |
| No extraction complexity | No structural integrity for composite packages |
| | Dependencies provide env composition but not file tree composition |

### Option 1: Full OCI-Compliant Layers — Sequential Changesets with Whiteouts

**Description:** Layers are applied sequentially, bottom-to-top. Upper layers can shadow files from lower layers using whiteout markers (`.wh.<name>`, `.wh..wh..opq`). This is the standard OCI container image model.

| Pros | Cons |
|------|------|
| Full OCI compliance — existing container tooling works | Layers must be applied sequentially, preventing parallel extraction |
| Images can be incrementally modified by adding layers | Upper layers can shadow lower layers, enabling "untransparent / chaotically crafted packages" |
| Well-documented spec with extensive ecosystem support | Whiteout handling adds significant extraction complexity |
| | overlayfs would enable lazy composition but is Linux-only and requires privileges |
| | Nondeterministic tar ordering can defeat dedup even with identical content |
| | Solving a container build problem OCX doesn't have |

### Option 2: Overlap-Free Layers — File Structures Merged

**Description:** Layers are independent file trees merged into one content directory. Files (other than directories) must not appear at the same path in two layers. Directories at the same path are merged (union). No whiteouts, no layer ordering dependency for extraction.

```
layer-gcc-common/            layer-gcc-amd64/
└── usr/                     └── usr/
    └── lib/                     └── bin/
        └── gcc/                     ├── gcc
            └── 13/                  ├── g++
                ├── specs            └── cpp
                └── plugin/
```

Both layers contribute to `usr/` (directory merge), but no non-directory file exists in both.

| Pros | Cons |
|------|------|
| Parallel extraction — no ordering dependency, no whiteouts | Differs from standard OCI layer semantics (no shadowing) |
| Overlap validation at extraction catches structural errors early | Standard OCI images with shadowing layers are incompatible |
| Simple mental model for publishers — "partition your files" | Shared directories (e.g., `usr/bin/`) require careful partitioning |
| Content-addressable dedup at layer granularity | Publishers must ensure non-overlap — tooling should help |
| Works on all platforms — pure userspace, no overlayfs | More complex GC (layer objects need reference tracking) |
| OCI-compatible wire format (standard media types, any registry) | |
| Backward compatible — single-layer packages unchanged | |

### Option 3: Completely Overlap-Free Layers — Disjoint Root Trees

**Description:** Like Option 2 but stricter: layers must differ at the root directory level. No directory merging at all — each layer contributes a unique top-level directory.

```
layer-gcc-common/            layer-gcc-amd64/
└── gcc-common/              └── gcc-amd64/
    └── usr/                     └── usr/
        └── lib/                     └── bin/
            └── gcc/                     ├── gcc
                └── 13/                  └── g++
                    └── specs
```

| Pros | Cons |
|------|------|
| Layers never interact — each has a unique root | Forces unnatural directory structures |
| Layers can be hardlinked/symlinked without extraction merging | `${installPath}/bin` can't work — paths depend on which layer a file is in |
| Simplest validation — just check root dirs are disjoint | Package env vars need per-layer `${installPath}` or layer-specific path variables |
| | Severely limits package design options |
| | **Breaks the `${installPath}` contract** — no single content directory to point env vars at |
| | Even less intuitive than Option 2 for OCI users |

## Decision Outcome

**Chosen Option:** Option 2 — Overlap-free layers with file structure merging

**Rationale:** Option 2 strikes the right balance between dedup efficiency, implementation simplicity, and compatibility with OCX's existing architecture. The key arguments:

1. **Parallel extraction** is a significant performance advantage. OCI's sequential changeset model exists for container build caching, which is irrelevant to binary packages. Removing the ordering dependency lets OCX download and extract all layers concurrently.

2. **No whiteouts** eliminates a class of complexity that serves no purpose in binary distribution. Publishers know their full content at publish time — there's no "delete a file from a previous layer" use case.

3. **Overlap validation** is a feature, not a limitation. It catches publisher mistakes at extraction time rather than producing silently wrong content. If two layers provide the same file, that's a structural error, not a valid composition.

4. **The `${installPath}` contract is preserved.** Option 3 breaks this by requiring layer-specific root directories, which would cascade into env var resolution, `ocx exec`, `ocx env`, and all tooling that relies on a single content path per package. Option 2 maintains one assembled `content/` directory per package — the layer boundary is invisible to consumers.

5. **Standard OCI wire format.** Layers use standard media types and sit in a standard `manifest.layers[]` array. Any OCI registry stores and serves them. The overlap-free constraint is enforced by OCX at extraction time, not encoded in the wire format.

### Nuances: what each option uniquely enables or prevents

| Capability | Option 1 (changesets) | Option 2 (overlap-free) | Option 3 (disjoint roots) |
|---|---|---|---|
| Incremental image modification (add a layer to patch) | Yes — add a shadowing layer | No — must republish with modified layer | No — must republish |
| Parallel layer extraction | No — ordering matters | Yes — no dependencies between layers | Yes — fully independent |
| Cross-platform portability | No — overlayfs is Linux-only; userspace apply is complex | Yes — pure userspace merge | Yes — pure symlink/hardlink |
| Standard OCI image consumption | Yes — any OCI image works | Partial — only overlap-free images work | Partial — only disjoint-root images work |
| Single `${installPath}` per package | Yes | Yes | **No** — breaks env resolution contract |
| Structural integrity validation | No — shadowing is intentional, can't distinguish error from intent | Yes — overlap = error, caught at extraction | Yes — disjoint roots trivially validated |
| Shims and tool execution | Works — standard content tree | Works — assembled content tree | **Complicated** — shims would need to know layer structure to find executables |
| Layer reuse without extraction | No — must apply changesets sequentially | Partial — needs directory merge step | Yes — hardlink/symlink individual layer roots |

### Impact on tool execution and shims

The choice between options has significant implications for `ocx exec` and future shims:

**Options 1 and 2** maintain the contract that `${installPath}` points to a single directory containing all package files. `ocx exec gcc:13 -- gcc -v` resolves `gcc` via `PATH=${installPath}/bin:...` and finds `${installPath}/bin/gcc` in the assembled content tree. Shims (future ADR) would generate launchers that invoke `ocx exec` — they don't need to know about layers because the assembled view is opaque.

**Option 3** breaks this. With disjoint roots, `gcc` lives at `${installPath}/gcc-amd64/usr/bin/gcc` and headers at `${installPath}/gcc-common/usr/lib/gcc/13/specs`. The publisher must declare per-layer env vars or use a complex path template. Shims would need layer-awareness to construct correct `PATH` entries. This cascades into `ocx env`, `ocx shell env`, CI export, and profile management — every path-consuming feature becomes layer-aware.

**Option 2's assembled view keeps layers as a storage optimization invisible to consumers.** This is the right abstraction boundary: publishers think about layers (how to partition files), consumers think about packages (one content dir with env vars).

### Consequences

**Positive:**
- Registry, mirror, and local storage dedup for shared layers across platform variants and package versions.
- Parallel download and extraction of layers — bounded only by bandwidth and I/O.
- Structural integrity: overlap validation catches malformed packages at extraction time.
- Backward compatible — existing single-layer packages work without changes.
- Synergy with variants: shared layers across variant builds reduce total storage.
- Standard OCI wire format: any registry, any mirror, standard tooling for push/pull.

**Negative:**
- GC becomes more complex — layer objects need reference tracking and cascading collection.
- Standard OCI images with layer shadowing (whiteouts) are incompatible — OCX will reject them with a clear error. This is by design: OCX packages are a subset of OCI images, not a superset.
- Publishers must partition files to avoid overlap — tooling should validate and help, but the constraint is non-obvious to someone expecting Docker-style layer behavior.
- Extraction step now includes a merge/assembly phase, adding I/O overhead for hardlink/symlink creation.

**Risks:**
- **Deep layer stacks may slow extraction.** Mitigation: practical use cases have 2–5 layers; the overlap validation is O(total files) regardless of layer count.
- **Hardlink-based assembly may fail on cross-filesystem setups.** Mitigation: fall back to symlinks; if symlinks also fail (unusual), fall back to copy.
- **Deterministic tar archiving is a publisher responsibility.** Mitigation: document the requirement; `ocx package create` should produce deterministic archives by default.
- **Shared layer eviction under disk pressure.** Mitigation: GC only collects objects with empty `refs/`; no LRU eviction. Users can `ocx clean` to reclaim unused layers.

## Technical Details

### Architecture

#### Storage model

```
ObjectStore (content-addressed by digest)
├── {registry}/{repo}/{layer1-digest}/
│   ├── content/            ← extracted layer 1 files
│   ├── metadata.json       ← layer metadata (optional, for standalone layers)
│   └── refs/               ← back-references from packages using this layer
│       ├── {hash1}  → ../{pkg-A-digest}/content   ← gcc:13 (linux/amd64) uses this layer
│       └── {hash2}  → ../{pkg-B-digest}/content   ← gcc:13 (linux/arm64) uses this layer
│
├── {registry}/{repo}/{layer2-digest}/
│   ├── content/            ← extracted layer 2 files (arch-specific)
│   ├── metadata.json
│   └── refs/
│       └── {hash1}  → ../{pkg-A-digest}/content
│
├── {registry}/{repo}/{pkg-A-digest}/
│   ├── content/            ← assembled view (hardlinks/symlinks to layer content)
│   │   ├── usr/lib/gcc/13/specs    → ../../{layer1-digest}/content/usr/lib/gcc/13/specs
│   │   └── usr/bin/gcc             → ../../{layer2-digest}/content/usr/bin/gcc
│   ├── metadata.json       ← package metadata (env vars, dependencies, etc.)
│   ├── manifest.json       ← OCI manifest (records layer digests and order)
│   ├── layers/             ← NEW: forward-references to layer objects
│   │   ├── {hash-of-layer1-content}  → ../../{layer1-digest}/content
│   │   └── {hash-of-layer2-content}  → ../../{layer2-digest}/content
│   └── refs/               ← back-references from install symlinks
│       └── {hash}  → ../../../installs/.../candidates/13
```

Key design choices:
- **Layer objects are first-class objects in the store**, stored by their own digest with their own `content/`, `refs/`, and optional `metadata.json`.
- **Package objects assemble** a merged `content/` view using hardlinks (preferred, same filesystem) or symlinks (cross-filesystem fallback).
- **`layers/` directory** in the package object holds forward-references to layer objects (analogous to `deps/` in the dependency model on evelynn). This enables GC to discover layer relationships via filesystem traversal.
- **`refs/` in layer objects** hold back-references from package objects that use the layer. This prevents GC from collecting shared layers while any package uses them.
- **Single-layer packages** continue to store content directly (no `layers/` directory, no assembly step).

#### Pull and extraction flow

```
pull_package(manifest):
    if manifest.layers.len() == 1:
        # Existing single-layer fast path (unchanged)
        pull_blob(layer[0].digest) → extract to content/

    else:
        # Multi-layer path
        parallel for layer in manifest.layers:
            if not in_object_store(layer.digest):
                pull_blob(layer.digest) → extract to layer object content/

        validate_no_overlap(layer_contents)   # fail-fast on file conflicts
        assemble_merged_view(layer_contents, package_content)
        create_layer_forward_refs(package → layers)
        create_layer_back_refs(layers → package)
```

#### Overlap validation

```
validate_no_overlap(layers):
    seen_paths: HashMap<RelativePath, LayerIndex>

    for (i, layer) in layers:
        for entry in walk_tree(layer.content):
            if entry.is_directory():
                continue  # directories merge naturally
            if let Some(prev) = seen_paths.get(entry.relative_path):
                error!("Overlap: '{}' exists in layer {} and layer {}", path, prev, i)
            seen_paths.insert(entry.relative_path, i)
```

Validation is O(total files across all layers). Directories are allowed to overlap (they merge); non-directory entries (files, symlinks) must be unique.

#### Assembly via hardlinks

```
assemble_merged_view(layers, target_content):
    for layer in layers:
        for entry in walk_tree(layer.content):
            source = layer.content / entry.relative_path
            target = target_content / entry.relative_path

            if entry.is_directory():
                create_dir_all(target)
            else:
                hard_link(source, target)  # fallback: symlink, then copy
```

Hardlinks are preferred because:
- Zero additional disk space (same inode)
- No broken-link risk if layer object is accessed directly
- Atomic — visible immediately after creation
- Work transparently with all tools (no symlink-aware special cases)

Symlinks are the fallback for cross-filesystem or platform limitations.

### Garbage collection

#### The challenge

With shared layers, GC becomes a graph problem — not just "delete objects with empty `refs/`". A layer object may be referenced by multiple packages. Deleting a package must decrement the layer's ref count, and only collect the layer when no packages reference it.

This is structurally analogous to the dependency GC problem solved on the evelynn branch (Kahn's algorithm on the reverse dependency graph). The same pattern extends naturally to layers.

#### GC model: layers as GC-tracked objects

The GC model uses the same `refs/` + forward-reference pattern as dependencies:

| Ref type | Location | Points to | Created when |
|----------|----------|-----------|-------------|
| **Install → Package** | Package's `refs/{hash}` | Install symlink path | `ocx install` |
| **Package → Layer** (forward) | Package's `layers/{hash}` | Layer's `content/` | Pull/extraction |
| **Layer → Package** (back-ref) | Layer's `refs/{hash}` | Package's `content/` | Pull/extraction |
| **Package → Dependency** (forward) | Package's `deps/{hash}` | Dependency's `content/` | Dependency pull (evelynn) |
| **Dependency → Package** (back-ref) | Dependency's `refs/{hash}` | Package's `content/` | Dependency pull (evelynn) |

GC phase 1 (scan) counts `refs/` entries for each object. GC phase 2 (Kahn's algorithm) processes:
1. Objects with `ref_count == 0` are enqueued.
2. For each dequeued object:
   - Decrement ref counts of its layers (via `layers/` forward-refs).
   - Decrement ref counts of its dependencies (via `deps/` forward-refs).
   - If any layer/dependency drops to 0, enqueue it.
3. All dequeued objects are collected.

This is O(N + E) where N = objects and E = total forward-references (layers + deps).

#### GC edge cases

**Shared layer across packages:** Layer L is used by packages A and B. Uninstalling A removes A's back-ref from L's `refs/`. L still has B's back-ref → not collected. Uninstalling B removes B's back-ref → L's `refs/` is empty → L is collected.

**Shared layer across packages with dependencies:** Package A uses layer L and depends on package D. D also uses layer L (same shared runtime). Uninstalling A removes A's refs from L and D. D still has its own install ref → D stays. L still has D's ref → L stays. When D is eventually uninstalled or its last referrer is removed, the cascade collects both D and L.

**Single-layer packages:** No `layers/` directory → GC treats them as before (only `refs/` and `deps/` matter). Fully backward compatible.

**Package with both layers and dependencies:** The `layers/` and `deps/` forward-reference directories are independent. GC processes both edge types in the same Kahn's pass.

#### Concurrency considerations

Same constraint as dependency GC: `clean` is not safe to run concurrently with `install`. A concurrent install that creates layer back-refs between phase 1 (scan) and phase 2 (delete) could race. The existing best-effort guard (re-check `refs/` before deleting) applies equally to layer objects.

### Package creation flow

```
ocx package create --layer common/ --layer arch/ -o gcc-13.tar.gz
```

Each `--layer` argument specifies a directory to archive as a separate layer. The resulting manifest has one entry per layer in `manifest.layers[]`. The config blob stores metadata (env vars, etc.) that applies to the assembled package.

Alternatively, a layer manifest file:

```json
{
  "layers": [
    { "path": "common/", "media_type": "application/vnd.oci.image.layer.v1.tar+zstd" },
    { "path": "arch/",   "media_type": "application/vnd.oci.image.layer.v1.tar+zstd" }
  ]
}
```

The `media_type` field is optional — OCX infers it from compression. Custom media types are allowed for publishers who want to tag layer roles (e.g., `application/vnd.ocx.layer.runtime.v1.tar+zstd`).

### Manifest structure

Standard OCI image manifest with multiple layers:

```json
{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "config": {
    "mediaType": "application/vnd.ocx.package.config.v1+json",
    "digest": "sha256:abc...",
    "size": 1234
  },
  "layers": [
    {
      "mediaType": "application/vnd.oci.image.layer.v1.tar+zstd",
      "digest": "sha256:layer1...",
      "size": 150000000,
      "annotations": {
        "org.opencontainers.image.title": "gcc-common"
      }
    },
    {
      "mediaType": "application/vnd.oci.image.layer.v1.tar+zstd",
      "digest": "sha256:layer2...",
      "size": 50000000,
      "annotations": {
        "org.opencontainers.image.title": "gcc-amd64"
      }
    }
  ]
}
```

The `annotations.org.opencontainers.image.title` is advisory — it helps publishers identify layers in registry UIs. It has no semantic meaning to OCX.

### Error types

New error variants in `PackageErrorKind`:

```rust
/// Two layers provide the same non-directory file path.
LayerOverlap {
    path: PathBuf,
    layer_a: oci::Digest,
    layer_b: oci::Digest,
},

/// Failed to assemble the merged content view from layers.
LayerAssemblyFailed {
    digest: oci::Digest,
    source: std::io::Error,
},
```

### API impact

- `ocx install` — no change in output. Layer download progress shows per-layer bars.
- `ocx find` — no change. Returns assembled `content/` path.
- `ocx exec` / `ocx env` — no change. `${installPath}` resolves to assembled `content/`.
- `ocx clean --dry-run` — shows layer objects that would be collected, with annotation "(shared layer)".
- `ocx index list` — no change. Layer count is an internal detail.
- `ocx package create` — new `--layer` flag for multi-layer bundling.
- `ocx package push` — handles multi-layer manifests, pushes shared layers only once.

## Implementation Plan

1. [ ] **Multi-layer pull** — Relax `layers.len() == 1` validation. Download all layer blobs. Extract each to its own object store entry.
2. [ ] **Overlap validation** — Walk extracted layer trees, detect non-directory path conflicts.
3. [ ] **Assembly** — Create merged `content/` view in package object via hardlinks (symlink fallback).
4. [ ] **Layer reference management** — Add `layers/` forward-refs and layer `refs/` back-refs to `ReferenceManager`.
5. [ ] **GC extension** — Extend Kahn's algorithm to traverse `layers/` edges alongside `deps/` edges.
6. [ ] **Multi-layer `package create`** — `--layer` flag for specifying layer directories.
7. [ ] **Multi-layer `package push`** — Construct multi-layer manifest, push layers, skip already-present digests.
8. [ ] **Acceptance tests** — Multi-layer install, shared layer dedup, GC correctness, overlap rejection.

## Validation

- [ ] Single-layer packages work unchanged (backward compatibility)
- [ ] Multi-layer packages extract correctly with assembled content view
- [ ] Overlap detection rejects conflicting layers
- [ ] Shared layers stored once in object store
- [ ] GC correctly cascades: package deletion → layer ref decrement → layer collection when unreferenced
- [ ] `ocx exec` / `ocx env` work transparently with multi-layer packages
- [ ] `task verify` passes (fmt, clippy, build, unit tests, acceptance tests)

## Links

- [ocx-sh/ocx#20 — Supporting Layers](https://github.com/ocx-sh/ocx/issues/20)
- [OCI Image Layer Specification](https://github.com/opencontainers/image-spec/blob/main/layer.md)
- [OCI Image Manifest Specification](https://github.com/opencontainers/image-spec/blob/main/manifest.md)
- [Research: OCI Layers and Composition](./research_oci_layers_and_composition.md)
- [ADR: Package Dependencies](./adr_package_dependencies.md) (evelynn — orthogonal)
- [ADR: Package Variants](./adr_variants.md) (main — synergistic)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-03-28 | architect | Initial draft |
