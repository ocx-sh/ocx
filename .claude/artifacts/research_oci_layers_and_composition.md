# Research: OCI Layer Spec, Multi-Layer Composition, and Overlap-Free Merging

**Date:** 2026-03-28
**Context:** Research for ocx-sh/ocx#20 (Supporting Layers)

---

## 1. OCI Layer Specification — Technical Detail

### Two digests per layer

- `manifest.layers[i].digest` = SHA-256 of the **compressed** blob (used for registry fetch and storage dedup)
- `config.rootfs.diff_ids[i]` = SHA-256 of the **uncompressed** tar = the "DiffID" (canonical content identity, independent of compression)

### Changeset semantics (three entry types)

| Entry | Meaning |
|-------|---------|
| Regular tar entry | Add or replace file at that path. Upper layer shadows lower. |
| `.wh.<name>` (whiteout) | Delete `<name>` from lower layers. Must be an empty file. |
| `.wh..wh..opq` (opaque whiteout) | Delete all children of the containing directory from lower layers. Applied *before* sibling entries regardless of tar archive ordering. |

Whiteouts MUST only apply to lower/parent layers; they cannot delete files introduced in the same layer.

### Sequential application

Layers applied bottom-up (index 0 first). Later layers shadow earlier layers on path conflicts.

### ChainID — position-sensitive cryptographic identity

```
ChainID(L0)        = DiffID(L0)
ChainID(L0|L1)     = SHA256(ChainID(L0) + " " + DiffID(L1))
ChainID(L0|...|Ln) = SHA256(ChainID(L0|...|Ln-1) + " " + DiffID(Ln))
```

Binds each layer to its position in the stack. If the runtime has a cached snapshot for ChainID(L0|L1), it can use it regardless of how many images share those bottom layers.

### Deduplication limits

- Dedup granularity = whole layer blob. One changed byte = new layer digest = full re-download.
- No cross-layer file-level dedup. Identical files in two different layers stored twice.
- Compression defeats file-level equality: same file content in two different tar archives produces different bytes (different dictionary context).

### Tar format legacy problems (Cyphar's OCIv2 analysis)

Tar entry ordering is nondeterministic (filesystem `readdir` order varies by OS and filesystem), so identical builds on different machines produce different layer digests even with identical file content. OCI inherited AUFS's on-disk tar format from Docker.

---

## 2. Package Manager Composition and Deduplication

### Nix (gold standard for isolation)

- Every package at `/nix/store/<hash>-<name>-<version>/`. Hash covers all build inputs.
- **Structural non-overlap**: two packages can never share a path — each under unique hash directory. Overlap physically impossible.
- **Profile composability**: "installed environment" is a symlink tree pointing into the store. No files copied for installation. Composition = additive symlink merging.
- Runtime dep tracking: scan binaries for embedded store path strings (RPATHs).

### Bazel

- Build actions declare exact inputs; action sandbox contains only declared inputs.
- Actions cached in CAS keyed by hash of (command + inputs + environment).
- `rules_oci` maps each Bazel target to one OCI layer. Targets cannot produce same output path — overlap prevented at action graph level.

### npm/pnpm

- npm hoisting: compatible packages promoted to root `node_modules`; conflicting versions nested.
- pnpm: global content-addressed store at `~/.pnpm-store`; `node_modules` populated with **hard links**. Identical files across all packages stored once at the inode level.

### Cargo

- Source compilation model; feature unification within a single build.
- `cargo-binstall`: installs pre-built binaries as flat files to `~/.cargo/bin`. No dedup, no overlay.

### apk (Alpine)

- Constraints in `/etc/apk/world`. SAT solver on every transaction. Refuses to commit inconsistent states.
- File conflict = hard error unless `Replaces:` declared. No content-addressed store.

### dpkg (Debian)

- File ownership database per package (`/var/lib/dpkg/info/*.list`). Every file owned by exactly one package.
- `Conflicts:` — two packages owning the same path refuse to coexist.
- `Replaces:` — package A takes ownership of files from package B.
- `dpkg-divert` — redirect a file to a different path to resolve conflicts without full Replaces.

---

## 3. OCI Artifacts Beyond Containers

### ORAS (OCI Registry as Storage)

- Same structure as OCI images: config descriptor + arbitrary layers array.
- Layer count arbitrary; `mediaType` (not ordering) communicates what each layer is.
- OCI v1.1 (Feb 2024): `artifactType` field on manifests; `subject` field for Referrers API.
- Registry-level dedup: identical blob digests stored once across all artifacts.

### Helm charts

- One layer: entire chart as `application/vnd.cncf.helm.chart.content.v1.tar+gzip`.
- Optional second layer: provenance file (`.prov`).
- No inter-chart dedup — chart dependencies bundled inside the single tarball.

### Ollama (LLM model distribution — most relevant binary dedup example)

- Multi-layer manifest: config layer (metadata), weight layer(s) (raw GGUF files), template layer, parameters layer.
- Blobs stored at `~/.ollama/models/blobs/sha256-<digest>` — raw files, not tar archives.
- **Cross-model deduplication**: if `llama3:8b` and `llama3:8b-instruct` share a base weight blob, they reference the same digest — stored once.
- No filesystem overlay — layers accessed directly as files, not extracted into merged directory.

### WASM modules

Typically single layer (raw `.wasm` binary). No established multi-layer composition pattern.

---

## 4. Overlap-Free Layer Merging

### overlayfs kernel semantics

```
mount -t overlay overlay -o lowerdir=/l3:/l2:/l1,upperdir=/upper,workdir=/work /merged
```

