# ADR: In-Place Exclusive flock on `ocx.toml` as Project Mutex

## Metadata

**Status:** Accepted
**Date:** 2026-05-03
**Deciders:** mherwig
**Beads Issue:** N/A
**Related Plan:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024 + Tokio, no new external dep (reuses `fs4` already pinned in `Cargo.toml`).
**Domain Tags:** architecture | storage | concurrency | dx
**Supersedes:** [`adr_lock_file_locking_strategy.md`](./adr_lock_file_locking_strategy.md)
**Working note:** [`research_lockfile_locking_primitives.md`](./research_lockfile_locking_primitives.md)

## Context

The previous ADR (`adr_lock_file_locking_strategy.md`) chose a per-project sentinel file at `<project>/.ocx-lock` to serialise concurrent writers of project-tier files (`ocx.toml`, `ocx.lock`). That choice was sound for the assumed usage profile — shared and exclusive modes, multiple concurrent readers coordinated via the lock — but a post-implementation audit found the actual usage profile is simpler:

1. **Readers never lock.** `ocx pull`, IDE integrations, and `git` read `ocx.lock` directly via an atomic open+parse. No reader acquires any form of advisory lock.
2. **Only exclusive locks are taken.** No code path calls `lock_shared` on the project data files. The shared/exclusive distinction in the old sidecar design was over-engineering for a purely exclusive mutation pattern.
3. **Writers use tempfile + atomic rename.** The data file (`ocx.toml` or `ocx.lock`) is written to a temporary file and renamed onto the target. The lock is dropped immediately after the rename. Because readers never hold any lock, orphan-inode-after-rename is a non-issue.
4. **`init_project` is not covered by the lock helper.** The file does not yet exist at init time; the implementation relies on tempfile-rename onto a missing target plus a `symlink_metadata` existence check to handle the sole creation race.

Given these facts, the sentinel file adds zero safety beyond what the in-place exclusive flock already provides, while creating a persistent artefact (`<project>/.ocx-lock`) that appears in `git status` after every project mutation.

### Why redesign

The sentinel file was supposed to be transient. In practice it persists until garbage-collected by a future `ocx clean` run. It appears in `git status`, requires a `.gitignore` entry, and has been a recurring source of user confusion. Eliminating it simplifies the operational model: after this change, the only files a user ever sees in the project root are `ocx.toml` and `ocx.lock`.

## Decision Drivers

- **Simplicity.** The actual usage profile (exclusive-only, readers unconditional) does not require a sidecar.
- **DX.** No `.ocx-lock` leftover in `git status` after every mutation.
- **Audit-confirmed correctness.** Readers never lock, so locking the data file in-place does not create any reader-writer contention. Orphan-inode is moot because the lock is dropped before the rename result is visible to other processes.
- **No new dependency.** Reuses `file_lock::FileLock` (`fs4` wrapper) already present in `crates/ocx_lib/src/file_lock.rs`.
- **Breaking-compat authorised.** Project memory (`project_breaking_compat_next_version`) explicitly authorises a clean break: no migration shim, one CHANGELOG entry.

## Decision

**The project mutex anchor is an exclusive advisory `flock(2)` taken in-place on `ocx.toml` itself via `acquire_project_lock(&Path) -> Result<FileLock, Error>`.**

- The `FileLock` RAII guard releases the lock on drop — no manual cleanup.
- No sentinel file is created. `.ocx-lock` and `ocx.lock.lock` are no longer written.
- Writers call `acquire_project_lock`, mutate their in-memory state, write via tempfile + atomic rename, then let the `FileLock` drop (which releases the flock on the original inode).
- `init_project` does **not** call `acquire_project_lock` — `ocx.toml` may not yet exist. It uses tempfile-rename onto a missing target, relying on the rename's atomicity plus a pre-check via `symlink_metadata` for the race.

## Rationale

### Why in-place flock on the data file is safe here

`flock(2)` attaches to the open file description (i.e., the inode at `open()` time). When a writer renames a tempfile onto `ocx.toml`:

