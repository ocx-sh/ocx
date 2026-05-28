# Research: File-Lock Primitives + Windows Semantics

Persisted output from Phase 2 worker-researcher (sonnet) on 2026-05-28. Architect input for `adr_file_lock_unification.md`.

## Context

Two Windows-only bugs in 11 days from `LockFileEx` semantics:
- F1 (commit `5f869dc2`): cross-process reader of locked data file → `ERROR_LOCK_VIOLATION` (33). Fixed by sidecar `data.lock`.
- F2 (commit `15226b7d`): same-process second handle on locked range → same error. Fixed by `FileLock::file_mut()`.

Upcoming refactor: collapse all lock call sites onto a single `LockedFile` primitive in `crates/ocx_lib/src/utility/fs/`, delete `BlobGuard`, replace with stateless tempfile + atomic rename + `singleflight::Group<Digest, ()>`. Drop sidecar pattern.

## 1. fs4 (current OCX dependency)

- **OCX pin:** `fs4 = { version = "0.13", features = ["sync"] }` (root `Cargo.toml`).
- **Latest crate:** 1.1.0 (2026-04-28). ~4.2M downloads/month. Fork of `fs2-rs` by Al Liu (al8n/fs4-rs).
- **MSRV:** 1.75.0 for `sync` feature; 1.85 for async variants.
- **Status:** Active, maintained, primary choice today.
- **Windows impl confirmed from source:** `lock_impl!` macro calls `LockFileEx` for both lock variants and `UnlockFile` (not `UnlockFileEx`) for unlock. `try_lock_*` passes `LOCKFILE_FAIL_IMMEDIATELY`. `lock_exclusive` passes `LOCKFILE_EXCLUSIVE_LOCK`; `lock_shared` passes 0. Byte range = full file (`!0, !0`). `ERROR_LOCK_VIOLATION` mapped to "would block".
- **`fs2` is unmaintained** — last release 0.4.3 (2018-01-06, Rust 2015 edition). OCX migration was correct.

## 2. Alternative crates

| Crate | Windows primitive | Shared | Try | Per-handle | Status |
|-------|-------------------|--------|-----|------------|--------|
| `fs4` 1.1.0 | `LockFileEx` | Yes | Yes | Yes | Active (2026) |
| `fd-lock` 4.0.4 | `LockFile` (legacy, exclusive-only) | No | No | Yes | Active (2025) |
| `named-lock` 0.4.1 | `CreateMutexW` (per-path mutex) | No | No | No | Marginal (2024) |
| `advisory-lock` 0.3.0 | `winapi` | Yes | Yes | Yes | Abandoned (2020) |
| `std::fs::File::lock` (stable 1.89+) | `LockFileEx` | Yes | Yes | Yes | stdlib — permanent |

**Verdict:** `fs4` is correct today. Replacement once MSRV ≥ 1.89 = `std::fs::File::lock` directly. Mechanical one-commit change.

## 3. Windows `LockFileEx` authoritative semantics

Source: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex

1. **Per-handle, not per-path.** MSDN: *"If the locking process opens the file a second time, it cannot access the specified region through this second handle until it unlocks the region."* This is F2.
2. **Mandatory byte-range lock.** Exclusive: denies all other processes read+write to the byte range. Shared: denies all processes write access (including the locking process); all processes can read. Stronger than POSIX `flock` (advisory).
3. **Non-locking reads blocked under exclusive lock.** `ERROR_LOCK_VIOLATION` (os error 33) for any conflicting access on the locked byte range.
4. **`LOCKFILE_FAIL_IMMEDIATELY` (0x1)** = non-blocking try variant.
5. **`LOCKFILE_EXCLUSIVE_LOCK` (0x2)** = exclusive; without it = shared.
6. **Lock follows the file object, not the filename.** Rename does NOT release the lock — kernel file object carries it. Critical for atomic-rename CAS: a lock on `data.partial` survives rename to `data`. A lock on the destination CAS path is unaffected by renaming a different temp file over it (different file object).
7. **Memory-mapped files: locks ignored.** Bypass risk if OCX ever uses `mmap`. Not currently a concern.

Locking guide: https://learn.microsoft.com/en-us/windows/win32/fileio/locking-and-unlocking-byte-ranges-in-files — explicitly confirms F2: *"If the locking process attempts to access a locked byte range through a second file handle, the attempt fails."*

