# Discovery: File-Lock Unification

Consolidated output from Phase 1 explorers (4 workers) and Phase 1 audit. Architect input for the unification design.

## Root cause taxonomy (Windows `LockFileEx`)

Windows `LockFileEx` produces a per-handle, mandatory byte-range lock that covers the entire file range when called with `LOCKFILE_EXCLUSIVE_LOCK`. Two failure modes:

| ID | Failure | Trigger | Os error |
|----|---------|---------|----------|
| F1 | Cross-process raw read of locked range | Another process (e.g. launcher child, kernel exec) opens the data file and `ReadFile`s | 33 `ERROR_LOCK_VIOLATION` |
| F2 | Same-process second handle on locked range | Same process opens a second handle (e.g. `tokio::fs::File`) and reads/writes the locked bytes | 33 `ERROR_LOCK_VIOLATION` |

POSIX `flock(2)` is advisory + per-process — both F1 and F2 silently tolerated on Linux/macOS. Hence Windows-only bug class undetected by Linux+macOS unit tests.

Commits already on `main`:
- `5f869dc2` — F1 fix on `BlobGuard` via sidecar `data.lock`. Data file no longer carries any byte-range lock; launcher children read freely.
- `15226b7d` — F2 fix on `TagGuard` via `FileLock::file_mut()`. All in-place I/O routes through the lock-owning handle.

## Call-site enumeration

10 call sites across 7 subsystems. Each row: location → lock target → mode → post-lock I/O pattern → F1/F2 exposure status.

| # | Site | Lock target | Mode | Post-lock I/O | F1 | F2 | Notes |
|---|------|-------------|------|---------------|----|----|-------|
| 1 | `file_structure/blob_store/blob_guard.rs:99` `acquire_exclusive` | sidecar `data.lock` | EX, 60 s timeout | `write_bytes` opens second fd on `data`, `shutdown()` to flush | safe (sidecar) | safe (different file) | `shutdown()` synchronously closes the write fd before return |
| 2 | `file_structure/blob_store/blob_guard.rs:143` `acquire_shared` | sidecar `data.lock` | SH, 60 s timeout | `read_bytes` opens second fd on `data` | safe | safe | mirror of #1 |
| 3 | `oci/index/local_index/tag_guard.rs:70` `acquire_exclusive` | tag JSON itself | EX, 60 s timeout | `read_disk` / `write_disk` go through `lock.file_mut()` | safe (no external reader) | fixed by 15226b7d | in-place truncate+write; no rename |
| 4 | `oci/index/local_index/tag_guard.rs:96` `acquire_shared` | tag JSON itself | SH, 60 s timeout | `read_disk` through `file_mut()` | safe | safe | — |
| 5 | `project/project_lock.rs:70` `acquire_project_lock_for_file` | `ocx.toml` itself | EX, non-blocking | `mutation::atomic_write_blocking` opens `NamedTempFile` and `persist` `MoveFileEx REPLACE_EXISTING` over locked target | safe (no child reads it) | **UNTESTED Windows hazard** | known weak spot; no Windows acceptance leg |
| 6 | `package_manager/tasks/common.rs:318` `acquire_select_lock` | sidecar `.select.lock` | EX, blocking | `wire_selection` writes `current` symlink via `ReferenceManager` (separate fds) | safe | safe | sentinel-only sidecar |
| 7 | `file_structure/temp_store.rs:129` `try_acquire` | sidecar `{32hex}.lock` | EX, non-blocking | finish_acquire creates temp dir; downloads on separate fds | safe | safe | sentinel-only sidecar |
| 8 | `file_structure/temp_store.rs:141` `acquire_with_timeout` | sidecar `{32hex}.lock` | EX, blocking + timeout | same as #7 | safe | safe | — |
| 9 | `package/install_status.rs:58` `check_install_status` | caller-supplied sidecar path | SH, blocking + timeout | `InstallStatus::read_json` opens status file separately | safe | safe | already sidecar-shaped (separate lock + status files) |
| 10 | `auth/store.rs:419` inline `fs4` spin loop | sidecar `config.json.lock` | EX, 25 ms × 5 s budget | `read_config` via `std::fs::read`, `write_config` via custom temp + `std::fs::rename` | safe | safe | sync `spawn_blocking` context — cannot use async `FileLock` API |

Existing pattern split: **3 sites already use sidecar correctly** (blob_guard, temp_store, install_status, select_lock, auth/store sidecar variant — 5 actually), **2 sites lock the data file directly** with F2-correct handle reuse (tag_guard), **1 site locks the data file directly** with an untested Windows hazard (project_lock).

