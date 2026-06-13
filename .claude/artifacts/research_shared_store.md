# Research: Sharing an OCX store across DevContainer instances

Supporting research for [`adr_devcontainer_shared_store.md`](./adr_devcontainer_shared_store.md).
Produced by a multi-agent discovery + web-research + adversarial-verification workflow (2026-06-13).

## 1. OCX storage internals (code-level)

### 1.1 Publish discipline differs per tier

| Tier | Primitive | Destination removed first? | Cross-device | Concurrent same-digest publish |
|------|-----------|---------------------------|--------------|--------------------------------|
| **Blob** | `NamedTempFile` in CAS parent + `persist`/`rename` (file) — `blob_store.rs:132-135` | No (atomic file overwrite) | impossible (temp co-located) | **Safe** — content-addressed, idempotent rename, present-target = success (`blob_store.rs:143`) |
| **Layer** | tempdir + bare `tokio::fs::rename` (dir) — `layer_staging.rs:40` | No — winner kept, loser discards own temp (`:45-48`) | `Err` → `InternalFile` (`:50-53`) | **Safe-ish** — temp lock + non-destructive rename; `Err(_)` race guard over-swallows; no layer completeness marker |
| **Package** | tempdir + `move_dir` = `remove_dir_all(dst)` + `rename` (dir) — `pull.rs:653`, `fs.rs:63-68` | **YES — destructive replace** | `Err` → `file_error` (`fs.rs:69`) | Lock-serialized via repo-agnostic temp key (`temp_store.rs:206-211`); **destructive reader window + `dest_override` bypass** |

**Highest-severity finding.** The package tier's `move_dir` does `remove_dir_all(dst)` then `rename`. Correctness depends entirely on (a) the repo-agnostic temp flock and (b) the fact that nothing *reads* `packages/{digest}/` under that lock — but package reads (`find_in_store`, live launchers dereferencing into `content/`, `ocx clean` BFS) take **no lock**. The re-pull-over-broken-install path (`pull.rs:303-308`) reaches `move_dir` on an existing live dir → reader-visible `ENOENT`/half-deleted window. The `pull_local` `dest_override` path bypasses the digest singleflight, allowing two concurrent destructive `move_dir`s onto the same target. The layer tier already demonstrates the safe pattern (non-destructive first-writer-wins rename).

### 1.2 Hardlinks / cross-device

`hardlink.rs` errors `CrossesDevices` with **no fallback** (no reflink, no copy); `reflink`/`clone_file` is not a dependency. Docstring bakes in the single-volume assumption (`$OCX_HOME` on one filesystem — required by `temp → packages/` rename and `layers → packages/` hardlink assembly). `same_filesystem.rs` exists as a guard primitive but `move_dir` does not call it.

### 1.3 GC roots and cross-instance behaviour

Roots (`reachability_graph.rs`): **install back-refs** (`refs/symlinks/`, liveness = `path_exists_lossy(forward_target)`) + **project-lock pins** (ledger `$OCX_HOME/projects/`, three-state `Live`/`Dead`/`Unknown`) + **implicit global `$OCX_HOME/ocx.lock`**.

Cross-instance fail-open holes:
- **Unmounted project dir** → `dunce::canonicalize` returns `NotFound` → classified **`Dead`** (`registry.rs:125`), not `Unknown`. The ledger link is pruned and the project dropped as a root → another instance's lock-pinned packages collected. (Transient non-`NotFound` errors *do* fail closed → `Unknown` → retain. The gap is that "not mounted here" == `NotFound` == "deleted".)
- **Install back-ref liveness has no `Unknown` state** — `path_exists_lossy` swallows I/O errors to `false` (`fs.rs:40-47`), so a non-visible/erroring forward symlink is treated as dead → fail-open.
- **No store-wide lock, no mtime grace period.** `GarbageCollector::delete_objects` doc: *"No guard against concurrent installs. Do not run `clean` while other OCX operations are in progress."* Reachability is a point-in-time snapshot; TOCTOU between object write and ref creation deletes freshly-written-but-not-yet-linked objects.
- `--force` ignores the registry entirely (`clean.rs:426-428`).

### 1.4 Env / path surface

Only **`OCX_HOME`** (whole root) and **`OCX_INDEX`** (index/tag path) exist. The single fan-out is `FileStructure::with_root` → `root.join("<name>")` for 7 stores (`file_structure.rs:65-76`); each store already takes a `PathBuf` (clean seam for per-store overrides). A second, divergent seam re-derives the tag root for `OCX_INDEX` in `context.rs:129-138`. Resolution-affecting vars must be mirrored in `env::keys`, `OcxConfigView`, `Env::apply_ocx_config`, and `environment.md`. Path precedence: flag ▸ env ▸ root-default (separate from the `config.toml` merge engine).

### 1.5 Locking inventory