## 4. `MoveFileEx` over an open/locked target

Source: https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexa

- `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`:
  - Destination open without `FILE_SHARE_DELETE` → fails with `ERROR_SHARING_VIOLATION` (32).
  - Destination open with `FILE_SHARE_DELETE` → rename succeeds; open handle continues pointing at unlinked file content (Unix-like).
- **`std::fs::rename` on Windows 10 1607+** uses `SetFileInformationByHandle` with `FileRenameInfoEx` + `FILE_RENAME_POSIX_SEMANTICS` → POSIX-style atomic replace regardless of destination share mode. Source: https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info
- **`tempfile::NamedTempFile::persist`** uses `MoveFileEx`. May or may not engage POSIX semantics — verify against `std::fs::rename` behavior in the implementation choice.

## 5. `tempfile` crate on Windows

Source: https://github.com/Stebalien/tempfile/blob/master/src/file/imp/windows.rs

- Opens with `FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE`. Creates with `FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE`.
- `NamedTempFile::persist(dest)` calls `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING`. Internal write handle opened with `FILE_SHARE_DELETE` does not block the rename.

**Risk for the refactor:** if a concurrent reader opened the CAS destination via `std::fs::File::open` (which does NOT pass `FILE_SHARE_DELETE`), `persist` fails with `ERROR_SHARING_VIOLATION`. Window is narrow (between check-first and persist) but non-zero.

**Mitigations (additive):**
1. Singleflight: only one writer per digest in-process.
2. Check-first: skip write if CAS path exists.
3. Cfg-windows reader helper: open CAS data files with `OpenOptionsExt::share_mode(FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE)`.
4. Idempotency: on `ERROR_SHARING_VIOLATION`, retry check-first; if `data` now exists with the right digest, succeed.

No open GitHub issues on `Stebalien/tempfile` about `FILE_SHARE_DELETE` + rename collisions as of research date.

## 6. OCX `singleflight` audit

File: `crates/ocx_lib/src/utility/singleflight.rs`. Watch-channel-based. Caller pattern:

```rust
pub struct Group<K, V>
where K: Clone + Eq + Hash + Send + Sync + 'static,
      V: Clone + Send + Sync + 'static;

pub fn new(max_entries: usize, timeout: Duration) -> Self;
pub async fn try_acquire(&self, key: K) -> Result<Acquisition<V>, Error>;

pub enum Acquisition<V> { Leader(Handle<V>), Resolved(V) }

impl<V: Clone> Handle<V> {
    pub fn complete(self, value: V);
    pub fn fail<E>(self, error: E) -> SharedError;
}

pub enum Error { Failed(SharedError), Abandoned, Timeout, CapacityExceeded { max: usize } }
```

`SharedError` wraps `Arc<dyn Error + Send + Sync>`; implements `Error::source()` to walk to the inner typed error — critical for OCX's `classify_error`. External crates lack this.

**Resolved entries retained for `Group` lifetime** (no Weak-based expiry). Scope the group to a single install batch.

**Verdict: keep OCX's implementation.** Superior to external alternatives for OCX's constraints.

## 7. CAS atomic write pattern (ecosystem consensus)

Universal: write to temp → fsync → atomic rename to CAS path. Cargo, oci-client, Nix follow it.