## Blob-store content-addressed audit

Question: does every `BlobGuard::acquire_exclusive` caller produce byte-identical output for a given SHA-256, with no in-place blob mutation? Required precondition for replacing `BlobGuard` with stateless `tempfile + atomic rename + singleflight`.

Result: **YES**, plan B is safe.

| Caller | Function | Source bytes | Mutation risk | Verdict |
|--------|----------|--------------|---------------|---------|
| `oci/index/local_index.rs:206` | `write_manifest_blob` | `serde_json::to_vec_pretty(manifest)` — deterministic for the same manifest | none | content-addressed safe |
| `oci/index/local_index.rs:309` | `stage_blob_bytes` | caller-provided, verified against digest | none | content-addressed safe |
| `package_manager/tasks/pull_local.rs:449` | `stage_blob_bytes` | OCI registry download bytes | none — fast-path at `pull_local.rs:438-441` skips write when `fs.blobs.data(reg, digest).exists()`; `pull_local.rs:425-426` documents "identity guaranteed — same digest ⟹ same bytes" | content-addressed safe |
| blob_guard tests | test harness | fixtures | n/a | test-only |

Test `concurrent_acquire_write_on_same_digest_serialises` (blob_store.rs:303-338) already proves 8 concurrent writers on the same digest converge to one valid blob. Switching from "exclusive lock serializes writers" to "tempfile-per-attempt + atomic rename" preserves the convergence property because every writer produces the same bytes — the last rename simply replaces with identical content.

## `utility/fs/` module surface (no name collision)

Current contents of `crates/ocx_lib/src/utility/fs/`:

```
assemble.rs   dir_walker.rs   drop_file.rs   empty_or_absent.rs
path.rs       same_dir.rs     same_filesystem.rs   symlink_walk.rs
```

Plus parent `utility/fs.rs` declaring submodules and re-exporting (`pub use assemble::*; pub use dir_walker::*;` etc.).

Grep across all crates for `LockedFile`, `locked_file`, `LockedJsonFile`, `LockedTomlFile`, `SidecarLock` — zero matches. No name collision risk.

`DropFile` (`utility/fs/drop_file.rs`) is **distinct** from any locking concern — it is a synchronous "delete-on-drop unless retained" sentinel for temp paths. Will not be touched by this refactor.

## Singleflight API surface (target for blob dedup)

`crates/ocx_lib/src/utility/singleflight.rs`:

```rust
pub struct Group<K, V>
where K: Clone + Eq + Hash + Send + Sync + 'static,
      V: Clone + Send + Sync + 'static;

impl<K, V> Group<K, V> {
    pub fn new(max_entries: usize, timeout: Duration) -> Self;
    pub async fn try_acquire(&self, key: K) -> Result<Acquisition<V>, Error>;
}

pub enum Acquisition<V> {
    Leader(Handle<V>),    // produce the value
    Resolved(V),          // reuse already-produced value
}

impl<V: Clone> Handle<V> {
    pub fn complete(self, value: V);
    pub fn fail<E: std::error::Error + Send + Sync + 'static>(self, error: E) -> SharedError;
}

pub enum Error {
    Failed(SharedError),  // leader produced an error; broadcast to all waiters
    Abandoned,            // leader dropped Handle without complete/fail
    Timeout,              // waited past Duration
    CapacityExceeded { max: usize },
}
```

For blob dedup: `K = Digest`, `V = ()` (the side effect is the on-disk blob). Leader does tempfile-write + atomic rename + records success; waiters return immediately when leader broadcasts `complete(())`.

## CI surface (current state)

| Workflow | Trigger | Windows native unit tests? | Windows acceptance? | macOS native unit tests? |
|----------|---------|----------------------------|---------------------|---------------------------|
| `verify-basic.yml` | PR + push to main | no (ubuntu only) | no | no |
| `verify-deep.yml` `build` | manual dispatch | **no — `cross-compile` job is `cargo xwin build`, no test execution** | — | yes (`macos-latest`, aarch64) |
| `verify-deep.yml` `acceptance-tests` | manual dispatch | yes (`windows-latest`) BUT docker `registry:2` does not run reliably; user reports broken | — | — |
| `build-windows-shims.yml` `acceptance-windows` | shim-touching PRs | yes (`windows-latest`, native, no docker) | — | — |

Per the user's running cost table (Linux $0.006/min, Windows $0.010/min, macOS $0.062/min):

