# ADR: File-Lock Unification (`LockedFile` Primitive, `BlobGuard` Removal, In-Place `ocx.toml` Rewrite)

## Metadata

**Status:** Accepted (implemented on `fix/file-lock-unification`, awaiting `/finalize`)
**Date:** 2026-05-28
**Authors:** architect (opus)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024 + Tokio. No new dependency. `fs4 = 0.13` retained.
**Domain Tags:** file-structure | oci | project | concurrency | windows | ci
**Related ADRs:** `adr_three_tier_cas_storage.md`, `adr_project_gc_symlink_ledger.md`, `adr_windows_exe_shim.md`
**Supersedes:** None (consolidation of post-hoc fixes `5f869dc2` + `15226b7d` into a single primitive).

---

## Context

Two Windows-only bugs landed within eleven days, both rooted in `LockFileEx` semantics:

- **F1** (commit `5f869dc2`, `BlobGuard`) — Cross-process raw read of a locked data file: when `BlobGuard::write_bytes` held an exclusive `LockFileEx` lock on the blob `data` file, a launcher child process opening the same `data` for read failed with `ERROR_LOCK_VIOLATION` (os error 33). Fixed by moving the lock onto a sidecar `data.lock` sentinel.
- **F2** (commit `15226b7d`, `TagGuard`) — Same-process second handle on the locked range: `write_disk` opened a second `tokio::fs::File` against the locked tag JSON and `LockFileEx`'s per-handle semantics rejected it with the same os error 33. Fixed by routing every read/write through the lock-owning handle via `FileLock::file_mut()`.

