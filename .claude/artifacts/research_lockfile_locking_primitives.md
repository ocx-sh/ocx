# Research: Lockfile Locking Primitives

**Date:** 2026-04-28
**Author:** worker-researcher
**Context:** OCX Unit 5a architect spike — replace or validate `ocx.lock.lock` sidecar approach.
**Current code:** `crates/ocx_lib/src/file_lock.rs` (fs4 wrapper), `crates/ocx_lib/src/project/lock.rs:199-424`.

---

## TL;DR

OCX's current `ocx.lock.lock` sidecar pattern is **architecturally correct** and matches the approach used by uv (the most relevant modern Rust tool). The primary finding: **no behavior change is required**. The cosmetic awkwardness of `ocx.lock.lock` is the only real argument for change. If we want a cleaner appearance, rename the sidecar to `.ocx.lock` (hidden dotfile) — single-line change, identical semantics.

---

## Current OCX Implementation (read from source)

- `file_lock.rs`: RAII wrapper over `fs4 0.13` sync feature. Async bridge via `spawn_blocking`. `try_exclusive`/`try_shared`/`lock_exclusive`/`lock_shared` — non-blocking variants return `Ok(None)` on contention.
- `lock.rs:load_exclusive` (lines 222–272): opens sidecar `ocx.lock.lock` via `open_sidecar_no_follow` (O_NOFOLLOW on Unix; symlink pre-check on non-Unix), acquires `FileLock::try_exclusive`, reads data file while lock held — all inside `spawn_blocking`.
- `lock.rs:save` (lines 328–424): writes tempfile → `sync_data` → `persist` (atomic rename) → parent `sync_all`. Caller must hold sidecar lock before calling.

---

## 1. Rust Crate Comparison

| Crate | Version | Stars | MSRV | Unix primitive | Windows primitive | Async | Last release |
|-------|---------|-------|------|---------------|------------------|-------|-------------|
| **fs4** | 1.1.0 | 109 | 1.75 (sync) | flock via rustix | LockFile/UnlockFile | Yes (tokio/async-std/smol) | 2026-04-28 (today) |
| **fd-lock** | 3.0.9 | 86 | unstated | flock via rustix | LockFile via windows-sys | No | 2023-01-23 |
| **nix::fcntl** | nix 0.29 | 2.7k (nix) | 1.69 | flock(2) or fcntl(F_SETLK)/OFD | None | No | Active |
| **parking_lot::FairMutex** | 0.12 | 1.6k | 1.49 | None (in-process) | None | N/A | Active |

**OCX-specific notes:**
- fs4 is already in `Cargo.toml` as `fs4 = { version = "0.13", features = ["sync"] }` — no new dep needed.
- **fs4 v1.1.0 is a semver-breaking bump from 0.13** released today. Verify `FileExt::try_lock_exclusive`/`lock_exclusive` API compatibility before upgrading.
- fd-lock is 18+ months stale (Jan 2023). No async support. Maintenance risk.
- nix is Unix-only — incompatible with OCX's Windows target.
- parking_lot has no inter-process capability — irrelevant for cross-invocation protection.

---

## 2. OS Primitive Semantics

**flock(2)** — BSD-style whole-file, used by fs4 and fd-lock on Unix:
- Lock attaches to the open file description (the object behind the fd), not [pid, inode].
- Advisory only — non-cooperating processes can still access the file.
- Survives `rename()` — lock is on the inode; rename doesn't move or invalidate it.
- No deadlock detection.
- Fork: child inherits open file description and shares the lock.
- **NFS (Linux 2.6.12+):** silently emulated as lockf; flock and lockf locks **do not conflict** — critical silent failure mode for cross-machine concurrency.

**fcntl(F_SETLK)** — POSIX record locks:
- Lock attaches to [pid, inode] — closing any fd on that inode from same pid releases all process's locks.
- Not thread-safe (all threads share lock ownership via pid).
- Byte-range locks; deadlock detection (EDEADLK).

**fcntl(F_OFD_SETLK)** — Open File Description locks:
- Like flock: attaches to open file description; thread-safe.
- Linux 3.15+ only; no macOS, no Windows equivalent.

**Recommendation for OCX:** flock (via fs4) is correct for the sidecar use case. OFD would be ideal but is Linux-only. fcntl record locks have the process-level semantics trap.

---

## 3. Atomic Rename + Advisory Lock Interaction

**The inode insight:** flock attaches to the open file description (i.e., the inode at `open()` time). After `rename(tempfile → ocx.lock)`:

- The old inode survives as long as something holds an fd on it.
- The new `ocx.lock` is a new inode with no locks on it.
- New openers of `ocx.lock` see the new inode (new content, no lock interference).
- Existing holders of the old inode's flock have a lock on an orphan inode — harmless.

**Why sidecar is correct for OCX:**
- The sidecar `ocx.lock.lock` is a stable file whose inode never changes.
- Writers hold a flock on the sidecar → serialized; they read + write data file while holding sidecar.
- The data file `ocx.lock` is atomically replaced underneath — concurrent readers always get a coherent snapshot (old or new, never partial).
- Locking the data file directly would create inode confusion: writer A's lock is on the old inode after writer B's rename completes; A's lock no longer protects the current content.

### Pattern Catalog

