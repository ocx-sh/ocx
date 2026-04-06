# ADR: Three-Tier Content-Addressed Storage

## Metadata

**Status:** Accepted
**Date:** 2026-04-06
**Deciders:** mherwig
**Related Issues:** #27 (repo in object path), #22 (multi-layer packages), #33 (project lockfile), #24 (referrers API), #31 (mount dependencies)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/tech-strategy.md`
**Domain Tags:** data | infrastructure
**Supersedes:** N/A

## Context

OCX's current `objects/` store conflates three concerns into one directory tree:

```
objects/{registry}/{repo}/{algorithm}/{shard1}/{shard2}/{shard3}/
  content/        ← extracted package files (single layer)
  metadata.json   ← OCX package metadata
  refs/           ← install back-references
  deps/           ← dependency forward-references
  resolve.json    ← dependency resolution tree
  install.json    ← completion sentinel
```

This has four interconnected problems:

1. **Repository in the CAS path breaks dedup** (#27). `objects/ocx.sh/cmake/sha256/...` and `objects/ocx.sh/clang/sha256/...` with the same layer digest produce separate copies. Digests are repo-independent — the repo has no business in the content-addressed path.

2. **No place for shared layers** (#22). The store assumes one layer per package. Multi-layer packages need layers as first-class objects that can be shared across platform variants of the same package.

3. **Manifests live outside the CAS** (#33). Manifests are cached in `index/{registry}/objects/`, a separate store with its own lifecycle. Lock files (#33) want to pin manifest digests and resolve from cache — but the manifest cache is coupled to the index update flow.

4. **No place for non-extracted blobs** (#24). Signatures, SBOMs, and attestations from the OCI referrers API are raw blobs with no extraction step. The current store only handles extracted content.

## Decision Drivers

- **Cross-repo deduplication**: identical OCI layers from different repositories must share storage
- **Offline-first**: once a blob is fetched, it must be available without network access
- **Relocatability**: `OCX_HOME` must be movable as a unit without breaking references
- **Windows MAX_PATH**: path depth must fit within 260 characters for representative configurations
- **GC correctness**: reachability model must work across all tiers in a single pass
- **OCI extensibility**: must accommodate referrers, multi-layer packages, lock files, and mount dependencies without structural changes

## Industry Context & Research

**Research artifact:** [research_content_addressed_storage.md](./research_content_addressed_storage.md)

**Surveyed systems:** Nix store, containerd, OCI Image Layout, Bazel CAS, pnpm, Cargo, Git objects.

**Trending approaches:** Multi-tier blob/extracted separation (containerd), hardlink-based file dedup (pnpm, uv), output-addressed content (Nix CA derivations), OCI 1.1 for non-container artifacts.

**Key insight:** containerd's strict separation of compressed blobs (content store) from extracted layers (snapshotter) directly validates our blobs/ → layers/ split. The hash used for registry transfer differs from the hash used for extracted content, justifying separate tiers with different keys. Cargo's three-tier cache (index / .crate / src) independently arrived at the same raw/extracted separation for the same reason: atomic install with partial download recovery.

**Maturity assessment:** The three-tier pattern is established (containerd, Cargo). Forward-ref-only BFS GC is proven (Nix, Git). Hardlink-at-file-granularity assembly is established (pnpm, uv). The digest-in-file pattern is novel — no direct precedent, but principled given Windows MAX_PATH constraints.

## Considered Options

### Option A: Flat single-tier (evolve current model)

Remove repo from `objects/` path. Keep one store for everything. Add blob caching alongside object directories.

| Pros | Cons |
|------|------|
| Minimal code change | Raw blobs mixed with extracted content — harder to GC independently |
| No new abstractions | Assembly semantics undefined — multi-layer is a special case |
| | No type distinction between layers, blobs, and packages |
| | ObjectDir methods (metadata, deps, resolve) don't apply to layers/blobs |

### Option B: Two tiers (blobs/ + packages/)

Raw blobs in `blobs/`. Extracted packages directly in `packages/`. No separate layers/ — extraction goes straight into the package.

| Pros | Cons |
|------|------|
| Simpler than three tiers | No cross-package layer dedup — two packages sharing a layer extract it twice |
| Clear raw/extracted distinction | Contradicts multi-layer design (layers ARE the dedup unit for multi-layer) |
| | Single-layer packages can't share layer content with multi-layer packages |

### Option C: Three tiers (blobs/ + layers/ + packages/) — proposed

Three tiers with clear separation: raw blobs, extracted layers, assembled packages.

| Pros | Cons |
|------|------|
| Cross-repo layer dedup (no repo in blobs/ or layers/ paths) | Three tiers to understand and maintain |
| Offline-first (all raw blobs cached) | Assembly walks every file in every layer (fast, but not free) |
| Established pattern (containerd, Cargo; hardlink-at-file matches pnpm, uv) | Relocate-as-unit invariant must be documented and enforced |
| Clean GC: single BFS across all tiers | Digest-in-file invariant must be enforced at API level |
| Hardlink assembly enables relocatable OCX_HOME on a single volume | Cross-volume `OCX_HOME` is rejected at install time (inherited from atomic-rename constraint) |
| OCI 1.1 referrers reconstructable from stored blobs | |
| Same assembly flow for single-layer and multi-layer | |
| Package `content/` is a real directory — no `@loader_path` / `$ORIGIN` gotchas | |

## Decision Outcome

**Chosen Option:** Option C — Three tiers

**Rationale:** Removing repository from the CAS path is the primary motivator (#27). Once that is done, layers must be independent of any particular package — a separate tier. If we cache at the layer level, we should also cache raw blobs for offline completeness and referrers support (#24). The three tiers map naturally to three interpretation levels: uninterpreted (blobs), extracted (layers), assembled (packages).

### Consequences

**Positive:**
- Cross-repo dedup for identical layers
- Offline manifest/index resolution for lock files (#33)
- Clean multi-layer assembly path (#22)
- Storage for referrer blobs (#24)
- Relocatable OCX_HOME on a single volume (hardlinks move with the inode)
- Same code path for single-layer and multi-layer packages
- Package `content/` is a real directory — no cross-tier symlink traversal, no `@loader_path`/`$ORIGIN` gotchas
- No filename-based dispatch in `package_dir_for_content` — `.parent()` works directly
- Matches the pattern used by pnpm and uv (hardlinks at file granularity)

**Negative:**
- Migration cost: pre-1.0 breaking change, users must `rm -rf ~/.ocx` and reinstall
- Three stores to walk during GC (mitigated: single BFS pass)
- Assembly walks every file in every layer (mitigated: hardlinks are fast; measured to be acceptable for realistic layer sizes)
- Cross-volume `OCX_HOME` configurations are rejected at install time — inherited from the pre-existing `temp → packages/` atomic-rename constraint (same `$OCX_HOME` single-volume requirement)

**Risks:**
- **Hardlink cross-volume failure**: `io::ErrorKind::CrossesDevices` surfaces as a clear error at install time with a message pointing to the single-volume `$OCX_HOME` requirement. No silent fallback — the operator fixes the layout. This constraint is not new; the pre-existing atomic rename from `temp/` into `packages/` already required both directories to be same-fs, and `layers/` is a sibling of those under `$OCX_HOME`.
- **Windows file symlink in layers**: Intra-layer file symlinks fall back to `fs::copy` of the dereferenced target. Correctness preserved; symlink identity lost inside the assembled package. Uncommon for Windows-targeted layers.
- **Windows directory symlink in layers**: Not supported in the initial implementation. Returns an error. Rare in practice.
- **Digest file integrity**: Path and digest file could diverge if a bug writes the wrong digest. Mitigated by writing digest file atomically alongside content; verification at read time is optional but possible.
- **`OCX_HOME` partial relocation**: Moving only one tier breaks cross-tier forward-ref symlinks (`refs/layers/`, `refs/blobs/`, `refs/deps/`). Mitigated by documenting the move-as-unit invariant. Hardlinks inside `packages/.../content/` are not affected because they are not cross-tier — they are file-level aliases to inodes that move with the volume.

## Technical Details

### Storage Layout

```
$OCX_HOME/
├── blobs/                              # Tier 1: Raw OCI blobs
│   └── {registry}/
│       └── {algorithm}/{2hex}/{remaining_hex}/
│           ├── data                    # Raw blob bytes
│           └── digest                  # Full digest string
│
├── layers/                             # Tier 2: Extracted OCI layers
│   └── {registry}/
│       └── {algorithm}/{2hex}/{remaining_hex}/
│           ├── content/                # Extracted file tree (immutable)
│           └── digest                  # Full digest string
│
├── packages/                           # Tier 3: Assembled packages
│   └── {registry}/
│       └── {algorithm}/{2hex}/{remaining_hex}/ # Keyed by platform-specific manifest digest
│           ├── content/                # Real directory; files hardlinked from layers/
│           ├── metadata.json           # Package metadata (OCI config)
│           ├── manifest.json           # OCI manifest (audit trail)
│           ├── resolve.json            # Dependency resolution tree
│           ├── install.json            # Completion sentinel
│           ├── digest                  # Full digest string
│           └── refs/                   # All references consolidated
│               ├── symlinks/           # Back-refs from install symlinks (GC roots)
│               ├── deps/               # Forward-refs → packages/
│               ├── layers/             # Forward-refs → layers/
│               └── blobs/              # Forward-refs → blobs/
│
├── tags/                               # Tag → digest (mutable, thin)
│   └── {registry}/
│       └── {repo_path}.json            # { "3.28": "sha256:..." }
│
├── symlinks/                           # Install symlinks (renamed from installs/)
│   └── {registry}/
│       └── {repo_path}/
│           ├── current → packages/.../content
│           └── candidates/{tag} → packages/.../content
│
└── temp/                               # Download staging (unchanged)
```

### Key: No Repo in CAS Paths

`blobs/` and `layers/` key by `{registry}/{digest}` only. A given digest refers to the same bytes regardless of which repository published it. Including the repository would prevent cross-repo deduplication.

`packages/` also keys by `{registry}/{digest}` — the manifest digest uniquely identifies a package for a given platform.

`symlinks/` and `tags/` retain the repository in their paths because they are the human-facing namespace (users reference `cmake:3.28`, not `sha256:abc...`).

### Digest Recovery via digest File

All three tiers use Git-style one-level prefix sharding: `{algorithm}/{2hex}/{truncated_hex}`.

For SHA-256: `sha256/{hex[0..2]}/{hex[2..32]}` — 256 buckets, each containing a 30-char hash suffix as a directory name. Only 32 hex chars of the full 64-char digest are encoded in the path; the remainder (and the full digest) is recovered from the sibling `digest` file (see `write_digest_file` in `cas_path.rs`). The two-level shard mirrors Git, Bazel, pnpm, and Docker Registry v2. See D11 for the rationale behind simplifying from the previous three-level scheme.

Every CAS entry contains a `digest` file with the full digest string (e.g., `sha256:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9`). The digest file is the source of truth for identity — the path is for routing only and does not carry the full hash.

**Why a digest file?** Windows MAX_PATH (260 chars). With `%APPDATA%` (~35 chars) + OCX structure + SHA-512 (128 hex chars), full-path-encoding risks exceeding limits. The digest file decouples identity from path length and keeps the sharding scheme an implementation detail.

**Why not reconstruct from resolve.json?** Only packages have `resolve.json`. Layers and blobs need identity recovery too. The `digest` file is universal across all three tiers.

### Package Key: Platform-Specific Manifest Digest

Packages are keyed by the platform-resolved manifest digest, not the image index digest. This means:

- Same tag, different platforms → different package entries (correct: different binaries)
- Cross-compilation supported: install `cmake:3.28` for both linux/amd64 and linux/arm64
- Image index is a blob in `blobs/`, used during resolution, not as package key

**Exec overhead for digest-direct access:** When a user passes a raw image-index digest (`ocx exec cmake@sha256:idx_abc`), there's a new resolution step: read image index from `blobs/`, resolve platform, derive manifest digest. Cost: <1ms (one cached disk read + platform match). Only for digest-direct access — symlink paths bypass this entirely.

### Tags Mirror the Registry

`tags/{registry}/{repo_path}.json` maps `tag → digest` where the digest is whatever the registry returns — image index digest or manifest digest. No local assumption or reinterpretation. Nested directory structure for repo path (split on `/`).

### Lock to Image Index Digest by Default

For #33, lock files pin the image index digest when one exists. Platform resolution happens at install time:
- Different runners resolve different manifests from the same lock entry
- Future system-requirements feature can influence resolution
- Explicit manifest-digest pinning available as opt-in for full reproducibility

### Hardlink-Based Assembly

Every package — single-layer or multi-layer — follows the same flow:

1. Extract each layer into `layers/{layer_digest}/content/`
2. Create `packages/{manifest_digest}/content/` as a real directory
3. Walk each layer's `content/` tree and mirror it into the package `content/`: real directories created, regular files hardlinked via `hardlink::create`, intra-layer symlinks recreated verbatim (Unix) or one-level-dereferenced copy (Windows file symlinks)

No file post-processing. Content is immutable after assembly. Hardlinks make `$OCX_HOME` relocatable as a unit on a single volume. Cross-volume configurations surface as a clear `CrossesDevices` error at install time — not silent dedup loss. This constraint is inherited from the pre-existing `temp → packages/` atomic-rename invariant, not newly introduced. The package `content/` is indistinguishable from a flat extraction — tools walking it never cross into the layer tier.

Future mount points (#31) still use symlinks from package content into dependency content directories, because mounts cross package boundaries and need to be resolvable independently of the hardlink graph.

### Consolidated Reference Structure

All references live under `refs/` in a package, organized by target tier:

| Subdirectory | Direction | Target | Meaning |
|-------------|-----------|--------|---------|
| `refs/symlinks/` | Back-ref | `symlinks/` entries | Install symlinks pointing to this package |
| `refs/deps/` | Forward-ref | `packages/` entries | Dependencies this package requires |
| `refs/layers/` | Forward-ref | `layers/` entries | Layers assembled into this package |
| `refs/blobs/` | Forward-ref | `blobs/` entries | Raw blobs associated with this package |

Reference filenames use two naming schemes depending on whether the ref's identity is a path or a digest:

- **`refs/symlinks/`** — entries named `{16_hex}` = first 16 hex chars of SHA-256(forward_symlink_path). Produced by `ReferenceManager::name_for_path`. The identity is the location of the install symlink.
- **`refs/deps/`, `refs/layers/`, `refs/blobs/`** — entries named `{algorithm}_{32_hex}` (e.g., `sha256_4a3f...1b2c`), derived from the content digest by `ReferenceManager::name_for_digest`. The 32-hex suffix matches the CAS shard total length so ref filenames are the same identity width as on-disk shards.

### Single-Pass GC

One BFS traversal over all three tiers. Roots are a **marking**, not a type — different operations set different roots:

- `ocx clean`: roots = packages with valid `refs/symlinks/` + profile content-mode refs
- `ocx uninstall --purge`: roots change after symlink removal

```
Edges (forward-refs only):
  packages/.../refs/deps/    → other packages/
  packages/.../refs/layers/  → layers/ entries
  packages/.../refs/blobs/   → blobs/ entries

Sweep:
  Walk all packages/, layers/, blobs/ entries
  Mark reachable from roots via BFS through forward edges
  Delete unreachable across all three tiers
```

**Implementation note:** The reachability graph should carry tier information on each node (derived from path prefix: `Package`, `Layer`, `Blob`). This enables tier-aware deletion (packages have `refs/` to clean up; layers `rm -rf` content dir; blobs delete data file), reporting (`ocx clean --dry-run` → "3 packages, 5 layers, 2 blobs"), and validation (refs in `refs/layers/` should resolve to `layers/` paths).

**Blob retention gap (#35):** Reachability-based GC only protects blobs referenced by installed packages (`refs/blobs/`). Resolution-cache blobs (manifests fetched during `index update` or install resolution) and prefetched blobs (for offline bundles) have no referencing package and would be swept immediately. These need a separate policy-based retention mechanism (LRU, TTL, size cap, or similar). Until #35 lands, `ocx clean` should either skip unreferenced blobs or treat them conservatively. This gap does not affect packages or layers — only blobs that exist purely as cache entries.

**Race-safety invariant:** Forward-refs in `refs/layers/` and `refs/blobs/` are created in the package's temp directory **before** the assembly walker runs. This closes the race window between layer extraction and package completion — a concurrent `ocx clean` that walks the package tier after the atomic `temp → packages/` rename will always see the forward-refs, so no reachable layer is ever swept mid-assembly.

### Index Update = Tags Only

`ocx index update cmake` refreshes `tags/ocx.sh/cmake.json` — fetches the tag list from the registry. Does NOT fetch manifests or image indexes. Those are fetched and cached in `blobs/` during `ocx install` / `ocx lock`.

**LocalIndex adaptation:** The `LocalIndex::fetch_manifest()` method must be updated to read cached manifests from `blobs/` instead of `index/objects/`. The local/remote index distinction applies to **tags** (floating pointers — freshness matters, `--remote` forces network lookup) but not to **content-addressed blobs** (a manifest blob is valid forever if it exists locally). This means `LocalIndex` gains the ability to resolve manifests and image indexes from `blobs/`, supporting local OCI index → platform → manifest resolution without network access.

**Future direction:** A layered index structure could unify the local/remote distinction: local tags → remote tags (based on `--remote`/`--offline` flags), with content-addressed blob lookups always checking `blobs/` first regardless of mode. This would enable portable offline bundles (`tags/ + blobs/` = self-contained resolution index) and optional tarball retention for shared caches. Not in scope for the initial redesign — see #35 for the prerequisite blob retention policy.

### Manifest-Digest Consistency Invariant

All package references — `resolve.json` dependency identifiers, `refs/deps/` forward-refs, install symlinks — use **manifest digests** (platform-resolved), never image index digests. The resolution chain (tag → image index → platform → manifest) executes at install/lock time. After that, everything references the resolved manifest.

```
resolve.json dep.identifier.digest
  = package key in packages/{registry}/{algorithm}/{2hex}/{remaining_hex}/
  = digest file content in that package directory
```

### resolve.json Scope

Stores the dependency transitive closure with full `PinnedIdentifier` per dependency (registry + repo + tag + digest + visibility). Needed for env resolution (content path construction) and conflict detection (same repo, different digest). The root package's own identifier is redundant — the `digest` file serves that role.

### Install Flow

```
ocx install cmake:3.28

1. Tag resolution (only if tag given, not digest-direct)
   Read tags/ocx.sh/cmake.json → "3.28" → "sha256:idx_abc"

2. Index/manifest resolution
   Fetch/cache image index blob → blobs/ocx.sh/sha256/.../data
   Resolve current platform → manifest digest "sha256:mfst_def"
   Fetch/cache manifest blob → blobs/ocx.sh/sha256/.../data

3. Layer pull (parallel, per layer)
   For each layer digest in manifest:
     layers/ocx.sh/sha256/.../content/ exists? → skip
     Otherwise fetch, extract into layers/.../content/
     Write digest file

4. Package assembly (walker)
   Create packages/ocx.sh/sha256/.../ (real directory, not a symlink)
     Create refs/layers/ forward-refs BEFORE assembly (GC-safety)
     Single-layer: walker mirrors layers/{A}/content/ into packages/.../content/
       - directories → real directories
       - regular files → hardlink::create (CrossesDevices surfaces as a clear error)
       - symlinks → symlink::create with verbatim target string (Unix)
                    or fs::copy of dereferenced target (Windows file symlinks)
                    or error (Windows directory symlinks, rare)
     Multi-layer (future #22): walker runs once per layer into the same content/
                                layers validated non-overlapping at extraction time
     Write metadata.json, manifest.json, digest
     Create refs/blobs/ forward-refs (manifest, image index, config)
     Create refs/deps/ forward-refs (resolved dependencies)
     Write resolve.json, install.json (sentinel)

5. Symlink creation
   symlinks/ocx.sh/cmake/candidates/3.28 → packages/.../content
   Back-ref in packages/.../refs/symlinks/
```

## Design Decisions Record

Decisions made during the design process, with alternatives considered and rationale.

### D1: Three tiers vs two vs one

**Decided:** Three tiers (blobs/ + layers/ + packages/).

**Alternatives rejected:**
- **Single tier** (Option A): Cannot separate raw from extracted from assembled. ObjectDir methods assume package structure.
- **Two tiers** (Option B): No layer-level dedup. Single-layer and multi-layer use different paths.

**Rationale:** containerd's two-stage pull (content store + snapshotter) validates the raw/extracted split. Adding the assembled tier gives a clean home for package-specific state (metadata, deps, refs) without polluting layers.

### D2: No repo in CAS paths

**Decided:** `blobs/`, `layers/`, `packages/` key by `{registry}/{digest}` only.

**Alternative rejected:** Keep repo for easier human navigation of the store directory.

**Rationale:** The whole point of content-addressing is that identity = digest. Repo in path defeats dedup (#27). Registry stays for security isolation (a private registry's blob should not be confused with a public one sharing the same digest).

### D3: Forward refs only in lower tiers

**Decided:** Only `packages/` have back-refs (`refs/symlinks/`). Blobs and layers have no refs at all.

**Alternative considered:** Back-refs in blobs/layers for direct reachability checking without traversing packages.

**Rationale:** Back-refs in two-tier entries add maintenance burden (must be created/removed atomically with the forward-ref in packages). Forward-ref-only means reachability is determined entirely from the package graph. Simpler invariants, fewer consistency risks.

**Exception noted:** OCI referrers (#24) may need discovery metadata in `blobs/` directories (e.g., "which signatures refer to this manifest?"). The directory structure supports adding sibling files alongside `data` when needed. Deferred until #24 implementation.

### D4: Consolidated refs/ directory

**Decided:** All references under one `refs/` directory with typed subdirectories: `refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/`.

**Alternative rejected:** Four sibling directories at the package root (the current pattern with `refs/` + `deps/` as siblings).

**Rationale:** Consolidation makes the package directory cleaner — data files + one `refs/` directory. Mirrors the top-level `~/.ocx/` structure (symlinks, layers, blobs are recognizable names). All reference concerns co-located under one parent.

**Naming:** `deps/` kept instead of `packages/` because it carries semantic meaning ("my dependencies") rather than just describing the target type.

### D5: Package key = platform-specific manifest digest

**Decided:** Packages keyed by manifest digest, not image index digest.

**Alternative considered:** Key by image index digest (current behavior — the tag points to an image index, and the object stores the resolved content).

**Rationale:** Image index digest changes when any platform variant is updated. Manifest digest is stable for a given binary. Keying by manifest enables cross-compilation (install for multiple platforms simultaneously). The image index lives in `blobs/` as a resolution artifact.

**Trade-off noted:** Digest-direct access with an image index digest (`ocx exec cmake@sha256:idx_abc`) requires an extra resolution step (<1ms). Acceptable given the benefits.

### D6: Hardlink-based assembly with symlink preservation

**Decided:** `packages/{P}/content/` is a real directory. A walker mirrors each layer's `content/` tree into the package's `content/`, hardlinking regular files from `layers/{digest}/content/` and preserving intra-layer symlinks verbatim. No directory-level symlinks are used for assembly.

**Mechanism:**

- **Walker**: Depth-first traversal of `layers/{digest}/content/`. Creates corresponding real directories in `packages/{P}/content/`. For each entry:
  - **Regular file** → `hardlink::create(source, dest)`. Any error — including `io::ErrorKind::CrossesDevices` — propagates as a clear install-time failure pointing at the single-volume `$OCX_HOME` requirement.
  - **Symlink** → read the target string with `read_link`, then recreate the symlink at the destination with the **identical target string** via `symlink::create` on Unix. The target is never dereferenced, never rewritten to absolute, never computed from the temp path. A symlink chain like `libfoo.so → libfoo.so.1 → libfoo.so.1.2.3` reproduces exactly in the package directory.
  - **Directory** → create a real directory and recurse.
- **Multi-layer** (future, #22): Run the walker once per layer into the same package `content/`. Layers are validated as non-overlapping subtrees at extraction time. The walker does not handle interleaving conflicts — those are rejected before assembly begins.

Example — Python app with dependency layers sharing `.venv/`:
```
Layer A: content/app/              (application code)
Layer B: content/.venv/numpy/...   (pinned dependency)
Layer C: content/.venv/pandas/...  (pinned dependency)

Package content/ (assembled, all real dirs):
├── app/           (real dir)
│   └── main.py    (hardlink → layers/{A}/content/app/main.py)
└── .venv/         (real dir)
    ├── numpy/     (real dir)
    │   └── ...    (hardlinks → layers/{B}/content/.venv/numpy/...)
    └── pandas/    (real dir)
        └── ...    (hardlinks → layers/{C}/content/.venv/pandas/...)
```

**Motivation beyond cross-platform dedup:** Layers still enable publishers to deduplicate across releases. A Python app can ship its own code as layer A (changes every release) and its bundled dependencies as layers B and C (change rarely). Users upgrading from v1 to v2 skip downloading unchanged dependency layers. At the filesystem level, hardlinks share inodes across packages that reference the same layer — a file appears in `layers/{digest}/content/` and in every `packages/{P}/content/` that assembled from that layer, all pointing to the same inode. This is the pnpm / uv pattern applied to general-purpose binary packages.

**Why hardlinks are safe here** (reconsidering historical objections):

- **"Can't cross filesystem boundaries"** → `$OCX_HOME` is already required to be single-volume by the pre-existing `temp → packages/` atomic-rename step. Hardlinks inherit that constraint; cross-device attempts surface as a clear `CrossesDevices` error at install time. Operators who split `$OCX_HOME` across volumes fix their layout — they do not get a silent dedup regression.
- **"Not relocatable"** → irrelevant at the file level. Hardlinks are just additional directory entries for the same inode; moving the volume moves the inode. The relocate-as-unit invariant (D3) is per-volume anyway, which is exactly where hardlinks work.
- **"Mutation propagates"** → layer `content/` and package `content/` are both immutable after install. Nothing in the normal OCX lifecycle mutates assembled files. The mutation concern applies only to mutable working trees, which OCX does not have.
- **"Can't hardlink directories"** → the walker handles directory structure explicitly by creating real directories and recursing. Only regular files are hardlinked; directories are never linked.
- **"`du` reports misleading sizes"** → intentional; dedup is a feature of this design, not a bug. Tools that want apparent size can use `du -L`; tools that want physical footprint get the correct deduped number by default.

**Windows file symlink fallback:** Unix uses `symlink::create` to preserve intra-layer symlinks verbatim. Windows does **not** use the existing `symlink::create`:
- The existing module falls back to NTFS junction points, which are **directory-only** and **absolute-only**. Intra-layer symlinks are typically file symlinks with relative targets (e.g., `libfoo.so → libfoo.so.1`). Junctions cannot represent these.
- The existing module also joins relative targets against `std::env::current_dir()` to form an absolute path. That semantic is wrong for the walker — the relative target is meaningful only **within the destination directory**, not relative to whatever `cwd` the OCX process happens to have.
- Junctions remain appropriate for install symlinks in `symlinks/{registry}/{repo}/candidates/{tag}` and `symlinks/{registry}/{repo}/current` where the target is always an absolute package content directory. The existing `symlink::create` is correct for that use case and stays in place.

Therefore, Windows file symlink handling in the walker uses `tokio::fs::copy` of the dereferenced target — a one-level dereference of the layer symlink, copying the resolved file into the package. This loses intra-package symlink identity (the result is a regular file copy, not a symlink) but preserves correctness. Developers on Windows who want true symlinks in assembled packages can enable Developer Mode; the walker does not currently try native `std::os::windows::fs::symlink_file` even then — that is a possible future optimization.

**Windows directory symlink fallback:** Intra-layer directory symlinks on Windows are **not supported** in the initial implementation and return an error. They are rare in practice (most Unix layers with directory symlinks come from build systems producing Unix-first artifacts) and the design cost of deep-copying a potentially-large directory tree outweighs the benefit. Deferred with a clear error message.

**Cross-volume handling:** The library-level `hardlink::create` surfaces `io::ErrorKind::CrossesDevices` as an `Error::InternalFile` wrapping the io error. The walker propagates this, and the install command surfaces it with a message pointing at the single-volume `$OCX_HOME` recommendation. There is no silent copy fallback — the pre-existing `temp → packages/` atomic-rename constraint already required single-volume `$OCX_HOME`, and the hardlink module inherits rather than relaxes that invariant.

**Alternatives rejected:**
- **Directory-level symlinks from package `content/` into `layers/`** (the original D6 decision): Plan 8b implemented this and surfaced real complexity. (a) **Cross-layer relative RPATH breaks.** macOS `@loader_path` / Linux `$ORIGIN` resolve from the **real** path of the binary after symlink dereferencing, so a binary at `packages/{P}/content/bin/tool` with `$ORIGIN/../lib` RPATH actually looks in `layers/{A}/content/lib` — not `packages/{P}/content/lib` as the user would expect. Multi-layer packages with binaries that depend on libraries in a sibling layer would silently fail to resolve. (b) **`package_dir_for_content` required filename-based dispatch.** The function needed to avoid following `content/` into `layers/`, so it switched from `dunce::canonicalize(content_path).parent()` to string manipulation that depends on the `content` filename. (c) **Test helpers needed `.readlink()` workarounds.** `.resolve()` and `Path.resolve()` traverse the symlink and land inside `layers/`, not `packages/`, so equality comparisons against install candidate targets broke. (d) **No surveyed general-purpose binary package manager uses directory-level symlinks for assembly.** Nix extracts flat, Cargo extracts flat, Homebrew extracts flat, pnpm hardlinks at file granularity, uv hardlinks at file granularity, containerd uses snapshotter layers (not symlinks in the visible mount), Bazel uses runfiles symlinks but at a different layer of the system. The directory-symlink approach was a novel design with no precedent. Rejected.
- **Per-file symlinks**: Creates thousands of symlinks per package. Same `@loader_path` / `$ORIGIN` failure as directory symlinks because dereferencing still lands in the layer's real path. More complex walker/GC code. Rejected.
- **Copies (no layers/ tier)**: No local dedup. Simpler implementation but loses the cross-release dedup benefit. Considered viable for a minimal first implementation; rejected because hardlinks give equivalent dedup at equivalent simplicity.
- **Reflinks**: Ideal (COW, independent modification if ever needed, correct `du` reporting) but not supported on ext4 / NTFS / HFS+. Worth tracking as a future optimization tier on APFS / Btrfs / XFS / ReFS. The pnpm pattern is `reflink → hardlink → copy`; OCX adopts `hardlink → copy` for the initial implementation and can slot reflinks above hardlinks later without changing the rest of the design.

**Invariants:**
- **Install symlinks** in `symlinks/` always point to `packages/.../content/`, never directly to layers. The package content directory is the stable API surface.
- **No file in `packages/{P}/content/` is a symlink into `layers/`.** The package content path is indistinguishable from a flat extraction; tools walking it never cross into the layer tier. The only symlinks inside a package `content/` are those that already existed inside the source layer (recreated verbatim).
- **Walker preserves layer symlink target strings verbatim.** The walker never dereferences layer symlinks, never rewrites targets to absolute form, and never constructs targets from the temp or final package path. Whatever the publisher put in the tarball is what ends up in the assembled package.
- **`$OCX_HOME` must live on a single volume.** Cross-volume configurations surface as a clear install-time error (`CrossesDevices`). This constraint is not new — the pre-existing `temp → packages/` atomic-rename already required it; hardlinks inherit the same requirement without introducing a new one.

**Research artifact:** See symlinks vs hardlinks analysis in [research_content_addressed_storage.md](./research_content_addressed_storage.md). Key finding: macOS `@loader_path`/`@executable_path` and Linux `$ORIGIN` resolve from the real path (after symlink dereferencing), making both per-file and directory-level symlink assembly unsafe for binaries with relative dylib references. Hardlinks do not have this problem because a hardlink **is** the real path — there is nothing to dereference.

## Implementation

Implemented on branch `goat`, commits `66538a2` through `085a662`:

- **Plan 1 — CAS path helpers** (`66538a2`): Added `CasTier`, `CasPath`, and the `digest` file API in `cas_path.rs`; established sharding and digest-recovery conventions for all three tiers.
- **Plans 2–4 — Three stores** (`34989..215c2ab`): Added `BlobStore`, `LayerStore`, `PackageStore`, `SymlinkStore` (renamed from `InstallStore`, rooted at `symlinks/`), and `TagStore` as standalone modules.
- **Plan 5 — FileStructure migration** (`32bf779`): Replaced the old composite root (`objects/`, `index/`, `installs/`) with the six-store `FileStructure` (`blobs`, `layers`, `packages`, `tags`, `symlinks`, `temp`).
- **Plan 7 — LocalIndex** (`bc3b0cf`): Updated `LocalIndex` to write and read manifests from `blobs/` instead of `index/objects/`; added `DIGEST_FILENAME` and manifest-caching in the pull pipeline.
- **Plan 8 — Pull pipeline** (`4f0b8f0`): Hardlink-based package assembly with a parallel layer walker and layer-level singleflight; replaced directory-level symlink assembly (Plan 8b) after surfacing `@loader_path`/`$ORIGIN` correctness failures.
- **Plan 9 — GC** (`6270bd0`): Extended GC to a single BFS pass across all three tiers (`packages/`, `layers/`, `blobs/`) using consolidated `refs/` forward-references.

### D7: Digest file in every CAS entry

**Decided:** Every entry in all three tiers contains a `digest` file with the full digest string.

**Alternatives considered:**
- **Full digest in path**: `{algorithm}/{hex[0..8]}/{hex[8..16]}/{hex[16..64]}` — 48-char last segment. Eliminates the need for a digest file but creates long paths. Windows MAX_PATH with `%APPDATA%` prefix + SHA-512 exceeds 260 chars.
- **Reconstruct from resolve.json**: Only works for packages. Layers and blobs don't have resolve.json.

**Rationale:** The digest file is universal (all three tiers), decouples identity from the sharding implementation (shard depth/length can change), and keeps paths short. The path is for routing; the digest file is for identity.

### D8: Tags as the only mutable index

**Decided:** Replace `index/` with `tags/` (tag → digest mappings only). Manifests move to `blobs/`.

**Alternative:** Keep current index structure with tags and objects subdirectories.

**Rationale:** The current index conflates two concerns: mutable pointers (tags) and immutable content (cached manifests). With manifests in `blobs/`, the index collapses to just tag mappings. `index update` becomes thinner (fetch tag list only, not all manifests). Manifest/index fetching moves to the pull operation.

**UX side note:** The offline-first tags model requires explicit `index update`. A future lazy-fetch mode (resolve on-demand if not cached) would improve UX. The `tags/` structure is compatible with this — just a matter of when the file gets populated.

### D9: Lock to image index digest by default

**Decided:** Lock files pin image index digest. Platform resolution at install time.

**Alternative:** Lock to manifest digest (fully resolved, no platform ambiguity).

**Rationale:** Locking to image index allows different runners (ARM, x86, Ubuntu, Alpine) to resolve different manifests. Future system-requirements feature influences platform selection. Fully-resolved manifest pinning available as opt-in for strict reproducibility. The key insight: if you want full reproducibility, you also need a reproducible environment (same OS, arch, system capabilities) — pinning just the manifest doesn't achieve that without also controlling the execution environment.

### D10: Rename installs/ → symlinks/

**Decided:** Rename the install store directory from `installs/` to `symlinks/`.

**Rationale:** "Installs" is ambiguous — could mean installed content. "Symlinks" describes exactly what's there: candidate symlinks and current symlink pointing to package content.

### D11: Simplify sharding from three levels to one

**Decided:** Change from `{algorithm}/{8hex}/{8hex}/{16hex}` (three-level, 32 hex chars) to `{algorithm}/{2hex}/{remaining_hex}` (one-level prefix, Git-style).

**Alternatives considered:**
- **Keep three levels**: No surveyed tool uses more than one level. The original motivation (Windows directory entry limits) doesn't apply at OCX's scale — NTFS uses B+ tree indexing with O(log n) lookup, and the practical soft limit (~10k entries) is far above OCX's realistic object count (~1,500 for a power user).
- **Fully flat** (`{algorithm}/{full_hex}`): OCI Image Layout spec style. containerd uses this on Windows without issues. Works at OCX's scale but offers no safety margin for edge cases (FAT32 on USB, antivirus scanning tools that enumerate directories).

**Rationale:** One-prefix sharding matches Git, Bazel, pnpm, and Docker Registry v2. 256 buckets keep per-bucket entries low (~6 at power-user scale). Simpler path construction (one join instead of three). Shorter paths help with Windows MAX_PATH. The three-tier redesign is already a breaking change, so sharding depth changes at zero additional migration cost.

**Research artifact:** See NTFS directory limit analysis in the research notes. Key finding: Git's `{2hex}/` sharding was originally for ext2-without-htree and FAT16 (O(n) linear scan). Modern filesystems all use B-tree indexing. The sharding is a conservative safety margin, not a necessity.

## Implementation Notes

Collected during the design discussion. These are pointers for the implementation phase — not a plan, but context that would otherwise be lost.

### Current Code That Changes

**Verified via code inspection** (file:line references to current codebase):

| Component | Current code | What changes |
|-----------|-------------|-------------|
| `ObjectStore::path()` | `object_store.rs:95-100` — joins registry + repo + digest | Remove repo, add digest file write |
| `ObjectStore` struct | `object_store.rs:68-71` — single root | Split into `BlobStore`, `LayerStore`, `PackageStore` |
| `SHARD_DIGEST_LENGTHS` | `object_store.rs:12` — `[8, 8, 16]` = 32 hex chars | Change to `[2]` prefix + remaining (Git-style); update `is_valid_object_path`, `MAX_WALK_DEPTH`, `digest_path` |
| `IndexStore` | `index_store.rs` — tags/ + objects/ subdirs | tags/ moves to top-level `tags/`; objects/ moves to `blobs/` |
| `InstallStore` | `install_store.rs:45-49` — `installs/` root | Rename to `symlinks/`, keep repo in path |
| `FileStructure` | `file_structure.rs:29` — 4 sub-stores | Becomes 6 sub-stores: blobs, layers, packages, tags, symlinks, temp |
| `ReferenceManager` | `reference_manager.rs:36-46` — ref_name = 16 hex of SHA256(path) | Adapt to consolidated `refs/` subdirectories |
| `ReachabilityGraph` | `reachability_graph.rs` — walks objects + deps | Walk three tiers; add CasTier enum on nodes |
| `TempStore::dir_name()` | `temp_store.rs:194-199` — hashes registry+repo+digest | Remove repo from hash input (or keep for compat) |
| `find_in_store()` | `common.rs:29-56` — checks content + metadata + resolve.json | Adapt to new package layout with refs/ subdirectory |
| `resolve_env()` | `resolve.rs:89-119` — reads `dep.identifier` for content paths | Ensure all identifiers use manifest digests (already true) |
| Profile `content_digests()` | `snapshot.rs:36-46` — resolves identifiers via `objects.path()` | Will automatically use new paths once store API changes |

### GC Tier-Awareness

The reachability graph should carry tier info on nodes:

```rust
enum CasTier { Package, Layer, Blob }

struct CasNode {
    path: PathBuf,
    tier: CasTier, // derived from path prefix
}
```

This enables:
- Tier-aware deletion (packages clean up `refs/`; layers remove content; blobs remove data)
- Reporting (`ocx clean --dry-run` → "3 packages, 5 layers, 2 blobs would be removed")
- Validation (refs in `refs/layers/` should resolve to `layers/` paths)

### Resolve.json Dependency Invariant

Dependencies in `resolve.json` must store **manifest digests** (platform-resolved), never image index digests. The resolution chain executes at install time; `resolve.json` captures the result. This is already true in the current implementation (`PinnedIdentifier` comes from the resolved manifest in `pull.rs:238-244`), but must be explicitly tested.

### Assembly Walker Structure

Hardlink-based assembly. See D6 for the full decision. The assembly logic must:

- **Single-layer**: Walker mirrors `layers/{digest}/content/` into `packages/{P}/content/`. Directories become real directories; regular files become hardlinks via `hardlink::create`; intra-layer symlinks are recreated verbatim (target strings preserved).
- **Multi-layer** (deferred to #22): Walker runs once per layer into the same `packages/{P}/content/`. Layers are validated as non-overlapping subtrees at extraction time. The walker does not resolve interleaving conflicts.
- **Validation**: At extraction time, verify no two layers contribute files to the same directory (non-overlapping subtrees). Reject interleaving layers with a clear error before assembly starts.
- **Cross-volume**: `io::ErrorKind::CrossesDevices` surfaces as a clean install-time error pointing at the single-volume `$OCX_HOME` requirement. No silent copy fallback.
- **Windows**: Regular files use `std::fs::hard_link`. Intra-layer file symlinks fall back to `tokio::fs::copy` of the dereferenced target because the existing `symlink::create` uses junctions (directory-only, absolute-only) which do not fit walker semantics. Intra-layer directory symlinks are not supported and return an error.
- **Mount points (#31)**: Mount symlinks are still created inside the package's `content/` directory as symlinks pointing to dependency content. Mounts cross package boundaries and must remain symlinks even in the hardlink assembly model.

### Import/Export Consideration

Hardlinks materialize naturally in portable archives — `tar` preserves hardlink identity across files by default, and `cp -a` or `rsync -H` preserve them for directory copies. The `ocx export` command design does not need special handling for symlink dereferencing inside package content, because there are no cross-tier symlinks in `packages/.../content/`. Cross-tier forward-ref symlinks (`refs/layers/`, `refs/blobs/`, `refs/deps/`) still need `-L` dereferencing or separate archival when exporting a single package in isolation.

## Validation

- [ ] Acceptance tests pass with new layout (all existing tests + new layout-specific tests)
- [ ] GC correctly collects unreachable entries across all three tiers
- [ ] Cross-repo dedup verified: same layer digest from two repos → one layers/ entry
- [ ] Hardlinks survive `mv $OCX_HOME /new/path` on the same volume
- [ ] Cross-volume hardlink failures surface as clear `CrossesDevices` errors (not silently dropped)
- [ ] Hardlinks created inside the temp directory survive the atomic `temp → packages/` rename (inode unchanged)
- [ ] Windows path lengths verified within MAX_PATH for SHA-256 and SHA-512
- [ ] `ocx clean --dry-run` reports tier-aware counts
- [ ] Digest file content matches path-derived partial digest
- [ ] resolve.json dependencies all carry manifest digests (not image index digests)
- [ ] Binary with `$ORIGIN`/`@loader_path` relative RPATH resolves libs inside the package (not in layers/)
- [ ] Intra-layer symlink chain (e.g., `libfoo.so → libfoo.so.1 → libfoo.so.1.2.3`) reproduces in the assembled package with identical target strings

## Links

- [#27 — bug: object store includes repository in path](https://github.com/ocx-sh/ocx/issues/27)
- [#22 — Multi-Layer Packages PR](https://github.com/ocx-sh/ocx/pull/22)
- [#33 — feat: project-level toolchain config](https://github.com/ocx-sh/ocx/issues/33)
- [#24 — feat: OCI referrers API](https://github.com/ocx-sh/ocx/issues/24)
- [#31 — feat: mount dependencies](https://github.com/ocx-sh/ocx/issues/31)
- [#35 — feat: policy-based retention for unreferenced blobs](https://github.com/ocx-sh/ocx/issues/35)
- [Research: Content-Addressed Storage](./research_content_addressed_storage.md)
- [System Design: Composition Model](./system_design_composition_model.md)
- [ADR: Package Layers](./adr_package_layers.md) (PR #22)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-06 | mherwig | Initial draft from design discussion |
| 2026-04-10 | mherwig / Claude | Revised D6: hardlink-based assembly with symlink preservation. Directory-level symlinks moved to Alternatives rejected after Plan 8b implementation surfaced complexity (filename-based dispatch in `package_dir_for_content`, `.readlink()` workarounds in test helpers, `refs/layers/` child-path conventions) and research confirmed no surveyed general-purpose binary package manager uses directory-level symlink assembly. Walker mirrors layer `content/` trees into package `content/` as real directories; regular files hardlinked via `hardlink::create`; intra-layer symlinks preserved verbatim on Unix; Windows file symlinks fall back to `fs::copy` of dereferenced target (existing `symlink::create` uses junctions which are directory-only and absolute-only, unfit for walker use — junctions remain appropriate for install candidate/current symlinks with absolute package-dir targets); Windows directory symlinks error out (deferred, rare). New invariants: no file in `packages/{P}/content/` is a symlink into `layers/`; walker preserves target strings verbatim; `$OCX_HOME` must live on a single volume (inherited from pre-existing `temp → packages/` atomic-rename constraint). Install Flow section updated to describe the walker. Non-Goals unchanged (reflink tier deferred, multi-layer merging deferred to #22, blob eviction policy #35). |
| 2026-04-10 | mherwig / Claude | D6 follow-up simplification: dropped `hardlink::create_or_copy` and `LinkMethod` enum. The hardlink module now has only `create` and `update`. Cross-device surfaces as a clear `CrossesDevices` error at install time instead of silently copying. Rationale: `temp → packages/` already requires same-fs; `$OCX_HOME` is inherently single-volume; adding a silent copy fallback hid a dedup regression without user signal. Added invariant: hardlinks created in temp survive the atomic rename (verified by unit test `hardlink_survives_directory_rename`, locking in POSIX `rename(2)` inode preservation). |