- The writer's flock is on the *old* inode of `ocx.toml` (the one that was open when the lock was acquired).
- After the rename, `ocx.toml` in the directory entry points to the new inode (the tempfile's inode).
- Any new opener of `ocx.toml` opens the new inode and sees no lock.

This is safe because:

1. Readers never acquire any lock, so there is no reader waiting on the old inode that might be confused by the new inode.
2. The writer drops its lock immediately after the rename completes. The window during which the old inode carries a now-unreachable flock is sub-millisecond and harmless.
3. A second writer that races to open `ocx.toml` during the rename window opens the new inode (no lock) and proceeds to acquire its own exclusive lock — correctly serialised after the first writer has completed.

The research artifact (§3, "Atomic Rename + Advisory Lock Interaction") confirms this analysis. The concern raised in Option 1 of the superseded ADR ("after rename, a second writer opens the new inode with no lock") is not a problem here because *both* writers complete their mutations atomically before releasing. The inode the lock is held on is the inode being rewritten; once the rename lands, the old inode is orphaned and no future writer sees it.

### Why the sidecar was over-engineered

The sidecar pattern is correct when readers also acquire a shared lock — because then the sidecar provides a *stable* inode for both shared and exclusive locking. OCX readers never lock. The sidecar's only contribution was stable-inode exclusive locking, which in-place flock on the data file also provides (the lock is taken before the rename, not after). Removing the sidecar is safe.

### Why `init_project` is excluded

At init time `ocx.toml` does not exist. Calling `acquire_project_lock` would create the file (to lock it), defeating the init-time existence check. Tempfile-rename atomicity handles the creation race: two concurrent `ocx init` invocations both write to distinct tempfiles and race the rename; the loser's rename will succeed if the winner already created `ocx.toml` in the right place (idempotent). The `symlink_metadata` pre-check surfaces a clear error to the user if the project file already exists. This is the same pattern used by `ocx.lock` writes.

## Consequences

### Positive

- No `.ocx-lock` in `git status` after project mutations.
- No sentinel file to document, gitignore, or clean up.
- Simpler mental model: the lock lives on the file being protected.
- `FileLock` RAII handles release — no explicit cleanup needed.

### Negative

- **Breaking:** `.ocx-lock` and `ocx.lock.lock` are no longer created. Existing `.gitignore` entries for these files become harmless no-ops but should be removed for cleanliness. Any `ocx.lock.lock` committed to VCS must be removed with `git rm`.
- Deleted artifacts: `ProjectLock::load_exclusive`, `sentinel_path_for`, `acquire_config_sentinel`.
- The in-place flock pattern is subtler to reason about than a stable sidecar. Anyone revisiting this code must understand the inode semantics documented in the research artifact.

### Risks

- **NFS / SMB.** Per research §2, `flock` on NFS is silently emulated as `lockf` and the two do not conflict. This risk is unchanged from the previous design — the lock is still on the same volume as the data file. Mitigation: same as today — document `OCX_HOME` and project-on-network-FS as best-effort.
- **Older binary.** A user with an older `ocx` binary that acquires `.ocx-lock` will not conflict with a newer binary that flocks `ocx.toml`. During a brief transition window, concurrent `ocx lock` from different binary versions can race. Mitigation: breaking-compat memo authorises a one-release incompatibility window.

### How to reverse this decision

This ADR is a One-Way Door Medium. To reverse:

1. Reintroduce the sidecar/sentinel pattern from `adr_lock_file_locking_strategy.md` — pick a sentinel name and update `acquire_project_lock` to open it instead of the data file.
2. Add a one-line note to `CHANGELOG.md` and the user guide: "the project mutex moved from an in-place flock on `ocx.toml` back to a sentinel file; add `<sentinel>` to your `.gitignore`."
3. No code-side compatibility shim — single strategy per version, per `project_breaking_compat_next_version`.

## Technical Details

### New API surface

```rust
// crates/ocx_lib/src/project/lock.rs (or project/mod.rs)
pub fn acquire_project_lock(project_toml: &Path) -> Result<FileLock, Error>;
```

`acquire_project_lock` opens `project_toml` (which must already exist) and calls `FileLock::try_exclusive`. On contention it returns `ProjectErrorKind::Locked` (exit code 75, `TempFail`). The caller holds the returned `FileLock` for the duration of the mutation and drops it when done.

### Deleted API surface

- `ProjectLock::load_exclusive` — sidecar-based exclusive load
- `sentinel_path_for(data_path: &Path) -> PathBuf` — computed `.ocx-lock` path
- `acquire_config_sentinel` (if present as standalone function)

### Writer call-site pattern

```rust
let _lock = acquire_project_lock(&project_toml_path)?;
// mutate in-memory state
write_atomically(&project_toml_path, &new_content)?;
// _lock dropped here → flock released
```

## Links

- Superseded ADR: [`adr_lock_file_locking_strategy.md`](./adr_lock_file_locking_strategy.md)
- Research artifact: [`research_lockfile_locking_primitives.md`](./research_lockfile_locking_primitives.md)
- Breaking-compat authorisation: `project_breaking_compat_next_version` (project memory)
- Utility catalog (FileLock row): `.claude/rules/arch-principles.md`
- Storage subsystem rule: `.claude/rules/subsystem-file-structure.md`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-03 | worker-doc-writer (Stream C) | Initial draft — in-place flock on `ocx.toml` chosen, superseding `.ocx-lock` sentinel. |