| Pattern | Use when | Caveats |
|---------|----------|---------|
| **Sidecar advisory lock** (OCX, uv) | Multi-writer + concurrent readers | Sidecar must be durable; O_NOFOLLOW required |
| **Lock data file directly** (cargo cache) | Single-writer, readers tolerate blocking | In-place writes not atomic; rename + lock has inode subtleties |
| **Atomic rename only** (cargo Cargo.lock) | Serial invocation assumption | Last-writer-wins on concurrent access |
| **Lock parent directory** | Protect multiple sibling files atomically | Very coarse; blocks all dir access |
| **Path-digest shared lock dir** | Data on NFS/remote FS | Orphan cleanup; cross-machine unsafe |

---

## 4. Competitive Analysis

| Tool | Lockfile | Lock mechanism | Sidecar? |
|------|----------|---------------|---------|
| **cargo** | `Cargo.lock` | Atomic rename only (no advisory lock on the artifact itself) | No |
| **cargo cache** | cache files | flock (Unix) / LockFileEx (Windows); two separate sentinel files (CACHE_LOCK_NAME, MUTATE_NAME); blocks with user message | Yes |
| **uv** | `uv.lock` | flock via `uv_fs::LockedFile`; acquired for duration of lock/sync/run commands | **Yes** (`.lock` beside the protected dir) |
| **poetry** | `poetry.lock` | No advisory lock on lockfile itself; `filelock` used for HTTP cache only | No |
| **npm/yarn** | `package-lock.json`/`yarn.lock` | None (yarn has `--mutex file` opt-in for CI) | No |
| **pnpm** | `pnpm-lock.yaml` | None; reported race conditions on concurrent install | No |
| **Bundler** | `Gemfile.lock` | None; CI docs recommend serializing `bundle install` | No |

**Pattern signal:** The sidecar advisory lock is used by the two most safety-conscious Rust-ecosystem tools (cargo's cache layer, uv). Others rely on user discipline + atomic rename.

---

## 5. Recommendations (Ranked — Architect Decides in 5a)

### Rank 1 — Keep current sidecar + fs4 (no behavior change)

OCX's design is correct and consistent with uv's approach. O_NOFOLLOW guard, spawn_blocking bridge, RAII release on drop all present and correct. Pros: zero risk, already working, proven pattern. Cons: `ocx.lock.lock` filename surprises users.

### Rank 2 — Keep sidecar, rename to `.ocx.lock` (hidden dotfile)

`path.parent().join(".ocx.lock")` instead of `path.with_added_extension("lock")`. Must add to `.gitignore`. Pros: cleaner git status; semantically identical. Cons: hidden files less discoverable.

### Rank 3 — Lock data file directly (cargo-style)

Acquire flock on `ocx.lock`, read+write while locked, atomic-rename for writes. Writers hold fd to old inode post-rename; harmless. Pros: eliminates sidecar file. Cons: subtle inode semantics; readers attempting to coordinate via the data file lock would observe the new inode immediately after rename with no lock held — unsafe for reader coordination.

### Rank 4 — Atomic rename only (no advisory lock)

Remove `load_exclusive`; rely on atomic rename for consistency. Pros: simplest possible. Cons: concurrent `ocx lock` runs in CI can produce last-writer-wins corruption — **not safe**.

### Not recommended

- nix OFD locks — Linux-only, incompatible with macOS/Windows target.
- parking_lot — in-process only.
- fd-lock — stale, no async.

---

## 6. Tracking

- **std::fs file lock API** is ACP-accepted (libs-team issue #412). Will eventually allow dropping fs4. Track for future migration.
- **fs4 v1.1.0** released today; verify API compatibility before bumping from 0.13.
- **NFS blind spot:** OCX_HOME on NFS has no cross-machine write serialization (flock+lockf don't conflict). Document explicitly if NFS support is in scope.

---

## Sources

- [fs4 GitHub (al8n/fs4-rs)](https://github.com/al8n/fs4-rs) — stars, MSRV, primitives, v1.1.0 release
- [fs4 docs.rs](https://docs.rs/fs4/latest/fs4/) — FileExt trait API
- [fd-lock GitHub (yoshuawuyts/fd-lock)](https://github.com/yoshuawuyts/fd-lock) — flock via rustix, last commit 2023-01-23
- [cargo flock.rs](https://github.com/rust-lang/cargo/blob/master/src/cargo/util/flock.rs) — flock+LockFileEx, FileLock::rename() holding lock across rename
- [cargo cache_lock docs](https://doc.rust-lang.org/nightly/nightly-rustc/cargo/util/cache_lock/index.html) — two-sentinel-file CacheLocker architecture
- [uv issue #18073](https://github.com/astral-sh/uv/issues/18073) — flock NFS silent failure; LockedFile details
- [uv issue #13626](https://github.com/astral-sh/uv/issues/13626) — uv .lock sidecar pattern
- [poetry PR #6471](https://github.com/python-poetry/poetry/pull/6471) — filelock for cache, not poetry.lock
- [File locking in Linux (gavv.net)](https://gavv.net/articles/file-locks/) — flock vs fcntl vs OFD semantics
- [Everything about file locking (apenwarr)](https://apenwarr.ca/log/20101213) — (pid,inode) pair, NFS, fork semantics
- [flock(2) man page](https://man7.org/linux/man-pages/man2/flock.2.html) — NFS emulation, advisory semantics
- [Rust libs-team ACP #412](https://github.com/rust-lang/libs-team/issues/412) — accepted std file lock API proposal