- Leftmost lower = highest priority. Upper always takes precedence over all lower.
- File in both upper and lower: upper is visible; lower hidden entirely.
- Directory in both: merged view; upper entry wins on name conflict.
- **No enforcement of non-overlap** — overlap is the core mechanism.

### Who enforces overlap-free trees

| System | Mechanism | Level |
|--------|-----------|-------|
| Nix | Hash-addressed unique path per derivation | Structural — physically impossible |
| Bazel + rules_oci | Targets cannot share output paths | Build system |
| dpkg | File ownership DB + `Conflicts:` + `Replaces:` | Package metadata + install-time check |
| apk | SAT solver rejects unsatisfiable constraints | Install-time solver |
| OCI spec | None — later layers shadow earlier | No enforcement |
| overlayfs | None — upper always shadows lower | No enforcement |

**Conclusion**: no production filesystem-level system rejects layer overlap. Enforcement is always at a higher level.

---

## 5. Technology Landscape

### Trending

| Tool/Pattern | Signal | OCX Relevance |
|-------------|--------|---------------|
| `zstd:chunked` layers | containerd support; growing CI adoption | Relevant if OCX packages grow large |
| Nydus (Dragonfly) | CNCF incubating; production at Alibaba | Future lazy-pull model |
| ORAS v1.1 + Referrers API | OCI spec ratified Feb 2024; ECR/GHCR support | Already relevant to OCX artifact publishing |
| pnpm hard-link store | 38k+ GitHub stars | Comparable to OCX content-addressed object store |

### Established

| Tool/Pattern | Status |
|-------------|--------|
| OCI tar-based layers | Mature/Standard |
| Nix isolated store paths | Gold standard for binary isolation |
| overlayfs | Linux kernel standard |
| dpkg Conflicts/Replaces | De facto for OS-level package conflict resolution |

### Emerging

| Tool/Pattern | Worth Watching |
|-------------|----------------|
| OCIv2 (new image format) | Could replace tar with content-tree format |
| Ollama-pattern multi-layer binary distribution | Most practical model for multi-component binary dedup on OCI |

### Declining

| Tool/Pattern | Avoid |
|-------------|-------|
| eStargz | Superseded by Nydus and `zstd:chunked` |

---

## 6. Design Patterns Worth Considering

- **Per-component `mediaType` layers (Ollama pattern)** — separate layers with distinct `mediaType` annotations enable component-level dedup at the registry without tar overhead.
- **Profile symlink trees (Nix pattern)** — OCX's `installs/` tree already follows this. Composition = additive symlink union.
- **Hard-link global content store (pnpm pattern)** — if cross-package file-level dedup becomes priority, hard links from file-hash-keyed global store is the model.
- **Opaque whiteout for destructive layer transitions** — relevant if OCX ever produces multi-layer artifacts representing upgrades/patches.

---

## Recommendation for OCX

OCX's content-addressed `objects/` store + `installs/` symlink tree is structurally equivalent to the Nix model — the right architecture for binary package management.

**Do not use OCI layers for cross-package file deduplication.** Layer-granularity dedup is too coarse. The right dedup unit is the whole package version (one digest = one object directory), which OCX already implements.

**If per-component layers are needed** (binary + large data asset + shared runtime), adopt the Ollama pattern: separate layers with `mediaType` annotations, raw content-addressed blobs, no tar wrapping.

**For overlap detection**, implement at the OCX metadata layer — a file-ownership database similar to dpkg. Do not rely on overlayfs or OCI layer semantics.

**Watch `zstd:chunked`** — first path to cross-package file-level dedup at the registry level.

---

## Sources

| Source | Type | Date |
|--------|------|------|
| [OCI Layer Spec](https://specs.opencontainers.org/image-spec/layer/) | Spec | Maintained |
| [OCI Image Config](https://github.com/opencontainers/image-spec/blob/main/config.md) | Spec | Maintained |
| [The Road to OCIv2 — Cyphar](https://www.cyphar.com/blog/post/20190121-ociv2-images-i-tar) | Blog | Jan 2019 |
| [OCIv2 Brainstorm — HackMD](https://hackmd.io/@cyphar/ociv2-brainstorm) | Design | 2019+ |
| [ORAS Concepts: Artifact](https://oras.land/docs/concepts/artifact/) | Docs | 2024 |
| [Helm OCI MediaTypes](https://helm.sh/blog/helm-oci-mediatypes/) | Blog | 2021 |
| [Ollama Model Registry — DeepWiki](https://deepwiki.com/ollama/ollama/4.2-model-registry-and-layers) | Analysis | 2024 |
| [Linux Kernel overlayfs](https://docs.kernel.org/filesystems/overlayfs.html) | Docs | Maintained |
| [Nix Store Path Spec](https://nix.dev/manual/nix/2.22/protocols/store-path) | Spec | Maintained |
| [Debian Policy: Relationships](https://www.debian.org/doc/debian-policy/ch-relationships.html) | Spec | Maintained |
| [npm dedupe docs](https://docs.npmjs.com/cli/v11/commands/npm-dedupe/) | Docs | 2024 |
| [What Package Registries Could Borrow from OCI](https://nesbitt.io/2026/02/18/what-package-registries-could-borrow-from-oci.html) | Blog | Feb 2026 |
| [Building Container Layers from Scratch — Depot](https://depot.dev/blog/building-container-layers-from-scratch) | Blog | 2023 |
| [Nydus/Dragonfly — CNCF](https://www.cncf.io/blog/2020/10/20/introducing-nydus-dragonfly-container-image-service/) | Blog | 2020 |
| [Bazel rules_oci](https://github.com/bazel-contrib/rules_oci) | Repo | 2024 |