`oci-client` (OCX's `external/rust-oci-client`): streams via `VerifyingStream` for on-the-fly digest check. Local disk atomicity = OCX's responsibility.

**Write-once CAS properties OCX can exploit:**
1. Given digest = exactly one correct byte sequence.
2. Concurrent racing writers produce identical bytes — either winning the rename is correct.
3. Once written, CAS path never modified.

→ Singleflight = perf opt (not correctness). Check-first = idempotent. `LockFileEx` on the CAS data file is not required.

**Recommended Windows-safe CAS write sequence:**

```
1. Check: cas_path.exists() && is_valid() → fast path return.
2. acquire = group.try_acquire(digest) → if Resolved: return.
3. tmp = NamedTempFile::new_in(cas_dir)  // same FS as CAS
4. stream digest → write to tmp, accumulate hash
5. Verify accumulated hash == expected; on mismatch: error
6. tmp.persist(cas_path)   // MoveFileEx REPLACE_EXISTING
   // On ERROR_SHARING_VIOLATION: re-check step 1; succeed if now exists
7. handle.complete(())
```

## 8. Recent developments (2025-2026)

### `std::fs::File::lock` stabilized in Rust 1.89 (2025-09)

PR: https://github.com/rust-lang/rust/pull/142125 (merged 2025-06-16, milestone 1.89). Tracking issue 130994 closed.

Stabilized surface:
```rust
fn lock(&self) -> io::Result<()>             // blocking exclusive
fn lock_shared(&self) -> io::Result<()>      // blocking shared
fn try_lock(&self) -> io::Result<bool>       // non-blocking exclusive
fn try_lock_shared(&self) -> io::Result<bool>// non-blocking shared
fn unlock(&self) -> io::Result<()>
```

Semantics: advisory on POSIX, mandatory byte-range on Windows (`LockFileEx`). Same per-handle semantics. Drop `fs4` once OCX MSRV ≥ 1.89.

### Tokio + `spawn_blocking` for file locks

Recommendation unchanged. `tokio::fs::File` does not expose locking. Blocking locks must use `spawn_blocking`. OCX's `FileLock::lock_exclusive` pattern is correct.

### Tokio async file drop (root cause of F2-class drop races)

`tokio::fs::File` closes the OS handle **asynchronously on a background thread pool** — NOT synchronously during `drop()`. Subsequent open of the same path immediately after `drop(file)` may race the background close on Windows → `ERROR_LOCK_VIOLATION` / `ERROR_SHARING_VIOLATION`.

Fix in OCX `BlobGuard::write_bytes`: `file.shutdown().await` before return. Drives the close to completion synchronously. Pattern must be preserved (or eliminated by switching to sync `std::fs::File` for short writes).

### Windows package manager landscape

No public reproductions of the F1 / F2 class found in other Rust Windows package managers as of May 2026. Pattern is OCX-specific, arising from Tokio async file drop + Windows mandatory byte-range locking.

## Decision matrix

| Call-site class | Recommended primitive | Rationale |
|-----------------|-----------------------|-----------|
| **In-place mutable rewrite** (`ocx.toml`, tag JSON, install_status, auth config, select sentinel, temp_store sentinel) | `LockedFile` on the data file itself; all I/O via `FileLock::file_mut()` (F2-safe by construction); reads and writes through the lock-owning handle | No external readers → no F1. Routing I/O through the lock handle eliminates F2. Drops the sidecar layer for these. |
| **Write-once content-addressed** (blob `data`) | Eliminate `BlobGuard`. Stateless `NamedTempFile::new_in(cas_dir)` + write + `sync_data` + `persist` over CAS path + `singleflight::Group<Digest, ()>` + check-first fast path | F1 root cause = lock on data. Atomic rename pattern eliminates the lock entirely. Content-addressed invariant audit returned safe. Singleflight handles same-process dedup. |
| **Future post-MSRV-1.89** | Replace `fs4::fs_std::FileExt` with `std::fs::File::lock` | Mechanical. Drops one dependency. |

## Sources

Authoritative URLs:
- https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex
- https://learn.microsoft.com/en-us/windows/win32/fileio/locking-and-unlocking-byte-ranges-in-files
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexa
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info
- https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-setfileinformationbyhandle
- https://github.com/rust-lang/rust/pull/142125
- https://github.com/rust-lang/rust/issues/130994
- https://github.com/al8n/fs4-rs
- https://github.com/Stebalien/tempfile/blob/master/src/file/imp/windows.rs
- https://docs.rs/tempfile/latest/tempfile/struct.NamedTempFile.html

## Recommendations

1. **Keep `fs4` 0.13.x for now.** Bump to 1.1 in a separate commit if desired (out of scope). Plan `std::fs::File::lock` migration when MSRV reaches 1.89.
2. **For `LockedFile`:** wrap `std::fs::File` directly. Take ownership (RAII). Expose `file_mut()` (or internalize via methods like `read_bytes` / `replace_bytes` that route through the lock handle).
3. **For `BlobGuard` replacement:** tempfile + `persist` + `singleflight` + check-first. Three mitigations stack against `ERROR_SHARING_VIOLATION`. Treat that error as retryable via check-first idempotency.
4. **Windows reader helper for CAS files:** `#[cfg(windows)]` helper that opens with `FILE_SHARE_DELETE`. Apply to launcher child blob reads if accessible from `ocx_lib`.
