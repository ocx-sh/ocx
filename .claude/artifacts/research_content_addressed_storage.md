# Research: Content-Addressed Storage in Package Managers

## Metadata

**Date:** 2026-04-06
**Domain:** packaging | infrastructure
**Triggered by:** Storage redesign (issues #27, #22, #33, #24) — introducing blobs/, layers/, packages/ tiers
**Expires:** 2027-04-06

## Direct Answer

Seven systems (Nix, containerd, OCI Image Layout, Bazel, pnpm, Cargo, Git) implement content-addressed storage with fundamentally different models. OCX's proposed three-tier design (raw blobs / extracted layers / assembled packages) is most closely validated by containerd's two-stage pull model (content store + snapshotter) and Cargo's three-tier cache (index / .crate / src). The key OCX-specific extension is relative symlinks for relocatable assembly — a pattern validated by pnpm's virtual store but novel in the binary package manager space.

## Technology Landscape

### Trending (gaining momentum)

| Tool/Pattern | Adoption Signal | Key Benefit | Relevance to OCX |
|-------------|----------------|-------------|-------------------|
| Reflinks as dedup primitive | pnpm, DVC, conda added `reflink,copy` fallback 2023-2025 | Zero-cost COW on supported filesystems | Future optimization for content/ assembly |
| OCI artifacts v1.1 for non-container payloads | WASM, Helm, binary packages converging on OCI dist-spec | Standard distribution for any artifact type | OCX is ahead of this curve |
| Content-addressed derivations (Nix CA) | Nix 2.4+ experimental, Tweag investment | True output-addressed dedup | Validates OCX's output-addressed model |
| Sub-file CDC dedup | Bazel remote APIs added `SplitBlob`/`SpliceBlob` RPCs | Fine-grained dedup for large artifacts | Future `ocx store optimise` |

### Established (proven, widely accepted)

| Tool/Pattern | Status | Notes |
|-------------|--------|-------|
| 2-char prefix sharding | Standard (Git, Bazel, pnpm) | 256-bucket fan-out, proven at scale |
| Blob/extracted two-tier separation | Mature (containerd, Cargo) | Separates transfer artifacts from usable content |
| Symlink stable refs + CAS backing | Mature (pnpm, NixOS profiles) | Stable paths for consumers, content-addressed storage |
| BFS mark-sweep GC from roots | Mature (Nix, Git) | Scales with graph size, not content volume |

### Emerging (early but promising)

| Tool/Pattern | Signal | Worth Watching Because |
|-------------|--------|----------------------|
| Relative symlinks for global stores | pnpm experimental, Lerna PR #435 | Enables relocatable stores without hardlink constraints |
| Filesystem-embedded reference graphs | OCX deps/ pattern | Eliminates external DB (unlike containerd's bbolt) |

### Declining (losing mindshare)

| Tool/Pattern | Signal | Avoid Because |
|-------------|--------|---------------|
| Per-project copies without dedup | npm classic, old pip | Massive disk waste; losing to pnpm/uv |
| Input-addressed-only store paths | Nix classic | CA derivations are the roadmap; output-addressed is more natural |
| Unbounded caches without GC | Bazel local, old Cargo | User complaint magnet; Cargo added SQLite GC in 1.75 |

## Surveyed Systems

### 1. Nix Store

**Layout:** `/nix/store/{32-char-base32-hash}-{name}/` — hash of *inputs*, not output content. Store paths contain extracted, immediately usable content.

**Content addressing:** Input-addressed by default. 32-char base-32 = truncated SHA-256 (first 160 bits) of a string encoding source hash + deps + build commands + store prefix. Experimental CA derivations (opt-in since Nix 2.4) hash *output content* instead.

**Dedup:** No per-file dedup at install time. Post-hoc `nix-store --optimise` hardlinks identical files across derivation outputs.

**GC:** Roots at `/nix/var/nix/gcroots/` (traversed recursively). Reachability determined by scanning *all file content* in live store paths for embedded `/nix/store/...` strings. O(total file content), not O(graph edges). Dead paths atomically moved to `/nix/store/trash`.

**Blob vs extracted:** No separation. Store IS extracted content.

### 2. Docker/containerd

**Layout:**
```
/var/lib/containerd/
  io.containerd.content.v1.content/blobs/sha256/{64hex}     # raw compressed blobs
  io.containerd.snapshotter.v1.overlayfs/snapshots/{id}/fs/  # extracted layers
  io.containerd.metadata.v1.bolt/meta.db                     # bbolt metadata
```

**Content addressing:** Full SHA-256 hex as flat filename in `blobs/sha256/`. No sharding.

**Layer sharing:** Same-digest layers share one blob and one snapshot. Snapshot tree is parent-linked; shared base layers reuse the chain.

**GC:** Label-based mark-sweep. Labels on content objects (`containerd.io/gc.ref.content.l.{n}`) protect referenced blobs. Leases with TTL protect in-flight operations. Runs as background goroutine.

**Blob vs extracted:** Strict separation. `content/` = compressed OCI blobs. `snapshots/` = extracted mount-ready layers. Two independent stores. The hash for transfer (compressed blob) differs from the hash for assembly (chain ID of uncompressed content).

### 3. OCI Image Layout Spec (v1.1)

**Layout:** `blobs/{algorithm}/{full-hex}` — content MUST match `{algorithm}:{hex}`. Full digest as filename. No sharding specified. Additional files in the layout are explicitly allowed.

**Scope:** Defines local filesystem representation for transport (docker save, ORAS, Skopeo). Does NOT specify extraction, assembly, or caching. Registry scoping, sharding, and everything beyond `blobs/` is implementation-defined.

### 4. Bazel CAS

**Layout:** `{cache}/cas/{2hex}/{full-sha256}` (content) + `{cache}/ac/{2hex}/{full-sha256}` (actions). Full hash in path with 2-char prefix shard.

**GC:** None built-in. `bazel-remote` adds LRU eviction. Local caches grow unbounded.

**Blob vs extracted:** Strict. CAS stores raw bytes. Build system handles extraction.

### 5. pnpm

**Layout:** `~/.pnpm-store/v3/files/{2hex}/{remaining-sha512}` — per-*file* content addressing (not per-package). SHA-512 with 2-char prefix.

**Dedup:** Hardlinks from global store to per-project `.pnpm/` virtual store. Same-filesystem requirement; falls back to copying. Hardlinks chosen over symlinks because Node.js `require()` resolves from the file's real location.

**GC:** `pnpm store prune` — scans installed `node_modules/.pnpm/` to find live references. No background GC.

**Blob vs extracted:** No separation. Store contains individual extracted files.

### 6. Cargo

**Layout:** `~/.cargo/registry/cache/{reg}/{name}-{ver}.crate` (raw tarballs) + `~/.cargo/registry/src/{reg}/{name}-{ver}/` (extracted). NOT content-addressed — keyed by name+version, integrity verified against checksums.

**GC:** Since Rust 1.75, SQLite-backed last-use tracking. `cargo clean --max-download-age` prunes.

**Blob vs extracted:** Explicit. `cache/` = tarballs, `src/` = extracted. No enforced integrity link at runtime.

### 7. Git Object Store

**Layout:** `.git/objects/{2hex}/{38hex}` — full SHA-1 in path with 2-char prefix. Pack files compress loose objects with delta encoding.

**GC:** Roots = all refs (branches, tags, HEAD). Grace period (default 14 days) before pruning unreachable loose objects. Auto-triggered at 6700 loose objects.

## Design Patterns Worth Considering

- **Two-stage pull with raw/extracted separation** — containerd's model. Separates immutable transfer artifact from extraction. Enables GC at two independent granularities. Used by: containerd, Cargo.
- **Forward-ref graph embedded in filesystem** — symlinks as the reference graph. No external DB. The filesystem IS the database. Used by: OCX (current deps/ pattern). Novel in the package manager space.
- **Registry-prefix in CAS paths** — enables multi-registry support without digest collision ambiguity. Used by: Cargo (hostname + hash as directory name).
- **Relative symlinks for relocatable stores** — enables moving the entire store directory without breaking references. Used by: Lerna (switched from absolute to relative for Heroku deployment). pnpm experimental global virtual store.
- **Digest file alongside truncated path** — stores full identity in a file rather than encoding it entirely in the path. Enables shorter paths (Windows MAX_PATH) while preserving full identity. No direct precedent found in surveyed systems.

## Key Findings

1. **containerd's content-store/snapshotter split directly validates blobs/ → layers/ separation.** The compressed-blob hash differs from the chain-ID used for assembly, justifying two tiers with different keys. ([containerd content-flow.md](https://github.com/containerd/containerd/blob/main/docs/content-flow.md))
2. **Nix truncates the in-path hash to 160 bits (base32).** This is of a different input string (derivation inputs, not content bytes), but the truncation technique is established. No separate digest file — Nix relies on the `.drv` file for full identity. ([Nix Pills ch.18](https://nixos.org/guides/nix-pills/18-nix-store-paths))
3. **OCI Image Layout v1.1 mandates `blobs/{alg}/{hex}` matching but leaves sharding, registry scoping, and extraction unspecified.** Additional files alongside blobs are explicitly allowed. ([OCI Image Layout Spec](https://github.com/opencontainers/image-spec/blob/main/image-layout.md))
4. **Windows MAX_PATH (260 chars) is a real constraint.** Storing 32 of 64 SHA-256 hex chars saves 32 characters vs Docker's flat approach. With `%APPDATA%` prefix (~35 chars) + OCX structure, SHA-512 full-path would exceed limits. ([Microsoft MAX_PATH docs](https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation))
5. **BFS from roots is sound for DAG-structured dependency graphs.** Reference counting cannot handle cycles; mark-sweep handles cycles but requires care around concurrent mutations. OCX's DAG enforcement eliminates cycles at the source. ([Tracing GC — Wikipedia](https://en.wikipedia.org/wiki/Tracing_garbage_collection))
6. **OCI 1.1 Referrers API adds `subject` field linking attestations to manifests.** A local referrers graph can be reconstructed from stored blob manifests without a local API implementation. ([OCI 1.1 Release Blog](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/))

## Symlinks vs Hardlinks for Content Assembly

### The Problem

When a package manager assembles content from a shared layer store into a per-package content directory, it must choose a linking strategy. The choice affects compatibility, portability, and dedup behavior.

### macOS `@loader_path` / `@executable_path` (Critical Finding)

Mach-O binaries use `@loader_path` and `@executable_path` for relative dylib resolution. macOS dyld resolves these from the **real path** of the binary (after symlink dereferencing). If a binary at `layers/{A}/content/bin/tool` is accessed via a symlink at `packages/.../content/bin/tool`, `@loader_path` resolves to `layers/{A}/content/bin/`, not `packages/.../content/bin/`. A relative reference like `@loader_path/../lib/libfoo.dylib` then looks in `layers/{A}/content/lib/`, which may not exist if `lib/` is in a different layer.

**Implication:** Per-file symlinks across layers break macOS relative dylib resolution. Directory-level symlinks are safe when the binary and its co-located resources are in the same layer (same real directory tree). ([iTwenty rpath explainer](https://itwenty.me/posts/01-understanding-rpath/), [mike.ash.com linking Q&A](https://www.mikeash.com/pyblog/friday-qa-2009-11-06-linking-and-install-names.html))

### ELF `$ORIGIN` (Linux)

ELF binaries use `$ORIGIN` in `DT_RPATH`/`DT_RUNPATH` for relative shared library lookup. On Linux, `$ORIGIN` resolves via `/proc/self/exe` which always returns the **real path** (follows symlinks). Same behavior as macOS — `$ORIGIN/../lib/` from a symlinked binary resolves relative to the layer, not the package. ([man ld.so(8)](https://man7.org/linux/man-pages/man8/ld.so.8.html))

### Shell Scripts `$(dirname "$0")`

`$0` in shell scripts contains the invocation path — it does **not** follow symlinks. A script invoked via an install symlink will compute the symlink's directory, not the content directory. Scripts using `$(dirname "$0")` to locate co-located resources will fail when invoked through install symlinks. The workaround (`readlink -f "$0"`) is not portable to stock macOS before 12.3. ([bashup/realpaths](https://github.com/bashup/realpaths))

### Node.js Module Resolution

Node.js dereferences symlinks and resolves `require()` from the **real path**. This is why pnpm uses hardlinks (not symlinks) from its content store into the virtual store — a symlink would cause Node to resolve peer dependencies from the store, not from `node_modules/`. Not relevant to OCX (native binaries, not Node modules) but instructive for the general pattern. ([pnpm discussion #6800](https://github.com/orgs/pnpm/discussions/6800))

### Windows: Junctions vs Symlinks

NTFS symlinks require Developer Mode or admin privileges for creation. NTFS junction points require no privileges and are directory-only. OCX already uses junctions on Windows (`symlink.rs`). Junctions store absolute paths (not relative), but OCX constructs absolute targets from the current directory. ([hy2k.dev Windows links guide](https://hy2k.dev/en/blog/2025/11-23-windows-hardlink-symlink-junction/), [Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/fileio/hard-links-and-junctions))

### Hard Link Limitations

| Limitation | Impact |
|-----------|--------|
| Can't cross filesystem boundaries | Rules out OCX_HOME on different mount than install target |
| Can't hardlink directories | Only file-level; need separate mechanism for directory structure |
| Mutation propagates to all links | Incompatible with shared immutable layers if any post-processing occurs |
| `du` reports misleading sizes | Confusing disk usage reporting in deduped stores |
| Not relocatable | Moving OCX_HOME doesn't update hardlinks (same inode, not path-based) |
| Backup tools may expand | rsync without `-H` creates full copies; Time Machine handles correctly on APFS |

### What Other Package Managers Use

| Tool | Mechanism | Why |
|------|-----------|-----|
| pnpm | Hardlinks (store → virtual) + symlinks (virtual → node_modules) | Node.js resolves from real path; hardlinks make files appear at correct path |
| Nix | Symlinks for profiles; hardlinks for `nix-store --optimise` | Store is read-only → hardlinks safe; optimise is post-hoc dedup |
| Homebrew | Symlinks from `$(brew --prefix)/bin/` → Cellar | Version switching by relinking |
| uv | Hardlinks from store → per-venv site-packages | Python doesn't have Node's resolution issue; hardlinks for same-FS dedup |
| Docker | overlayfs (kernel union mount) | Not applicable to user-space package managers |
| DVC | Reflinks (preferred) → hardlinks → copies | Avoids symlinks for ML data; reflinks give COW isolation |

### Reflinks as Future Alternative

Copy-on-write clones: near-instant, no space overhead until data diverges, independent modification (unlike hardlinks). Supported on APFS (macOS), Btrfs, XFS (Linux), ReFS (Windows Enterprise). **Not supported on ext4 or NTFS** — the most common Linux and Windows filesystems. Worth tracking as opt-in optimization. ([DVC docs](https://dvc.org/doc/user-guide/data-management/large-dataset-optimization), [NixOS discourse](https://discourse.nixos.org/t/use-of-shallow-copy-reflinks-on-btrfs-xfs-zfs/24430))

### Recommendation for OCX

**Directory-level symlinks** for layer assembly. Never per-file symlinks. The single-layer case (one directory symlink) is trivially safe. Multi-layer requires non-overlapping subtrees — shared parent directories are real directories in the package, with per-subtree symlinks into respective layers. This avoids the `@loader_path`/`$ORIGIN` problem entirely as long as publishers don't create cross-layer relative path references (a documented publisher constraint).

## Recommendation

The three-tier design is well-grounded in industry practice. Three choices are particularly defensible:

1. **Digest-keyed objects** (not version-keyed like Cargo, not input-keyed like Nix) — true content dedup with no accidental identity collisions.
2. **Three-tier separation** with no auxiliary database — cleaner lifecycle separation than any surveyed system, and filesystem-native (no bbolt like containerd).
3. **BFS GC with forward-ref edge-list** — scales O(graph size), not O(file content volume) like Nix.

Two patterns worth tracking for future investment:
- **Reflinks** for zero-cost COW on Btrfs/XFS/APFS (`reflink,copy` fallback in assembly).
- **File-level dedup pass** (`ocx store optimise`) hardlinking identical files across content/ directories.

## Sources

| Source | Type | Date | Relevance |
|--------|------|------|-----------|
| [Nix Store Paths — Nix Pills ch.18](https://nixos.org/guides/nix-pills/18-nix-store-paths) | Docs | Current | Hash computation, input-addressed vs CA |
| [Nix Garbage Collector — Nix Pills ch.11](https://nixos.org/guides/nix-pills/11-garbage-collector.html) | Docs | Current | GC roots, reachability, deletion |
| [Nix GC Roots — Manual](https://nix.dev/manual/nix/2.33/package-management/garbage-collector-roots) | Docs | Current | gcroots/ structure, indirect roots |
| [CA Derivations — Tweag](https://www.tweag.io/blog/2021-12-02-nix-cas-4/) | Blog | 2021 | Nix CA derivations design and motivation |
| [containerd content-flow.md](https://github.com/containerd/containerd/blob/main/docs/content-flow.md) | Docs | Current | Content store layout, snapshot pipeline |
| [containerd garbage-collection.md](https://github.com/containerd/containerd/blob/main/docs/garbage-collection.md) | Docs | Current | Label-based mark-sweep, leases |
| [containerd internals — samuel.karp.dev](https://samuel.karp.dev/blog/2024/12/containerd-internals-images/) | Blog | 2024 | Chain ID vs digest, compressed vs uncompressed |
| [How Containerd Stores Images](https://midbai.com/en/post/how-containerd-image-store/) | Blog | Current | On-disk layout walkthrough |
| [OCI Image Layout Spec v1.1](https://github.com/opencontainers/image-spec/blob/main/image-layout.md) | Spec | 2024 | blobs/{alg}/{hex} structure, allowed extensions |
| [OCI 1.1 Release Blog](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/) | Blog | 2024 | Referrers API, subject field |
| [Bazel Remote Caching](https://bazel.build/remote/caching) | Docs | Current | CAS and action cache semantics |
| [Bazel cache internals](https://ejameslin.github.io/Bazel-cache-behind-the-scenes/) | Blog | Current | AC/CAS 2-char sharding on disk |
| [pnpm Motivation](https://pnpm.io/motivation) | Docs | Current | Content-addressable store, hardlink rationale |
| [pnpm Symlinked node_modules](https://pnpm.io/symlinked-node-modules-structure) | Docs | Current | Two-layer model, virtual store |
| [pnpm FAQ](https://pnpm.io/faq) | Docs | Current | Hardlinks vs symlinks, same-filesystem constraint |
| [Cargo Home](https://doc.rust-lang.org/cargo/guide/cargo-home.html) | Docs | Current | registry/cache and registry/src layout |
| [Cargo Cache Cleaning](https://blog.rust-lang.org/2023/12/11/cargo-cache-cleaning/) | Blog | 2023 | SQLite-backed GC, max-download-age |
| [Git Database Internals — GitHub Blog](https://github.blog/open-source/git/gits-database-internals-i-packed-object-store/) | Blog | Current | Loose objects, packs, delta compression, GC |
| [DVC Large Dataset Optimization](https://dvc.org/doc/user-guide/data-management/large-dataset-optimization) | Docs | Current | reflink > hardlink > symlink > copy hierarchy |
| [Lerna relative symlinks PR #435](https://github.com/lerna/lerna/pull/435) | GitHub | 2017 | Absolute vs relative symlink portability |
| [Docker registry GC](https://gbougeard.github.io/blog.english/2017/05/20/How-to-clean-a-docker-registry-v2.html) | Blog | 2017 | Two-phase GC, read-only mode requirement |
| [Windows MAX_PATH](https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation) | Docs | Current | 260-char path length constraints |
| [iTwenty rpath explainer](https://itwenty.me/posts/01-understanding-rpath/) | Blog | Current | macOS @executable_path/@loader_path resolution |
| [mike.ash.com linking Q&A](https://www.mikeash.com/pyblog/friday-qa-2009-11-06-linking-and-install-names.html) | Blog | 2009 | Mach-O install names and dylib paths |
| [pnpm discussion #6800](https://github.com/orgs/pnpm/discussions/6800) | Discussion | Current | Why pnpm uses hardlinks not symlinks |
| [hy2k.dev Windows links guide](https://hy2k.dev/en/blog/2025/11-23-windows-hardlink-symlink-junction/) | Blog | 2025 | Junction vs symlink vs hardlink on Windows |
| [rust-lang/rust#43617](https://github.com/rust-lang/rust/issues/43617) | GitHub | Current | current_exe() symlink inconsistency |
| [LWN: A walk among the symlinks](https://lwn.net/Articles/650786/) | Article | 2015 | Linux kernel symlink depth limit, RCU-walk |
| [NixOS Storage optimization](https://wiki.nixos.org/wiki/Storage_optimization) | Docs | Current | Nix hardlink dedup rationale |