`discovery_file_lock_unification.md` enumerates ten call sites across seven subsystems (rows #1–#10) and concludes that the codebase has accreted two incompatible patterns: a **sidecar sentinel** pattern (`BlobGuard`, `temp_store`, `acquire_select_lock`, `install_status`, `auth/store::config.json.lock`) and a **lock-the-data-file** pattern (`TagGuard`, `project_lock`). Pattern split is a drift hazard: every new call site re-litigates the F1/F2 trade. Three sites (`TagGuard` post-F2, `project_lock`, future writers) need the in-place pattern; the rest are sentinel-shaped. There is no single mechanism a reviewer can defend.

The discovery audit (§"Blob-store content-addressed audit", rows for `write_manifest_blob`, `stage_blob_bytes`, `pull_local::stage_blob_bytes`) returned **verdict: content-addressed safe** for every `BlobGuard::acquire_exclusive` caller. The invariant — given digest = exactly one correct byte sequence, never mutated in place — means the lock on blob `data` exists only to serialise concurrent writers, not to protect a critical section against partial reads. This unlocks a strictly better mechanism for the blob path: stateless `tempfile + sync_data + atomic rename` (which is what cargo, oci-client, and Nix already use), with `singleflight::Group<Digest, ()>` for same-process write coalescing.

`research_file_lock_primitives.md` (§3, §5, §7) confirms the Windows primitives:

- `LockFileEx` byte-range locks are per-handle (F2) and mandatory (F1).
- `LockFileEx` follows the kernel file object, not the directory entry — renaming a locked file does NOT release the lock; the lock travels to the renamed file object. **A rename of the locked file's path replaces the directory entry with a fresh inode; the old (now-unlinked) inode keeps the lock.** A second process opening the path through the new directory entry gets a fresh inode and can acquire its own exclusive lock — breaking mutual exclusion if the original holder is still alive (`research §3 #6`).
- `tempfile::NamedTempFile::persist` calls `MoveFileExW REPLACE_EXISTING` directly, bypassing `std::fs::rename`. Rust 1.85 (PR #131072) fixed `std::fs::rename` to use `SetFileInformationByHandle` with `FILE_RENAME_POSIX_SEMANTICS`, but this fix does NOT apply to `persist`. Against a concurrent non-sharing reader on the CAS destination, `persist` may surface `ERROR_SHARING_VIOLATION` (32). Production data (rattler's `rename_with_retry`, npm/write-file-atomic #227) shows Windows Defender real-time scanning on `windows-latest` GitHub Actions runners amplifies this window to hundreds of milliseconds — a single re-probe is insufficient at scale.

The combination — content-addressed write-once + atomic rename + singleflight — eliminates the lock from the blob data path entirely. F1 cannot recur for blobs because no `LockFileEx` lands on `data`. F2 cannot recur because there is no second handle on a locked file; the writer opens one temp file, writes it, and renames it over the CAS target. On Windows, the persist path retries with exponential backoff on `ERROR_SHARING_VIOLATION` / `ERROR_ACCESS_DENIED` to absorb AV interference (matches rattler's published precedent).

For everything else, one usage rule governs `LockedFile`:

- **Direct in-place lock:** `LockedFile` opens the data file itself; reads and writes route through the lock-owning handle via `replace_bytes` (truncate + write + sync_data on the locked inode). F2-safe by construction. The inode never rotates, so the lock fd never strands. kill-9 can leave the file truncated; recovery is manual (restore from VCS / re-run the mutator). **Applies to:** tag JSON (`TagGuard`), `ocx.toml` mutation, `install.json` install status, `auth/store::config.json`. Sentinel-only locks (where no separate "data" file exists at the lock path) cover `temp_store`'s `<hash>.lock` and `acquire_select_lock`'s `.select.lock` — the lock IS the canonical record and protects a directory or symlink elsewhere.

One primitive (`LockedFile`), one in-place usage rule. No tempfile + rename for files that are also locked — the lock-on-orphan-inode race is eliminated by construction. No sidecar `.lock` files anywhere in the codebase.

CI surface is the second half of the bug class. `verify-deep.yml::build` runs Linux + macOS unit tests only; Windows native unit tests do not exist on any green-required workflow. `verify-deep.yml::cross-compile` cross-builds Windows via `cargo-xwin` but executes nothing. `verify-deep.yml::acceptance-tests` includes `windows-latest` but the embedded `registry:2` Docker container does not run reliably on Windows runners. Two F1/F2 regressions in eleven days is exactly the failure mode this gap produces.

---

## Decision

**Three decisions, executed atomically across eleven commits.**

### 1. `LockedFile` replaces direct `FileLock` use across the codebase

A new primitive `LockedFile` lives at `crates/ocx_lib/src/utility/fs/locked_file.rs`. It wraps an owned `std::fs::File` together with its advisory lock, exposes `open_exclusive`, `open_shared`, `read_bytes`, `replace_bytes`, and `path`, and routes every in-place I/O through the lock-owning handle. The current top-level `file_lock.rs` relocates to `crates/ocx_lib/src/utility/fs/file_lock.rs` as the low-level primitive `LockedFile` is built on; its acquisition methods become `pub(super)` so the only crate-internal route to a raw `LockFileEx` is through `LockedFile` or its codec wrappers `LockedJsonFile<T>` / `LockedTomlFile<T>`. The five existing call sites that own their lock target (`TagGuard`, `project_lock` + `MutationGuard`, `temp_store` sentinels, `acquire_select_lock` sentinel, `install_status` reader) migrate to `LockedFile` or the codec wrappers; F2 is eliminated by construction at every one of them.

### 2. `BlobGuard` is deleted; blob writes become tempfile + atomic rename + singleflight

`crates/ocx_lib/src/file_structure/blob_store/blob_guard.rs` is removed. Two stateless free functions on `BlobStore` replace it: `write_blob(registry, digest, bytes)` and `read_blob(registry, digest)`. The write path uses `tempfile::NamedTempFile::new_in(cas_parent_dir)` + `write_all` + `sync_data` + `persist(cas_target)`, coalesced per-process via `singleflight::Group<Digest, ()>` owned by `BlobStore`. The read path is plain `tokio::fs::read`. The sidecar `data.lock` pattern is eliminated from the codebase; no future blob lands behind a `LockFileEx` lock on a content-addressed data file. F1 cannot recur for any blob path because the lock has been deleted, not relocated.

### 3. `ocx.toml` mutation uses in-place lock + `replace_bytes`

`MutationGuard` locks `ocx.toml` directly (no sidecar). The `commit` body rewrites `ocx.toml` in place through the lock-owning handle via `LockedFile::replace_bytes` (truncate + write + sync_data on the locked inode). No tempfile, no rename. The `commit` flow becomes:

1. Validate `declaration_hash` coherence (unchanged).
2. Write `ocx.lock` via the existing tempfile-and-rename helper in `ProjectLock::save` (unchanged — `ocx.lock` is a separate file, not the lock target).
3. Rewrite `ocx.toml` in place via `self.flock.replace_bytes(serialized.as_bytes())`. The flock is on the same inode the rewrite touches; the inode never rotates, so the lock fd never strands.
4. Register the lock with `ProjectRegistry` (unchanged).

**Why in-place, not sidecar?** Two concurrent writers must serialize. With a sidecar `ocx.toml.lock` the data file `ocx.toml` is replaced via tempfile + rename — atomic on kill-9 but adds a second file (sidecar) and an extra rename surface. With the in-place lock the lock and the data are the same inode: a single primitive, no sidecar files cluttering project directories, and `LockedFile` already routes the rewrite through the lock-owning handle so F2 on Windows is impossible by construction. The mutual-exclusion race that previously motivated the sidecar (rename rotates the inode, second writer locks the orphan) is structurally absent here because `replace_bytes` does NOT rename — it truncates the existing inode and writes new bytes through the locked fd.

**Crash safety trade-off:** kill-9 between `set_len(0)` and `sync_data` leaves `ocx.toml` truncated or partial. Recovery is manual (restore from VCS / re-run the mutator). This is a deliberate trade — eliminating the sidecar file from user-facing project directories outweighs the kill-9 atomicity that tempfile + rename provided. `ocx.lock`'s newer-than-`ocx.toml` mtime is no longer a "corrupt config" signal; it strictly means "lock write completed, manifest write pending or interrupted." The operator must inspect `ocx.toml` and repair if truncated.

The rollback path in `MutationGuard::commit` (Codex Critical-1 contract) is preserved: post-rename failure on the lock side restores the predecessor lock; post-write failure on the manifest side surfaces the original error and leaves the lock advanced (the existing risk, unchanged).

---

## Component Contracts

### `crates/ocx_lib/src/utility/fs/file_lock.rs` (relocated from top-level)

```rust
//! Low-level cross-process advisory file lock.
//!
//! Consumers prefer `LockedFile`, `LockedJsonFile<T>`, or
//! `LockedTomlFile<T>` for the canonical async, F2-safe API.

#[derive(Debug)]
pub struct FileLock {
    file: std::fs::File,
}

impl FileLock {
    /// The file handle that owns the lock.
    pub fn file_mut(&mut self) -> &mut std::fs::File { &mut self.file }

    pub fn try_exclusive(file: std::fs::File) -> std::io::Result<Option<Self>>;
    pub async fn lock_exclusive_with_timeout(file: std::fs::File, duration: std::time::Duration) -> std::io::Result<Self>;
    pub async fn lock_shared_with_timeout(file: std::fs::File, duration: std::time::Duration) -> std::io::Result<Self>;

    /// Synchronous sibling for callers inside `spawn_blocking` that cannot
    /// `.await` the async API (e.g. `auth/store::acquire_lock`).
    pub fn lock_exclusive_blocking_with_timeout(
        file: std::fs::File,
        timeout: std::time::Duration,
    ) -> std::io::Result<Self>;

    // Test-only: lock_exclusive, lock_shared, try_shared — exercised by
    // the inline regression test only, no production callers.
}
```

**Error model:** `Ok(Some(_))` on success, `Ok(None)` on `try_*` contention, `Err(io::Error)` on real I/O failure or `io::ErrorKind::TimedOut` after `*_with_timeout` exhaustion. Unchanged from today.

### `crates/ocx_lib/src/utility/fs/locked_file.rs` (NEW)

```rust
//! Single canonical primitive for in-place locked file I/O.
//!
//! Owns the file handle and the advisory lock together; every in-place
//! read or write routes through the lock-owning handle. F2-safe by
//! construction (cannot accidentally open a second handle on the locked
//! range). F1-safe by use: only used on files that have no external
//! concurrent reader (sentinels, `ocx.toml`, tag JSON, `install_status`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::file_lock::FileLock;

const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub struct LockedFile {
    lock: FileLock,
    path: PathBuf,
}

impl LockedFile {
    /// Acquire an exclusive lock on `path`. Creates the file (and parents)
    /// if absent. Blocks until acquired or `DEFAULT_LOCK_TIMEOUT` elapses.
    pub async fn open_exclusive(path: impl Into<PathBuf>) -> crate::Result<Self>;

    /// Same as `open_exclusive` with caller-supplied timeout.
    pub async fn open_exclusive_with_timeout(path: impl Into<PathBuf>, timeout: Duration) -> crate::Result<Self>;

    /// Acquire a shared lock on `path`. Returns `Ok(None)` if the file
    /// does not exist (reader sees "no content yet" without racing a writer).
    pub async fn open_shared(path: impl Into<PathBuf>) -> crate::Result<Option<Self>>;
    pub async fn open_shared_with_timeout(path: impl Into<PathBuf>, timeout: Duration) -> crate::Result<Option<Self>>;

    /// Try to acquire an exclusive lock without blocking.
    /// Returns `Ok(None)` on contention (another process holds the lock).
    pub async fn try_exclusive(path: impl Into<PathBuf>) -> crate::Result<Option<Self>>;

    /// Read the full file under the lock, through the lock-owning handle.
    /// Seeks to 0 first. Empty file returns an empty `Vec`.
    pub async fn read_bytes(&mut self) -> crate::Result<Vec<u8>>;

    /// Truncate to zero, write `bytes`, `sync_data` for durability — all
    /// through the lock-owning handle. Order: `set_len(0)` → `seek(0)` →
    /// `write_all` → `sync_data`. Caller holds the exclusive lock.
    pub async fn replace_bytes(&mut self, bytes: &[u8]) -> crate::Result<()>;

    pub fn path(&self) -> &Path;
}
```

**Errors:** every fallible method returns `crate::Error::InternalFile(path, io::Error)` via `crate::error::file_error`. Lock timeout surfaces as `io::ErrorKind::TimedOut` wrapped the same way. No new error variants on `crate::Error`.

**Drop semantics:** `Drop` on `LockedFile` releases the OS lock (via `FileLock::Drop`). All `std::fs::File` write operations happen on the blocking pool via `spawn_blocking`; `replace_bytes`'s `sync_data` is the synchronization point — the write completes before the function returns. There is no `tokio::fs::File` involvement on `LockedFile`, so the asynchronous-drop hazard described in `BlobGuard::write_bytes` (`shutdown().await` workaround) cannot recur.

### `crates/ocx_lib/src/utility/fs/locked_file/codec.rs` (NEW — same file)

```rust
/// `serde_json` codec wrapper. Bound: `T: Serialize + DeserializeOwned`.
pub struct LockedJsonFile<T> {
    inner: LockedFile,
    _marker: std::marker::PhantomData<T>,
}

impl<T: serde::Serialize + serde::de::DeserializeOwned> LockedJsonFile<T> {
    pub async fn open_exclusive(path: impl Into<PathBuf>) -> crate::Result<Self>;
    pub async fn open_shared(path: impl Into<PathBuf>) -> crate::Result<Option<Self>>;

    /// Reads bytes via `inner.read_bytes`, parses with `serde_json::from_slice`.
    /// Empty file → `Ok(None)`. Unparseable → `Ok(None)` + WARN log
    /// (kill-9 recovery contract — mirrors `TagGuard::read_disk`).
    pub async fn read(&mut self) -> crate::Result<Option<T>>;

    /// `serde_json::to_vec_pretty(value)` + `inner.replace_bytes`.
    pub async fn write(&mut self, value: &T) -> crate::Result<()>;
}

/// `toml` codec wrapper. Bound: `T: Serialize + DeserializeOwned`.
pub struct LockedTomlFile<T> {
    inner: LockedFile,
    _marker: std::marker::PhantomData<T>,
}

impl<T: serde::Serialize + serde::de::DeserializeOwned> LockedTomlFile<T> {
    pub async fn open_exclusive(path: impl Into<PathBuf>) -> crate::Result<Self>;
    pub async fn open_shared(path: impl Into<PathBuf>) -> crate::Result<Option<Self>>;
    pub async fn read(&mut self) -> crate::Result<Option<T>>;
    pub async fn write(&mut self, value: &T) -> crate::Result<()>;
}
```

**Why both codecs in the same module:** they are pure thin wrappers (≤ 30 lines each), share the empty-OK and unparseable-recovery contracts, and live next to `LockedFile` so the recovery contract is enforced in one place. There is no per-codec extension surface that justifies separate files. `LockedJsonFile` consumes `TagGuard`'s entire surface; `LockedTomlFile` consumes the `MutationGuard::commit` manifest write.

### `BlobStore::write_blob` / `BlobStore::read_blob` (REPLACES `BlobGuard`)

```rust
impl BlobStore {
    /// Stateless. No singleflight here — see `pull_local::stage_blob_bytes`
    /// for the wrapper that coalesces concurrent OCI downloads on the
    /// same Digest.
    ///
    /// Idempotently write `bytes` to the CAS path for (registry, digest).
    /// Caller MUST have verified `digest == sha256(bytes)` upstream — this
    /// function does not re-hash.
    ///
    /// Behavior:
    /// 1. If the CAS `data` path already exists, return `Ok(())` (idempotent).
    /// 2. Otherwise: `tempfile::NamedTempFile::new_in(cas_parent_dir)` →
    ///    `write_all(bytes)` → `sync_data` → `persist(cas_path)`.
    /// 3. On Windows `ERROR_SHARING_VIOLATION` (32) or `ERROR_ACCESS_DENIED`
    ///    (5) — caused by concurrent non-sharing readers or AV scanning —
    ///    retry the persist with exponential backoff (3 retries:
    ///    100ms / 400ms / 800ms with ±25% jitter). After exhausting retries,
    ///    re-check the CAS path; if it now exists, return `Ok(())`
    ///    (the writer that won the race is byte-equivalent by content-addressing).
    ///    Matches rattler's `rename_with_retry` precedent for the same hazard.
    ///
    /// # Errors
    /// - `crate::Error::InternalFile(cas_path, io::Error)` on disk failure
    ///   after retry exhaustion.
    pub async fn write_blob(
        &self,
        registry: &str,
        digest: &oci::Digest,
        bytes: &[u8],
    ) -> crate::Result<()>;

    /// Read the full blob bytes from the CAS `data` path. Returns
    /// `Ok(None)` if the path does not exist. No lock taken — the blob
    /// is immutable by digest, race-free.
    pub async fn read_blob(&self, registry: &str, digest: &oci::Digest) -> crate::Result<Option<Vec<u8>>>;
}
```

**Callers updated in commit 9:**

- `oci/index/local_index.rs::write_manifest_blob` — replaces `BlobGuard::acquire_exclusive(...) + write_bytes(...)` with bare `BlobStore::write_blob`. **No singleflight wrapper** — manifest writes are sequential per index call; content-addressing covers any rare cross-process race.
- `oci/index/local_index.rs::stage_blob_bytes` — same. **No singleflight wrapper** — index-tier blob staging is also sequential.
- `package_manager/tasks/pull_local.rs::stage_blob_bytes` — wrapped in `singleflight::Group<Digest, ()>::try_acquire` (see next section). This is the only caller with concurrent same-digest fan-out (OCI manifest layer downloads parallelized by the resolver).

### `singleflight::Group<Digest, ()>` integration

**Scope: per-`PullLocalContext` (or equivalent stateful struct in `pull_local`).** The architect-review finding identified that `BlobStore::new` is constructed at six call sites per process (`app/context.rs`, `package_manager.rs`×2, `update_check.rs`, `oci/index/local_index.rs`, `oci/index/chained_index.rs`), so per-`BlobStore` scope does NOT coalesce concurrent writers across all instances within one CLI run. Moving the `Group` into `BlobStore` would advertise a dedup guarantee the construction pattern violates.

The actual concurrent-same-digest fan-out exists only in `pull_local::stage_blob_bytes` (OCI manifest layer downloads parallelized via `JoinSet`). Index-layer callers (`write_manifest_blob`, `local_index::stage_blob_bytes`) are sequential within their callers. **Therefore the `Group` lives inside `pull_local`, scoped to one pull operation**, and only that caller benefits from in-process dedup.

```rust
// In pull_local module (NOT BlobStore):
pub(crate) struct PullCoordinator {
    write_group: utility::singleflight::Group<oci::Digest, ()>,
}

impl PullCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            write_group: utility::singleflight::Group::new(
                /* max_entries */ 1024,
                /* timeout */ Duration::from_secs(60),
            ),
        }
    }

    /// Wraps `BlobStore::write_blob` with per-pull-operation dedup.
    pub(crate) async fn stage_blob_bytes(
        &self,
        store: &BlobStore,
        registry: &str,
        digest: &oci::Digest,
        bytes: &[u8],
    ) -> crate::Result<()> {
        // 1. try_acquire(digest)
        // 2. Leader → store.write_blob(...) → complete(())
        //    Resolved → return Ok(())
    }
}
```

**Cross-process / cross-`PullCoordinator` races remain uncoalesced** — two `pull` invocations against the same digest both download. This is acceptable because (a) the content-addressed invariant means both produce byte-equivalent output, (b) `BlobStore::write_blob`'s check-first fast path absorbs the second writer for free, and (c) the rename is idempotent. The cost is one wasted download — bounded and operator-visible.

**`max_entries = 1024` rationale:** one entry per concurrently-downloading blob within one pull. OCI client default fan-out is 16 layers per manifest × 64× headroom for multi-manifest pulls. Per-pull scope means the group is dropped after each pull completes; no accumulation across CLI runs.

**Capacity-exceeded behavior:** `singleflight::Group::try_acquire` returns `Error::CapacityExceeded { max }`. `PullCoordinator::stage_blob_bytes` surfaces it as `crate::Error::Singleflight(_)`. The CLI classification (`classify_error`, `quality-rust-exit_codes.md`) maps it to `ExitCode::TempFail` (75) — retry succeeds once in-flight blobs complete. Backstop, not expected path.

**Resolved-entry retention:** the research artifact (§6) notes `Group` retains resolved entries for its lifetime. Per-pull scope bounds this to one pull operation; mirror flows that pull thousands of packages drop the coordinator between pulls.

### Visibility

`utility/fs.rs` declares `mod file_lock;` (private module) and re-exports `pub use file_lock::FileLock;`. The `FileLock` type and all its production methods are plain `pub` — no `pub(crate)` / `pub(super)` qualifiers (per `quality-rust.md` "`pub(crate)` / `pub(super)` as design smell": control visibility through module nesting, not path qualifiers). The async acquisition primitives `lock_exclusive`, `lock_shared`, and `try_shared` are gated `#[cfg(test)]` — no production callers, used only by the inline regression test in `file_lock.rs::tests`. `LockedFile::from_lock` is `pub` (consumed by `temp_store::try_acquire`'s sync escape hatch).

---

## User Experience Scenarios

| Command | Action | Expected outcome | Error cases |
|---|---|---|---|
| `ocx install pkg` | (a) Manifest blob write via `BlobStore::write_blob`. (b) Status file write via `LockedFile` (replaces `install_status` sidecar). (c) Symlink wiring under `acquire_select_lock` via `LockedFile`. No `ocx.toml` touched. | Blob lands at `blobs/.../data` atomically via rename. Status file holds the lock during write. Symlink sentinel coordinates the candidate-vs-current wiring. | F1: cannot recur on blob (no lock on data). F2: cannot recur on status/select (lock-owning handle). Lock contention on select: `Ok(None)` → caller WARN + abort. Singleflight CapacityExceeded → `TempFail` (75). |
| `ocx pull pkg` | Blob fetch via `BlobStore::write_blob`. No install, no `ocx.toml`. | Identical to install (a). | Same as install (a). |
| `ocx add pkg` | (a) Resolve in-memory. (b) `MutationGuard::commit` → `ocx.lock` via existing tempfile+rename + `ocx.toml` rewritten in place through the lock-owning handle via `LockedFile::replace_bytes`. | `ocx.lock` rewritten atomically. `ocx.toml` rewritten in place under the lock the guard owns on `ocx.toml` itself — no sidecar. Two-file transactional ordering preserved. | Lock-write failure → `ocx.toml` untouched. Manifest-write failure after lock-write → rollback restores predecessor lock (per existing Codex Critical-1 path). kill-9 mid manifest-write → `ocx.toml` is truncated/partial; recovery is manual. |
| `ocx remove pkg` | Same as `ocx add` (mutation transaction). | Same. | Same. |
| `ocx lock` | `MutationGuard::stage` with identity closure → `StagedMutation::lock_only()` → `commit`. `manifest_changed=false`, so the in-place `ocx.toml` rewrite is **skipped entirely** — `ocx.toml` stays byte-identical. Only `ocx.lock` (tempfile+rename) is written. | `ocx.toml` unchanged. `ocx.lock` rewritten atomically. The advisory flock on `ocx.toml` is taken for the duration of the transaction. | Lock-write failure → both files untouched. No partial state possible because the `manifest_changed=false` branch performs zero writes against `ocx.toml`. |
| `ocx login` | `auth/store::ConfigGuard` opens `config.json` directly through `LockedFile::open_exclusive_blocking_with_timeout` (sync API for inside `spawn_blocking`), reads via `read_bytes_blocking`, mutates the in-memory `DockerConfig`, rewrites via `replace_bytes_blocking`. No sidecar. | Config file rewritten in place under the exclusive lock; permissions tightened to `0o600` on Unix. | Lock contention beyond timeout: error 75 (TempFail). Permission denied: error 77 (PermissionDenied). kill-9 mid-write → `config.json` truncated; recovery is manual (`ocx login` again). |
| `ocx exec pkg -- cmd` | Launcher resolves package, may spawn child that reads blob `data`. Parent may be writing **another** digest concurrently. | Child reads `data` freely — no `LockFileEx` lock anywhere on blob data after this refactor. | F1 cannot recur (lock removed entirely from blob data path). |
| `ocx index update` | Tag merge through `TagGuard` → reimplemented on `LockedJsonFile<TagLock>`. | Tag JSON is acquired exclusive, read through the lock-owning handle, merged, written back through the same handle. Identical contract to today's post-F2-fix `TagGuard`. | F2 cannot recur (lock-owning handle). Lock contention: 60 s timeout → `TempFail`. Unparseable file (kill-9 recovery): treated as empty tags + WARN log (unchanged contract). |
| `ocx clean` | Acquires the per-session select-lock sentinel via `LockedFile`, walks the three CAS tiers, removes unreachable entries. Per `subsystem-file-structure.md` the select-lock sentinel is sentinel-only — the data is the directory state. | Reachability walk runs under the sentinel lock. GC contract from `adr_project_gc_symlink_ledger.md` unchanged. | Lock contention beyond timeout: error 75. |

---

## Error Taxonomy

| Failure | Mechanism after this ADR | Surfaced as | Remediation |
|---|---|---|---|
| F1 — cross-process raw read of locked blob data | Eliminated by construction: blob writes use tempfile + rename + singleflight, no `LockFileEx` on blob data file. | Never produced. | n/a (root cause removed). |
| F2 — same-process second handle on locked range | Eliminated by construction: every `LockedFile` consumer routes reads/writes through `lock.file_mut()`. Raw `FileLock` acquisition is `pub(super)` inside `utility::fs`. | Never produced. | n/a (root cause removed). |
| Lock contention (non-blocking) | `LockedFile::try_exclusive` → `Ok(None)` | CLI: `ExitCode::TempFail` (75) when the caller treats it as a hard error. `MutationGuard` already maps it to `ProjectErrorKind::Locked`. | Retry with backoff. |
| Lock timeout (blocking with timeout) | `LockedFile::*_with_timeout` → `io::ErrorKind::TimedOut` → `crate::error::file_error` | `ExitCode::TempFail` (75). | Retry; investigate stuck holder. |
| Singleflight CapacityExceeded | `singleflight::Group::try_acquire` → `Error::CapacityExceeded { max: 1024 }`. Wrapped as `crate::Error::Singleflight`. | `ExitCode::TempFail` (75). | Retry once in-flight blobs settle. Bump `max_entries` if persistent under realistic load. |
| Singleflight Abandoned | Leader dropped without complete/fail (panic in writer task) | `crate::Error::Singleflight` → `ExitCode::Failure` (1). | Bug — leader path must not panic. Re-run. |
| Singleflight Timeout | Waiter exceeded 60 s | `ExitCode::TempFail` (75). | Retry; investigate slow writer. |
| `ERROR_SHARING_VIOLATION` on Windows during `persist` | Caller (a concurrent non-sharing reader on the CAS destination) is racing the rename. Window: between the check-first existence probe and `persist`. | `BlobStore::write_blob` retries the existence check; if the CAS path now exists, returns `Ok(())` (idempotent). If still absent, propagates as `crate::Error::InternalFile`. | Idempotency handled by the function. Operational mitigation: a future cfg-windows reader helper that opens CAS files with `FILE_SHARE_DELETE` (research §5, mitigation 3) — documented as follow-up, not in scope. |
| Crash mid in-place `ocx.toml` rewrite (kill-9) | `set_len(0)` succeeds, `write_all` partial or absent → `ocx.toml` truncated or short. | Next `ProjectConfig::from_path` fails to parse. Recovery anchor: `ocx.lock` was written first; its mtime is newer than `ocx.toml`. Surface error: `ProjectErrorKind::TomlParse` pointing at `ocx.toml` with a hint "ocx.lock is newer; manifest may have been truncated by a crash — restore from VCS or re-run `ocx lock`". | Restore `ocx.toml` from VCS. Project state is not silently corrupt: the parse error fires loudly. |
| Crash mid `TagGuard` write | Identical contract to today (`TagGuard::read_disk` returns empty tags + WARN on unparseable). | `WARN` log; next `ocx index update` rebuilds. | Already documented. Unchanged. |
| Crash between `ocx.lock` rename and `ocx.toml` rewrite | `ocx.lock` advanced, `ocx.toml` is the predecessor. `MutationGuard::commit`'s post-rename rollback path **does not run** because the process was killed (not a returned error). | Next reader sees fresh `ocx.lock` paired with stale `ocx.toml` → `ProjectContextError::StaleLock` (the existing staleness gate, `declaration_hash` mismatch). | User runs `ocx lock` to re-resolve. Existing recovery contract; no change. |

---

## Edge Cases

1. **Same-process concurrent writers to the same digest.** Singleflight coalesces: the first call becomes `Leader`, subsequent calls become `Resolved`. Even if the `Group` is missed (e.g., a writer bypasses `write_blob` — which it must not), both writers produce identical bytes by the content-addressed invariant; the second `rename` is idempotent because `tempfile + persist` uses POSIX-semantics replace.
2. **Cross-process concurrent writers to the same digest.** No singleflight coordination across processes — none possible. Both writers produce identical bytes (content-addressed invariant); the second `rename` replaces with bit-identical content. Final state is correct regardless of which writer wins.
3. **Windows launcher child opening blob `data` mid-rewrite.** Pre-refactor: F1 (`ERROR_LOCK_VIOLATION` from `BlobGuard`'s lock). Post-refactor: `data` carries no lock. `MoveFileEx` with POSIX semantics atomically replaces the file object; the child sees either the old `data` content (its open handle continues pointing at the unlinked inode) or the new `data` content, never partial bytes. Both are byte-identical anyway.
4. **Crash between `ocx.lock` rename and `ocx.toml` in-place rewrite.** `ocx.lock` is the recovery anchor. The existing `MutationGuard::commit` runs `ocx.lock` first deliberately for this reason. On next run, `declaration_hash` mismatch fires `ProjectContextError::StaleLock` — the existing gate. **No auto-repair.** The chosen response is the explicit error; auto-repair (re-resolving lock from manifest) would mask user-edited `ocx.toml` changes. The user runs `ocx lock` to reconcile. This preserves the Codex Critical-1 contract that the wedge is operator-visible.
5. **`LockedFile::replace_bytes` on a file whose size is reduced.** Order is canonical: `set_len(0)` → `seek(0)` → `write_all(bytes)` → `sync_data`. After `set_len(0)` the file is empty; `write_all` extends it to `bytes.len()`. No stale tail bytes possible. `sync_data` ensures data + length metadata are durable; we do not need `sync_all` because file existence/parent-dir durability is not the invariant being asserted (the file pre-existed under the same parent).
6. **Empty `ocx.toml` after kill-9.** `LockedTomlFile::read` reads zero bytes and returns `Ok(None)`. Callers (`ProjectConfig::from_path`) treat empty as a recoverable parse failure → `ProjectErrorKind::TomlParse`. Same surface as today's `TagGuard::read_disk` empty-OK contract, applied to TOML.

---

## Trade-Off Analysis

### Lock primitive

| Option | Description | Pros | Cons | Verdict |
|---|---|---|---|---|
| **A. `LockedFile` (CHOSEN)** | One primitive owning file + lock; reads/writes route through lock-owning handle. | One mechanism. F2-safe by construction. Drops sidecar pattern. Visibility-tightened so the raw `LockFileEx` cannot be reached from outside `utility::fs`. Symmetric with the post-F2-fix `TagGuard`. | Requires migrating five call sites. `pub(super)` on raw `FileLock` violates the `quality-rust.md` warn-tier guideline (negotiated exception, documented inline). | **Chosen for all in-place mutables (`ocx.toml`, tag JSON, install_status, sentinels).** |
| B. `SidecarLock` | Keep the sidecar pattern uniformly; every locked file gets a `.lock` sibling. | Status quo for five sites; minimum migration. Each lock fd never touches data → F1-safe by construction. | Two-file write per lock (sentinel + data) doubles the inode count under `OCX_HOME`. F2 hazard remains for any future caller who locks the data file directly — drift-prone. Requires rebuilding the data-write path's atomicity story per-call-site. | **Rejected.** Drift hazard is the original failure mode. |
| **C. No lock + atomic rename (CHOSEN for blobs only)** | tempfile + sync_data + atomic rename; no `LockFileEx` on the data path. | Eliminates F1/F2 root cause for the content-addressed write path. Matches cargo/oci-client/Nix consensus. No lock to release, no sidecar to clean up. | Requires the content-addressed invariant (write-once, byte-identical-per-digest) — verified by the discovery audit. Adds the `ERROR_SHARING_VIOLATION` Windows narrow window (mitigated by check-first idempotency). | **Chosen for blob `data` only.** Not applicable to `ocx.toml` or tag JSON, which are mutated in place. |

### Blob coordination

| Option | Description | Pros | Cons | Verdict |
|---|---|---|---|---|
| **A. tempfile + rename + singleflight (CHOSEN)** | Stateless write per digest, deduped per-process by `Group<Digest, ()>`. | Eliminates F1 by removing the lock from the data path. Coalesces concurrent same-digest writers (saves a redundant download). Cross-process correctness via content-addressed invariant. | New `crate::Error::Singleflight` variant. Group retention bounded per-`BlobStore` instance (per-session), not Weak-based — needs `max_entries` cap (1024). | **Chosen.** |
| B. Keep `BlobGuard` | Sidecar + lock-owning handle to F2-safe-ify like `TagGuard`. | Smallest diff. Existing sidecar pattern preserved. | Lock cost stays on every write. Concurrent writers serialise instead of coalescing — fan-out perf regression on the mirror path. Sidecar pattern persists, contradicts "one mechanism" goal. | **Rejected.** |
| C. tempfile + rename without singleflight | Stateless, no coordination. | Simplest. F1/F2 gone. | Two same-process writers for the same digest each download N MB independently — gigabytes of redundant transfer in a mirror run. The existing test `concurrent_acquire_write_on_same_digest_serialises` (`blob_store.rs:303-338`) documents the perf-correctness boundary; serialisation is a feature, not an accident. | **Rejected** (cost of duplicate downloads is unacceptable in `ocx_mirror`). |

### `ocx.toml` write

| Option | Description | Pros | Cons | Verdict |
|---|---|---|---|---|
| A. Lock on `ocx.toml` + in-place rewrite via `LockedTomlFile` | `LockedFile` opens `ocx.toml`; `set_len(0)` + `write_all` + `sync_data` through the lock-owning handle. Inode stable. | F2-safe by construction. Symmetric with `TagGuard`. Drops the second filesystem write per commit. Drops the lock-on-orphan-inode race option B suffers. | kill-9 mid-write can truncate `ocx.toml`. **Recovery is not operable for non-VCS users**: `ocx lock` cannot recover (it reads `ocx.toml` first); the project may live in a CI temp dir or fresh scaffold with no VCS to restore from. Cargo PR #12744 chose tempfile-rename for `Cargo.toml` to eliminate exactly this trade. | **Rejected.** kill-9 truncation trades away a foot-gun class for nothing; option C achieves F2-safety AND atomic rename. |
| B. Lock on `ocx.toml` + tempfile-and-rename (status quo) | Current code: `MutationGuard::commit::atomic_write_blocking` uses `NamedTempFile::persist`. Lock held on the original `ocx.toml` inode while the rename swaps the directory entry to a fresh inode. | Atomic-rename crash safety: kill-9 mid-write leaves either the old or new file, never partial. | **Structurally breaks mutual exclusion.** The rename rotates the inode; the lock fd retains its handle on the orphan inode. The next writer that opens `ocx.toml` gets a fresh inode and can acquire `LockFileEx` independently — two processes both think they hold exclusive. Race is conceptual, not "untested" — derivable from `research §3 #6` on Windows. POSIX `flock(2)` is per-fd advisory; the same race exists for tightly racing writers though it has not been observed because OCX project mutations are infrequent. | **Rejected.** Mutual exclusion is a structural invariant; cannot trade it for atomicity. |
| **C. Sidecar lock (`ocx.toml.lock`) + tempfile-and-rename on `ocx.toml` (CHOSEN)** | `LockedFile` opens the sidecar `ocx.toml.lock` sentinel. The existing `atomic_write_blocking` (tempfile + `persist`) writes `ocx.toml` unchanged. Lock and data file are decoupled. | F2-safe (lock is on a separate file from the data write). Atomic-rename crash safety preserved. Mutual exclusion preserved across rename (lock target is stable). Mirrors existing `auth/store::config.json.lock` pattern; uniform across renamed-data sites. **Cargo's `write_atomic` precedent for `Cargo.toml`** validated. | One extra file per project (`ocx.toml.lock`). Two-mechanism story relaxed: `LockedFile` is single primitive but two usage rules (direct vs sidecar) — the rule is mechanical (file renamed on write → sidecar). | **Chosen.** Recovers atomicity, eliminates the mutual-exclusion race, preserves the F2-safety property, aligns with cargo + existing auth/store patterns. Cost: one sentinel file. |

### Visibility

| Option | Description | Pros | Cons | Verdict |
|---|---|---|---|---|
| A. Leave `FileLock::*` `pub` | Status quo. | No visibility wrangling. | Any future caller can re-introduce F2 by opening the raw primitive directly. Drift hazard guaranteed. | **Rejected.** |
| B. `pub(crate)` | Lock visible across `ocx_lib`. | Prevents external-crate misuse. | `ocx_lib` internals can still bypass `LockedFile`. | **Rejected.** Internal drift is exactly the failure mode. |
| **C. `mod file_lock;` + `pub(crate) use file_lock::FileLock;` in `utility/fs.rs`; methods `pub(super)` except sync sibling `pub` (CHOSEN)** | Module stays private to `utility::fs`. The `FileLock` *type* is re-exported `pub(crate)` so `auth::store` can name `crate::utility::fs::FileLock`. Method visibility carries the actual encapsulation: only `lock_exclusive_blocking_with_timeout` is `pub` (callable from anywhere that can name the type); all other methods are `pub(super)` (visible to siblings in `utility::fs` only). | Narrowest scope that still allows the sync escape hatch to compile. Forces every async consumer through `LockedFile` or codec wrappers. Type is nameable crate-wide but the async API is invisible outside `utility::fs`. | Violates `quality-rust.md` "pub(crate)/pub(super) as design smell" warn-tier guideline. Documented inline as a negotiated exception: the encapsulation is the whole point of this ADR. Grep-verifiable: post-commit-7 only two `use` sites reach the type. | **Chosen.** Method-level visibility IS the encapsulation; the type-level re-export is the one-line cost of having the sync escape hatch compile. |

### CI

| Option | Description | Pros | Cons | Verdict |
|---|---|---|---|---|
| **A. Add `windows-latest` to `verify-deep.yml::build` (CHOSEN)** | Native Windows unit tests on `cargo nextest run`. Drop x86_64 from `cross-compile`; drop Windows from `acceptance-tests`. | Closes the F1/F2 blind spot. Net cost approximately flat: +$0.10 (Windows native), −$0.06 (cross-compile shrink), −$0.10 (acceptance shrink). Catches the F1/F2 class structurally — `#[cfg(target_os = "windows")]` tests are the only mechanism that surfaces `ERROR_LOCK_VIOLATION` pre-merge. | One new ~10-min job. | **Chosen.** Two regressions in eleven days is the cost of *not* doing this. |
| B. Skip and rely on acceptance tests | Status quo plus more acceptance tests. | No new build leg. | The `registry:2` Docker setup on Windows is broken per discovery — acceptance tests cannot run reliably there. Doesn't catch primitive-level bugs in any case. | **Rejected.** |

---

## Migration Plan — Eleven Atomic Commits

Each commit independently passes `task verify` (or its `task rust:verify` subset during iteration). Each is a single named transformation (`workflow-refactor.md`, Two Hats Rule). Behavior-changing commits are explicitly flagged.

| # | Commit | Type | Files touched | Why this order |
|---|---|---|---|---|
| 1 | `refactor(utility/fs): relocate file_lock module into utility/fs` | refactor (pure move) | `src/file_lock.rs` → `src/utility/fs/file_lock.rs`; `src/lib.rs` re-export removed; `src/utility/fs.rs` declares `mod file_lock; pub(crate) use file_lock::FileLock;` (type re-exported `pub(crate)` so the eventual sync escape hatch in `auth::store` compiles; method-level visibility carries the encapsulation per §Visibility tightening). Update existing `use crate::file_lock::FileLock` to `use crate::utility::fs::FileLock` at all eight consumer sites. | Module relocation must precede the new code that builds on `file_lock` from inside `utility::fs`. Pure rename — uses LSP `findReferences` (per OCX memory note) — zero behavior change. |
| 2 | `feat(utility/fs): add LockedFile primitive + LockedJsonFile + LockedTomlFile codecs` | feat (additive) | NEW: `src/utility/fs/locked_file.rs`; new entries in `src/utility/fs.rs` re-exports. New unit tests inline. | Adds the primitive without removing anything. Subsequent commits migrate consumers one by one. |
| 3 | `refactor(oci): migrate tag_guard to LockedJsonFile` | refactor (behavior-preserving) | `src/oci/index/local_index/tag_guard.rs` thinned to a typed shim over `LockedJsonFile<TagLock>`. Existing `TagGuard` tests still pass byte-for-byte. | First migration; smallest blast radius. Validates the codec API against the F2-fix call site. |
| 4 | `refactor(file_structure): migrate temp_store sentinel to LockedFile` | refactor | `src/file_structure/temp_store.rs` — replace `FileLock::try_exclusive` direct use with `LockedFile::try_exclusive`. | Sentinel-only path; no I/O routed through the lock handle. Pattern validation. |
| 5 | `refactor(package): migrate install_status to LockedFile` | refactor | `src/package/install_status.rs` — caller-supplied sidecar replaced with `LockedFile::open_shared_with_timeout`. | Sentinel + separate data read. Pattern validation. |
| 6 | `refactor(package_manager): migrate acquire_select_lock to LockedFile` | refactor | `src/package_manager/tasks/common.rs` — sentinel migrated. | Sentinel-only. Same pattern as #4 + #5. |
| 7 | `refactor(auth): migrate write_config sidecar lock through LockedFile sync helper` | refactor | `src/auth/store.rs` — inline `fs4` spin loop replaced by `FileLock::lock_exclusive_blocking_with_timeout`. Sync `spawn_blocking` context preserved. | Last sentinel site. Closes the visibility-tightening list. |
| 8 | `refactor(project): relocate ocx.toml lock target to sidecar; preserve tempfile-rename` | refactor (lock-target relocation) | `src/project/project_lock.rs::acquire_project_lock_for_file` opens a `LockedFile` on `<project>/ocx.toml.lock` (sidecar sentinel) instead of `ocx.toml` itself. `src/project/mutation.rs::MutationGuard.flock` field swaps `FileLock` → `LockedFile`. `MutationGuard::commit::atomic_write_blocking` body preserved unchanged (tempfile + `persist`). Existing rollback path preserved verbatim. Document the new sidecar in `subsystem-package.md` / `subsystem-package-manager.md` if relevant. | Lock-target relocation, not behavior change: kill-9 atomicity unchanged (still tempfile+rename), mutual-exclusion preserved (sidecar inode stable), F2 eliminated (lock and data are different files). Lands after sentinel migrations so `LockedFile` is battle-tested when the highest-stakes consumer adopts it. |
| 9 | `refactor(file_structure): replace BlobGuard with stateless tempfile+rename; introduce PullCoordinator` | **refactor (BEHAVIOR CHANGE — lock removed from blob path)** | DELETE `src/file_structure/blob_store/blob_guard.rs` + the `pub use blob_guard::BlobGuard;` re-export at `src/file_structure.rs:14`. Add stateless `BlobStore::write_blob` / `read_blob`. Add `package_manager/tasks/pull_local::PullCoordinator` owning the per-pull `singleflight::Group<Digest, ()>` (cap 1024, timeout 60 s). Update three callers: `oci/index/local_index::write_manifest_blob` and `oci/index/local_index::stage_blob_bytes` call `BlobStore::write_blob` directly (sequential — no singleflight needed); `package_manager/tasks/pull_local::stage_blob_bytes` calls `PullCoordinator::stage_blob_bytes` (singleflight-wrapped — concurrent fan-out). Update doc comments at `local_index.rs:196`, `:851`, `:884`, `package_manager/tasks/resolve.rs:303` removing `BlobGuard` mentions. Update `test/tests/test_resolution_chain_refs.py::test_no_sidecar_lock_files_in_blobs_dir_after_install` (AC12) — the `data.lock` allowance becomes an assert-absence so any regression that re-introduces a sidecar is caught. Update `.claude/rules/subsystem-file-structure.md:27, :189` (Module Map + canonical-access-pattern entries) and `.claude/rules/arch-principles.md:152` (utility catalog) to reflect the new design. Replace `concurrent_acquire_write_on_same_digest_serialises` with content-addressed equivalent (see Test Strategy). | Behavior change (lock removed from blob path; singleflight scope is per-pull, not per-BlobStore). Lands after #8 because the project-tier consumers are at rest; only OCI-tier callers move. |
| 10 | `ci(verify-deep): add Windows native build+test leg, drop Windows acceptance, drop x86_64 from cross-compile` | ci | `.github/workflows/verify-deep.yml` — `build` matrix gains `windows-latest`; `cross-compile` matrix shrinks to `aarch64-pc-windows-msvc`; `acceptance-tests` matrix drops `windows-latest`. The `macos-latest` entry in `build` is **commented out** with `# RESTORE BEFORE MERGE — see ADR file-lock-unification §CI Integration` so iteration costs land at −$0.50/run while Windows is being debugged. | Lands after commit 2 (so Windows cfg-tests exist) and runs in parallel with commits 3–9. May be split into 10a (matrix change + macOS comment-out) and 10b (macOS re-enabled before merge) at the implementor's discretion. **macOS must be re-enabled before the branch lands on `main`.** |
| 11 | `chore(file_lock): remove dead pub(super) FileLock methods` | chore | Drop any `FileLock` method that no longer has a caller after #3–#9 (likely the `lock_*` non-timeout variants if every consumer migrated to `*_with_timeout`). Skip if every method still has a caller. | Cleanup commit. Safe to omit if nothing is dead. |

**Dependency arrows:** 1 → 2; 2 → {3, 4, 5, 6, 7, 8, 9, 10}; 8 ∥ 9; 11 → {3, 4, 5, 6, 7, 8, 9}.

---

## Test Strategy

| Commit | New / modified tests | Windows-only cfg tests | Notes |
|---|---|---|---|
| 1 | None (pure relocation). Existing `file_lock::tests::test_file_lock` moves with the file. | — | Mechanical rename; LSP refactor. |
| 2 | `locked_file::tests::open_exclusive_acquires_and_replaces_bytes`, `open_shared_returns_none_when_file_absent`, `replace_bytes_truncates_then_writes`, `read_bytes_returns_empty_for_empty_file`, `concurrent_open_exclusive_serialises`, `concurrent_open_shared_coexist`. `LockedJsonFile<T>` round-trip + empty-OK + unparseable-recovery tests. `LockedTomlFile<T>` round-trip + empty-OK + unparseable-recovery tests. | `#[cfg(target_os = "windows")] same_process_second_handle_fails_outside_locked_file` — proves raw `OpenOptions::open` on the locked path hits os error 33, demonstrating the F2 surface `LockedFile` shields against. `#[cfg(target_os = "windows")] replace_bytes_via_locked_handle_succeeds` — the corresponding positive case. | These are the load-bearing tests for the primitive. |
| 3 | All existing `TagGuard` tests pass unmodified. Add `tag_guard_is_locked_json_file_shim` — proves the public surface is identical. | F2 regression test (`merge_under_lock_rewrites_in_place_through_lock_handle`) preserved verbatim and runs under Windows leg. | Behavior preservation. |
| 4–7 | Existing tests pass unchanged. Add per-site `*_uses_locked_file_primitive` smoke tests. | None new. | Behavior preservation. |
| 8 | New: `acquire_project_lock_creates_sidecar` (Unix + Windows): confirms `ocx.toml.lock` appears on disk; `ocx.toml` unchanged. New: `concurrent_mutation_contention_uses_sidecar` (sidecar is the contended file, NOT `ocx.toml`). New: `commit_rotates_ocx_toml_inode_via_rename` (Unix: stat inode before/after — must DIFFER, because atomic rename rotates the inode). New: `kill_9_mid_persist_leaves_ocx_toml_unchanged` (fault-injection probe; tempfile+rename means the data file is never partially overwritten). Existing `MutationGuard` Codex Critical-1 rollback tests preserved verbatim. Existing `add_binding_returns_locked_when_ocx_toml_already_locked` renamed to `..._when_ocx_toml_lock_already_locked` and updated to lock the sidecar. | `#[cfg(target_os = "windows")] commit_sidecar_lock_holds_across_ocx_toml_rename_no_lock_violation` — exercises the sidecar-lock + tempfile-rename path on Windows; asserts no os error 33 anywhere in the chain. | Sidecar relocation is behavior-preserving for kill-9 (atomic rename); the inode-rotates-on-write contract is the visible structural change tests pin. |
| 9 | DELETE `concurrent_acquire_write_on_same_digest_serialises` (`blob_store.rs:303-338`). ADD `blob_store::write_blob_idempotent_when_target_already_exists`. ADD `blob_store::concurrent_write_blob_on_same_digest_atomic_rename_idempotent` (N writers without singleflight, all succeed; final file matches expected digest). ADD `pull_local::pull_coordinator_coalesces_concurrent_same_digest_writers` (N writers via `PullCoordinator`; instrument `BlobStore::write_blob` invocation counter; assert exactly one delegate call). ADD `pull_local::pull_coordinator_returns_singleflight_error_on_leader_failure`. ADD `error::singleflight_error_classifies_to_correct_exit_codes` (Failed/Abandoned → Failure(1); Timeout/CapacityExceeded → TempFail(75)). | `#[cfg(target_os = "windows")] write_blob_retries_on_sharing_violation_then_succeeds` — opens the eventual CAS path with `std::fs::File::open` (no `FILE_SHARE_DELETE`), races a `write_blob` call; asserts eventual `Ok(())` via the retry-with-backoff loop. `#[cfg(target_os = "windows")] write_blob_returns_ok_when_target_exists_after_retry_exhaustion` — simulates retry exhaustion; externally places correct bytes at the CAS path; idempotent re-check returns `Ok(())`. `#[cfg(target_os = "windows")] launcher_child_can_read_blob_data_during_concurrent_other_write` — F1 cannot-recur proof. | The serialisation property test is REPLACED by the rename-idempotency property test + the singleflight coalescing test in `pull_local`. Both prove correctness invariants (one valid blob from N writers; one physical download per digest within one pull). |
| 10 | No code tests; CI matrix change. Verify the new Windows leg runs all of commits 2/3/8/9's `#[cfg(target_os = "windows")]` tests. | — | The whole point of this commit is to actually run the cfg-windows tests added in earlier commits. |

**F1 negative coverage:** commit 9's `launcher_child_can_read_blob_data_during_concurrent_other_write` is the explicit F1 cannot-recur test, replacing `BlobGuard`'s `data_file_openable_while_exclusive_guard_held` test.

**F2 negative coverage:** commit 2's `same_process_second_handle_fails_outside_locked_file` is the explicit F2 surface demonstration. Commit 3's preserved `merge_under_lock_rewrites_in_place_through_lock_handle` is the F2 cannot-recur proof for the migrated `TagGuard`. Commit 8's `commit_sidecar_lock_holds_across_ocx_toml_rename_no_lock_violation` is the F2 cannot-recur proof for `MutationGuard` — exercises the sidecar-lock + tempfile-rename path together on Windows.

---

## CI Integration

### Diff for `.github/workflows/verify-deep.yml` (commit 10)

**`build` job matrix:**

```diff
   build:
     name: Build & Unit Test (${{ matrix.job.name }})
     runs-on: ${{ matrix.job.os }}
     strategy:
       fail-fast: false
       matrix:
         job:
           - { os: ubuntu-latest, name: Linux, target: x86_64-unknown-linux-gnu }
-          - { os: macos-latest, name: macOS, target: aarch64-apple-darwin }
+          # RESTORE BEFORE MERGE — see ADR adr_file_lock_unification.md §CI Integration.
+          # Temporarily disabled during Windows iteration to save ≈ $0.50/run. The
+          # macOS leg MUST be re-enabled before this branch lands on main.
+          # - { os: macos-latest, name: macOS, target: aarch64-apple-darwin }
+          - { os: windows-latest, name: Windows, target: x86_64-pc-windows-msvc }
```

**`cross-compile` job matrix:**

```diff
   cross-compile:
     name: Cross-compile (${{ matrix.job.name }})
     runs-on: ubuntu-latest
     container:
       image: messense/cargo-xwin
     strategy:
       fail-fast: false
       matrix:
         job:
-          - { name: Windows x64, target: x86_64-pc-windows-msvc }
           - { name: Windows ARM64, target: aarch64-pc-windows-msvc }
```

**`acceptance-tests` job matrix:**

```diff
   acceptance-tests:
     name: Acceptance (${{ matrix.job.name }})
     needs: [build, cross-compile]
     runs-on: ${{ matrix.job.os }}
     strategy:
       fail-fast: false
       matrix:
         job:
           # No macOS entry: arm64 runners lack nested virtualization for Docker.
           - { os: ubuntu-latest, name: Linux, target: x86_64-unknown-linux-gnu, ext: "" }
-          - { os: windows-latest, name: Windows, target: x86_64-pc-windows-msvc, ext: ".exe" }
+          # Windows acceptance leg dropped: registry:2 Docker image does not run
+          # reliably on windows-latest runners (discovery briefing §CI surface).
+          # F1/F2 regressions are covered by the windows-latest entry in the
+          # `build` job above (native unit tests with cfg(target_os = "windows")).
```

### Final state diff (must land before `main`)

A commit `10b` (or amended into `10` at finalize time) restores `macos-latest`:

```diff
         job:
           - { os: ubuntu-latest, name: Linux, target: x86_64-unknown-linux-gnu }
-          # RESTORE BEFORE MERGE — see ADR ...
-          # - { os: macos-latest, name: macOS, target: aarch64-apple-darwin }
+          - { os: macos-latest, name: macOS, target: aarch64-apple-darwin }
           - { os: windows-latest, name: Windows, target: x86_64-pc-windows-msvc }
```

The `acceptance-tests` job's `needs: [build, cross-compile]` is unchanged — cross-compile still runs (for `aarch64-pc-windows-msvc`), and build now runs Linux + macOS + Windows.

**Cost estimate (final state):** Linux build ($0.06) + macOS build ($0.62) + Windows build ($0.10) + aarch64 cross-compile ($0.06) + Linux acceptance ($0.06) = ≈ $0.90/run. Compared to today's ≈ $0.96 (Linux + macOS build + 2× cross-compile + Linux + Windows acceptance). **Net flat to mildly cheaper** after the refactor lands.

---

## Open Hazards (Documented but Accepted)

1. **Windows `MoveFileEx` `ERROR_SHARING_VIOLATION` / `ERROR_ACCESS_DENIED` on the CAS persist path.** Production data (rattler's `rename_with_retry`, npm/write-file-atomic #227) shows Windows Defender real-time scanning on `windows-latest` GitHub Actions runners amplifies this window to hundreds of milliseconds, well beyond what a single re-probe absorbs. Bare `std::fs::File::open` from a concurrent reader (no `FILE_SHARE_DELETE`) widens it further. **Mitigation:** `BlobStore::write_blob` on Windows wraps `persist` in an exponential-backoff retry loop (3 retries: 100 ms / 400 ms / 800 ms with ±25 % jitter) on `ERROR_SHARING_VIOLATION` (32) or `ERROR_ACCESS_DENIED` (5). After retry exhaustion, re-check the CAS path; if it now exists, return `Ok(())` (the racing writer was byte-equivalent by content-addressing). Final failure surfaces as `crate::Error::InternalFile`. Cites rattler's published precedent (`crates/rattler_cache/src/package_cache/mod.rs`). **Future structural fix** (deferred): a cfg-windows reader helper that opens CAS files with `FILE_SHARE_DELETE`, eliminating the share-mode mismatch entirely.
2. **`auth/store` and `install_status` migrated to in-place lock on the data file.** `auth/store::config.json` no longer uses a `config.json.lock` sidecar; the data file is the lock target, accessed through `LockedFile::open_exclusive_blocking_with_timeout` + `read_bytes_blocking` / `replace_bytes_blocking` (sync API added to `LockedFile` for callers inside `spawn_blocking` bodies). `install_status` similarly drops the dead `lock_path` parameter — the writer opens an exclusive lock on `install.json` itself via `LockedJsonFile<InstallStatus>`, readers (`check_install_status`) acquire a shared lock and parse, returning `false` for absent / unparseable / not-ok states. Trade-off: kill-9 mid-write can leave `config.json` or `install.json` truncated; recovery is manual (`ocx login` again / re-pull). Cross-process F2 safety is preserved by routing every I/O through the lock-owning handle.
3. **`ocx.toml` kill-9 truncation accepted as the cost of removing the sidecar.** `MutationGuard::commit` rewrites `ocx.toml` in place through the lock-owning handle via `LockedFile::replace_bytes` (truncate + write + sync_data on the locked inode). kill-9 between `set_len(0)` and `sync_data` leaves `ocx.toml` truncated or partial. Recovery is manual (restore from VCS / re-run the mutator). The trade-off was made explicitly to eliminate sidecar `.lock` files from user-facing project directories; the lock-on-orphan mutual-exclusion race that motivated the previous sidecar design is structurally absent because `replace_bytes` does NOT rename. `ocx.lock`'s newer-than-`ocx.toml` mtime is no longer a "corrupt config" signal; it strictly means "lock write completed, manifest write pending or interrupted."
4. **fs4 → `std::fs::File::lock` migration deferred.** Requires MSRV bump to 1.89 (the stabilization release). Mechanical follow-up — one commit, ≤ 50 LOC change inside `utility/fs/file_lock.rs`. Out of scope for this refactor.
5. **WSL2 `\\wsl$` filesystem does not support `LockFileEx`.** Microsoft WSL #5762 and #4689 (open since 2019, unresolved) confirm that `LockFileEx` on `\\wsl$` paths returns "Incorrect function" regardless of flags. If a user sets `OCX_HOME` to a `\\wsl$` path accessed from a native Windows `ocx` process, `LockedFile` will error on every acquisition. **Workaround:** `OCX_HOME` must be on a native NTFS volume. Low priority for OCX's target audience (CI runners use native filesystems); documented for completeness.
6. **F1 is closed by convention, not by structural invariant.** This ADR eliminates F1 for the current blob path by removing the lock from `data` entirely. A future caller who needs cross-process blob-write coordination via locking would have to re-introduce a lock and risk re-opening F1. The structural fix — moving installed binaries out of `blob_store/` so no executable lives in a CAS path — is the deferred ADR called out in §Out of Scope. Until then, the F1 closure is a code-review responsibility: any new `LockFileEx` on a path readable by launcher children is a regression.

7. **No-lock sites under multi-process concurrent CI on a shared `$OCX_HOME`.** The branch leaves three sites without cross-process coordination locks. Each is safe today; each has a documented future-direction path if concurrent-CI dedup becomes a requirement.

   **`BlobStore::write_blob`** (raw OCI blob writes). Two CI processes pulling the same package both run the full download → tempfile + persist pipeline. Content-addressing makes both outcomes byte-identical and the rename is idempotent (target either does not exist yet or already matches). On Windows the persist retry-with-backoff absorbs `ERROR_SHARING_VIOLATION` / `ERROR_ACCESS_DENIED` from short-lived concurrent reader handles or AV scans. Correctness: safe. Cost: duplicate downloads (bandwidth waste only). Risk: a long-lived reader handle without `FILE_SHARE_DELETE` exceeding the retry budget — not observed in production. **Future direction:** sentinel per-digest lock at `$OCX_HOME/state/blob-locks/<algo>/<2hex>/<30hex>` (separate dir tree, never inside `blobs/`) would deliver cross-process dedup without re-introducing F1. Sentinel lock pattern keeps the data file itself unlocked so launcher children retain unrestricted read access. Not implemented now.

   **`PullCoordinator` singleflight** (in-process same-digest dedup). The `singleflight::Group<Digest, ()>` is scoped per-pull operation — same-process fan-out is coalesced (one downloader, others await), but cross-process fan-out is not. Multi-process CI with N parallel runners on the same `$OCX_HOME` produces N downloads of any shared blob. The sentinel lock above would also close this gap. Until then, content-addressing guarantees correctness; only bandwidth is wasted.

   **`ocx.lock` writes** (project lock file, distinct from any flock). Coordinated via the `ocx.toml` flock held by `MutationGuard` for the entire commit transaction. Cross-project (multi-CI on shared `$OCX_HOME` but different projects) trivially safe — each project has its own `ocx.toml` flock. Same-project across processes correctly serialised by the in-place lock on `ocx.toml`. No future-direction work required.

---

## Out of Scope

- **Moving installed binaries out of `blob_store/`.** This would eliminate the F1 class entirely by separating "data we lock-coordinate" from "data anyone may exec". Bigger ADR; the launcher child reading blob `data` is the symptom, not the root.
- **Bumping fs4 from 0.13 to 1.1.** Orthogonal cleanup; landed if and when desired, not coupled to this refactor.
- **Dropping the `tempfile` dependency.** `tempfile` is still used by `ocx.lock`'s rename-based atomic write (which this ADR does not change). Dropping it would require migrating `ocx.lock` writes too — not justified by this refactor.
- **Changing `ocx.lock` atomicity.** `ocx.lock` continues to use tempfile + rename. The flock target is `ocx.toml` (not `ocx.lock`), so no lock fd ever holds `ocx.lock`'s inode and the rename is safe. The atomic-rename crash safety for `ocx.lock` is unchanged from today.
- **Windows acceptance test suite.** The `registry:2` Docker-on-Windows runtime issue is a separate effort. Native Windows unit tests in the `build` job are the gate this refactor closes; acceptance coverage on Windows is a follow-up.
- **`PathBuf` parameter narrowing in `LockedFile::open_*`.** The signatures use `impl Into<PathBuf>` to avoid double-allocations at common call sites (`PathBuf` and `&Path` both work). `&Path` was considered but loses ergonomics with owned paths the guard needs to retain. Final shape is the trade-off recorded in §Component Contracts.

---

## Validation

- [ ] `cargo deny check` passes — no new third-party deps introduced.
- [ ] Every `#[cfg(target_os = "windows")]` test added in commits 2/3/8/9 fails on Linux (skipped) and passes on `windows-latest`.
- [ ] F1 cannot-recur test (commit 9 `launcher_child_can_read_blob_data_during_concurrent_other_write`) passes on Windows.
- [ ] F2 cannot-recur tests (tag_guard `merge_under_lock_rewrites_in_place_through_lock_handle`, project_lock `replace_bytes_via_locked_handle_no_lock_violation`) pass on Windows.
- [ ] `task verify` passes on every commit individually (bisectability).
- [ ] **Structural test for macOS leg restoration** — `.claude/tests/test_ai_config.py` gains a test that parses `.github/workflows/verify-deep.yml` and asserts the `build` job matrix contains an enabled (non-commented-out) `macos-latest` entry. Test added in commit 10b (or as part of commit 10's amend at finalize time). Without this, the branch cannot land on `main` even if a human reviewer forgets the checklist — converts the "RESTORE BEFORE MERGE" human-only safeguard into a CI-blocked invariant.
- [ ] `macos-latest` entry restored in `build` matrix before the branch lands on `main` (enforced by structural test above).
- [ ] `test/tests/test_resolution_chain_refs.py::test_no_sidecar_lock_files_in_blobs_dir_after_install` (AC12) updated to forbid `.lock` sidecars under `blobs/` (was: allowlisted `data.lock`).
- [ ] `.claude/rules/subsystem-file-structure.md` `BlobGuard` references removed; `BlobStore::write_blob` / `read_blob` documented as the canonical concurrent-access path.
- [ ] `.claude/rules/arch-principles.md` utility catalog entry for `BlobGuard` removed and replaced with `BlobStore::{write,read}_blob`.

---

## Links

- Discovery: [`./discovery_file_lock_unification.md`](./discovery_file_lock_unification.md) — call-site enumeration (10 sites), content-addressed audit, CI surface, conventions, open hazards.
- Research: [`./research_file_lock_primitives.md`](./research_file_lock_primitives.md) — `LockFileEx` semantics, `MoveFileEx` + `FILE_SHARE_DELETE`, `tempfile` Windows behavior, `std::fs::File::lock` (Rust 1.89), `singleflight` API surface, decision matrix.
- Sibling commits already on `main`: `5f869dc2` (F1 fix), `15226b7d` (F2 fix).
- Affected source: `crates/ocx_lib/src/file_lock.rs`, `crates/ocx_lib/src/file_structure/blob_store/blob_guard.rs`, `crates/ocx_lib/src/oci/index/local_index/tag_guard.rs`, `crates/ocx_lib/src/project/mutation.rs`, `crates/ocx_lib/src/project/project_lock.rs`, `crates/ocx_lib/src/file_structure/temp_store.rs`, `crates/ocx_lib/src/package_manager/tasks/common.rs`, `crates/ocx_lib/src/package/install_status.rs`, `crates/ocx_lib/src/auth/store.rs`, `crates/ocx_lib/src/utility/fs.rs`, `crates/ocx_lib/src/utility/singleflight.rs`.
- Subsystem rules consulted: `subsystem-file-structure.md` (BlobStore + ReferenceManager + GC), `subsystem-oci.md` (LocalIndex write paths), `subsystem-package.md` (install_status), `subsystem-package-manager.md` (acquire_select_lock + pull_local), `subsystem-cli-api.md` (exit code mapping). Quality rules: `quality-rust.md` (visibility, async patterns, RAII), `quality-rust-errors.md` (new `Error::Singleflight` variant follows `#[non_exhaustive]` + lowercase rule), `quality-rust-exit_codes.md` (TempFail = 75 for transient lock contention).
- CI workflow: `.github/workflows/verify-deep.yml`.

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-05-28 | architect (opus) | Initial draft — `LockedFile` chosen for in-place mutables; `BlobGuard` deleted; `ocx.toml` in-place rewrite; Windows CI leg added; 11-commit migration plan. |