All OS locks use `fs4` flock (advisory on Unix, mandatory on Windows): L1 TempStore download (per-digest, repo-agnostic), L2 TagGuard (per-repo), L3 project `ocx.toml`, L4 `install.json` (per-package), L5 auth `config.json`. Singleflight (`PullCoordinator`, `SetupGroups`) is **in-process only** → N processes = N downloads. **flock on NFS/SMB silently degrades to a no-op** (`research_lockfile_locking_primitives.md:52`; `adr_lock_file_locking_strategy.md:157`); `rename(2)` is not atomic across NFS clients. Same-host containers sharing a local-FS volume share one kernel → flock is enforced cross-container.

## 2. Industry reference patterns

**The universal lesson (Nix, Bazel, containerd, pnpm): separate immutable content-addressed storage — shareable concurrently with no locking, collisions impossible by identity — from mutable per-environment state, and confine concurrency control to the mutable layer.**

- **pnpm** — global CAS (`store-dir`); `package-import-method` fallback order **clone (reflink/CoW) → hardlink → copy**; store on a different drive → copies. `pnpm store prune` GC tracks live projects via symlinks under `{storeDir}/v11/projects/` (the same pattern as OCX's project ledger). CoW clone preferred over hardlink because editing a hardlink corrupts the store. Sources: pnpm.io/package_store, /settings, /cli/store, /global-virtual-store.
- **Nix** — immutable `/nix/store`; multi-user safety via **single-writer `nix-daemon`**; GC roots (`gcroots`, profiles, indirect, temp/runtime); concurrency-safe GC via a **global lock + live socket** so in-flight builds register roots mid-collection. Sources: nix.dev gc, deepwiki NixOS/nix 3.4.
- **Bazel** — split **CAS (content)** vs **Action Cache (results)**; `--disk_cache`; idle GC by size/age (`--experimental_disk_cache_gc_max_size`/`_max_age`/`_idle_delay`). Sources: bazel.build/remote/caching, blog.engflow.com.
- **containerd** — **content store (CAS, immutable)** vs **snapshots (per-container, CoW)** — exact analogue of "shared blobs/layers" vs "per-env install state". Concurrent pulls: `OpenWriter` retries while ref locked (`ErrLocked`); `Commit` verifies digest, returns `ErrAlreadyExists` rather than overwrite; content visible only after commit. BuildKit cache mounts: `sharing=shared|private|locked`. Sources: containerd content-flow.md, pkg.go.dev/containerd/content, docs.docker.com/reference/dockerfile.
- **DevContainers** — named volume mounted at the cache dir is the canonical share-across-rebuilds pattern; **named volumes are created root-owned (UID 0)** → non-root container user hits permission-denied (match UID/GID or chown via entrypoint). Sources: code.visualstudio.com/remote/advancedcontainers, devcontainers/spec #104/#345.

## 3. Filesystem semantics (the substrate matrix)

| Filesystem | Reflink (FICLONE) | rename atomic | Advisory locks | Hardlink |
|---|---|---|---|---|
| btrfs | Safe | Safe | Safe | Safe (same FS) |
| XFS (`reflink=1`) | Safe | Safe | Safe | Safe |
| ext4 | Unsafe (copy fallback) | Safe | Safe | Safe |
| APFS (macOS) | Safe (`clonefile`) | Safe | Safe | Safe |
| ZFS ≥2.2 | Risky (off by default; 2.2.1 corruption bug) | Safe | Safe | Safe |
| overlayfs | Unsafe | Caution (`EXDEV` on lower/merged dir rename) | Caution (old kernels) | Caution (copy-up breaks shared inode) |
| NFS | Unsafe | Caution (retransmit not crash-safe) | **Degrades silently** (flock→lockf, no conflict) | Same-export only |
| Docker local volume | = host FS | Safe | Safe | = host FS |

- `FICLONE`/reflink requires **same mounted filesystem** (cross-mount → `EXDEV`); works across btrfs/XFS subvolumes (one FS), not across a separate device. Rust crate: **`reflink-copy`** — `reflink_or_copy()` mirrors `std::fs::copy` and falls back to byte copy on any failure (unsupported FS, `EXDEV`). Exactly the resilient primitive pnpm models.
- Recommendation: keep store paths on one filesystem; use `reflink-copy::reflink_or_copy` for layer→package assembly with automatic copy fallback; publish via same-FS rename; treat NFS/cross-host as best-effort only.

Sources: man7.org ioctl_ficlone(2), flock(2), rename(2); docs.rs/reflink-copy; kernel.org overlayfs; rpm#2355 (overlay rename EXDEV); openzfs#15050 (block-cloning).

## 4. Adversarial verdicts (all refuted, high confidence)

- **"Concurrent same-digest publish is safe"** → **refuted.** Package `move_dir` destructive window + unlocked reads; `dest_override` bypass; NFS voids the temp lock.
- **"Concurrent `ocx clean` won't delete another instance's objects"** → **refuted.** No store-wide lock/grace period; unmounted project → `NotFound` → `Dead` → pruned; install back-ref fail-open; write-before-ref TOCTOU; `--force` ignores registry.
- **"NFS/named-volume preserves rename+lock guarantees"** → **refuted for NFS/cross-host** (flock degrades, rename non-atomic, GC over-collects); **holds only for same-host local-FS named volume**.