- Adding `windows-latest` native build+test leg to `verify-deep::build`: +$0.10/run (~10 min).
- Dropping `windows-latest` from `verify-deep::acceptance-tests`: −$0.10/run.
- Dropping x86_64-pc-windows-msvc from `verify-deep::cross-compile` (built natively now): −$0.06/run.
- Temporarily disabling macos-latest in `verify-deep::build` during Windows iteration: −$0.50/run.

Net change after refactor lands: approximately flat. During iteration with macOS disabled: net **savings**.

## Established codebase conventions (relevant)

- Module style: `pub mod foo;` + selective `pub use foo::Foo;` re-export. No `mod.rs`.
- Naming: single-purpose, no domain prefixes (`FileLock`, `BlobGuard`, `DirWalker`, `DropFile`).
- Errors: `crate::error::file_error(path, io::Error) -> crate::Error::InternalFile(PathBuf, io::Error)` is the canonical wrapper. No `Locked`/`TimedOut` variants on the global error — lock contention surfaces as `Ok(None)` on `try_*`, lock timeout surfaces as `io::ErrorKind::TimedOut`.
- Async I/O: `tokio::fs::*` + `tokio::task::spawn_blocking` for sync locking primitives. Synchronous-only call sites (e.g. `auth/store`) live inside an outer `spawn_blocking` and use `fs4::fs_std::FileExt` directly.

## Synthesis (constraints for design)

1. **One mechanism**: `LockedFile` is default for everything where ocx is the only reader/writer. Live in `crates/ocx_lib/src/utility/fs/locked_file.rs`. All I/O routes through `FileLock::file_mut()` — F2-safe by construction.
2. **Eliminate `BlobGuard` entirely**: replace with stateless `BlobStore::write_blob` = `tempfile + sync_data + atomic rename` and `BlobStore::read_blob`. Wrap `write_blob` in `utility::singleflight::Group<Digest, ()>` for same-process coalescing. Content-addressed invariant audit returned safe.
3. **Eliminate sidecar pattern from the codebase**. Existing sidecar users (`temp_store`, `acquire_select_lock`, `install_status`, `auth/store::config.json.lock`) migrate to `LockedFile` on the same sentinel file — the file is opened, locked, and the lock fd is the only fd we touch. Data is read/written via the lock handle when applicable, or via separate uncoordinated I/O when the lock is sentinel-only (e.g. directory existence flag).
4. **`ocx.toml` mutation switches to in-place via `LockedTomlFile<ProjectConfig>::replace_bytes`**. Drops `NamedTempFile::persist` from the commit path. Crash window matches `TagGuard`'s documented kill-9 trade. `ocx.lock` write remains tempfile+rename (its own non-locked file).
5. **`auth/store::acquire_lock` stays inline** (cannot use async `FileLock` API from sync `spawn_blocking` context). But its sidecar is already F1-safe; only the inline retry loop survives. Optionally factor into a sync sibling `FileLock::lock_exclusive_sync_with_timeout` for symmetry.
6. **Visibility tighten**: raw `FileLock::lock_exclusive` / `lock_shared` (no timeout variants) become `pub(crate)` inside the new `utility::fs::file_lock` module. External callers must use `LockedFile` or one of its codec wrappers (`LockedJsonFile<T>`, `LockedTomlFile<T>`).
7. **CI**: add `windows-latest` to `verify-deep::build` matrix (native unit tests); drop x86_64-pc-windows-msvc from `cross-compile`; drop `windows-latest` from `acceptance-tests`; restore macos-latest after Windows iteration green.

## Open hazards to call out in the ADR

- `ocx.toml` in-place rewrite trades atomic-swap for inode-stable in-place truncate+write. A kill-9 mid-write can leave `ocx.toml` truncated or partial; `ocx.lock` is the recovery anchor (written first). Document the contract.
- `auth/store::write_config` currently uses tempfile + rename. Keep that pattern (the file is not held by an open handle during rename — the lock is on `config.json.lock`, not `config.json`). Migration here is just s/inline fs4 spin loop/`LockedFile`-like helper that returns a sync guard/.
- Singleflight `max_entries` for blob writes: in long-lived processes (mirror tool), the in-flight set could grow large. Pick a generous default (e.g. 1024) and document behaviour at capacity (returns `Err::CapacityExceeded`).
- Windows `MoveFileEx REPLACE_EXISTING` against `data` from `data.partial`: only matters if any *reader* holds an open handle on the existing `data`. The launcher pattern is `open + read + close` synchronously; no race observed in practice. Document anyway.
