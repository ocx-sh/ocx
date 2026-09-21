# ADR: `ocx.toml` Published by Atomic Rename, Mutex Off the Data File

## Metadata

**Status:** Accepted
**Date:** 2026-09-21
**Deciders:** mherwig
**Beads Issue:** N/A
**Related Plan:** `.claude/artifacts/plan_issue_batch_477_494.md` (D-8, WP-6 toml-rename-publish)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024 + Tokio, no new external dep (`tempfile` already pinned).
**Domain Tags:** architecture | storage | concurrency | windows | dx
**Supersedes:** [`adr_project_lock_inplace.md`](./adr_project_lock_inplace.md)
**Amends:** [`adr_file_lock_unification.md`](./adr_file_lock_unification.md) — retracts Decision 3 for `ocx.toml` only; `TagGuard`/`install_status` and the rest of that ADR are unaffected.

## Context

`adr_project_lock_inplace.md` put the project mutation mutex directly on `ocx.toml` — an exclusive `flock` taken in place, with the data file rewritten through the locked handle (`LockedFile::replace_bytes`: `set_len(0)`, `seek(0)`, `write_all`, `sync_data`). That trade accepted a known cost: `flock` is advisory, so an unlocked reader with the file already open — `ocx status`, the per-prompt shell reconciler, a text editor, `git status` — could observe the inode mid-rewrite. [ocx#441](https://github.com/ocx-sh/ocx/issues/441) found exactly this defect class in `~/.docker/config.json` (`auth::store`, the same `LockedFile::replace_bytes` primitive), reproducible 100/100 with a two-binary interleaving, invisible to the acceptance suite's timing window on a fast host.

The first attempted fix ([ocx#492](https://github.com/ocx-sh/ocx/pull/492)) kept the lock on the data file and added a cross-platform acquire-time identity re-verify to `LockedFile` (`lock_matches_path`, comparing `same_file::Handle` — previously `true` unconditionally off Unix). It shipped, then was reverted: on Windows, `MoveFileEx`/`NamedTempFile::persist` cannot replace a file while a `LockFileEx` byte-range lock is held on that same file (`ERROR_ACCESS_DENIED`, ExitCode 5), deterministic on every `put`. `rename(2)` on Unix does not consult `flock`, so the defect was invisible there and only surfaced on a real `windows-msvc` run. The issue's own postmortem named the fix: "the write path and the locking protocol move together… move the lock off the data file onto a sidecar… it also removes the reason `LockedFile`'s acquire-time identity re-verify had to become load-bearing."

[ocx#494](https://github.com/ocx-sh/ocx/issues/494) is that fix, scoped to `ocx.toml`. `crates/ocx_project` is the one surviving `LockedFile`-on-data-file caller after `auth::store` moved (`adr_file_lock_unification.md`'s 2026-09-15 amendment already made the same move for `config.toml`, establishing the `lock_scoped`-under-`$OCX_HOME/locks` shape as precedent rather than a novel mechanism).

## Decision Drivers

- **Cross-platform correctness, not a Unix-only fix.** The chosen shape must publish atomically on Windows too, where a rename-while-locked-on-the-same-file is categorically refused, not merely slow or racy.
- **Kill-9 atomicity.** `ocx.toml` is user-edited and VCS-tracked; a truncated manifest after a crash is a worse failure mode than a rotated inode.
- **No new artefact in the project tree.** A `.ocx-lock` or `ocx.toml.lock` sidecar next to `ocx.toml` was already rejected once (`adr_project_lock_inplace.md`'s own reason for superseding `adr_lock_file_locking_strategy.md`) — `git status` noise and a `.gitignore` entry every contributor has to carry.
- **One mechanism, not a second one invented for this file.** `arch-principles.md`'s Locking Policy table already states the rule this decision needs: atomic-rename-replaced data locks through `lock_scoped` into `$OCX_HOME/locks`, never a sidecar. `ocx_config::edit` is the working instance for `config.toml`.
- **Preserve what operators already rely on.** Unix file mode (`0644` etc.) must survive a mutation; nothing today depends on `ocx.toml`'s inode number.

## Decision

**`ocx.toml` is published by atomic rename, and its mutation mutex moves off the data file entirely.**

1. **Publish by `tempfile` + `rename(2)`/`MoveFileEx`, not in-place rewrite.** `ocx_project::mutate::publish_by_rename(path, bytes)` (and its `publish_by_rename_async` pool wrapper) — grown from the helper `init_project` already used for its own creation case — builds a `NamedTempFile` in the same directory, writes the new content, `fchmod`s it to the *existing* file's Unix mode (`& 0o0777` — permission bits carried uncapped, setuid/setgid/sticky dropped) when one is present (an absent file keeps the `tempfile` default, `0600`), `sync_data`s the temp file, persists it onto `path` through `ocx_util::fs::persist_temp_file` (which retries Windows' transient sharing-violation errors), then fsyncs the parent directory so the rename entry itself is durable. It is the single writer of **both** project files: `ocx.toml` reaches it through `add_binding_in_memory`/`remove_binding_in_memory` under `MutationGuard::commit` and through `set_activate`/`init_project`, and `ocx.lock` through `ProjectLock::save` — so one commit publishes its two files with one crash and Windows contract, and the lock no longer caps its carried mode at `0o644`. `ocx.toml`'s inode therefore rotates on every `ocx add`/`ocx remove`/`ocx lock`/`ocx update` — by design, the same as `config.toml` and every blob this codebase writes.
2. **The mutex is a `lock_scoped` entry under `$OCX_HOME/locks`, scope `project-mutate`, never a lock on `ocx.toml` itself.** `acquire_project_lock{,_for_file}(config_path, locks_root)` keys the lock file's identity off the *config file's parent directory* plus its file name (device+inode of the directory, not of `ocx.toml` — the directory's identity is stable across the manifest's own renames). A contended acquire waits up to a 500 ms budget (sized against this codebase's own measured fork→exec window, well over an order of magnitude of headroom) before reporting `ProjectErrorKind::Locked` → `ExitCode::TempFail` (75, "`ocx.toml` is locked by another process"). No `.lock` file ever appears beside `ocx.toml`.
3. **Readers stay lock-free.** `read_config_via_guard` and the project-context snapshot load become plain bounded reads (`FILE_SIZE_LIMIT_BYTES`), guarded only by a pre-read symlink refusal (`refuse_symlink_at`) — never a lock acquire. Rename-publish is exactly what makes this safe: a reader with an open descriptor keeps reading the inode it opened, in full, regardless of a concurrent writer's rename.
4. **Fold this into the general primitive placement rule, not a one-off.** `LockedFile`/`LockedJsonFile` remain correct for every *stable-inode, edited-in-place* target — `TagGuard`, `install_status`, `temp_store` sentinels, `acquire_select_lock`. `ocx_util::fs::LockedTomlFile` loses its one production caller with this change (`ocx.toml` was it) and is left in place as a follow-up deletion, not removed in this change series — no other caller depends on it today, and deleting dead code is a separate, reviewable diff.

## Rationale

**Why not repeat #492's identity-reverify fix instead of moving the lock?** Because #492's own postmortem measured the wrong operation: a standalone probe renamed over a target that was merely *open*, not one that was *locked* — `NamedTempFile::persist` with the temp handle still open is the real shape, and that one Windows categorically refuses while the destination carries a `LockFileEx` lock. No acquire-time re-verify makes a refused syscall succeed. The only fix that clears the blocker rather than working around it is the one #441's postmortem named directly: stop making the lock target the file being replaced. Moving the lock to `$OCX_HOME/locks` also removes the reason the identity re-verify had to be load-bearing for this file — `lock_scoped`'s entries never rotate out from under a held lock, because nothing ever renames *them*.

**Why `$OCX_HOME/locks` and not a project-local sidecar** (the shape #441's postmortem also floated, and the shape the original `.ocx-lock` sentinel already was): a sidecar next to `ocx.toml` is exactly the artefact `adr_project_lock_inplace.md` deleted to stop `git status` from showing a stray file on every mutation. `$OCX_HOME/locks` already holds every other cross-process mutex this codebase takes (`ocx_config::edit`'s `config.toml` lock, `project-mutate`'s own budget constant precedent), so this decision extends an established convention rather than inventing a second one.

**Why kill-9 atomicity outweighs the inode-stability guarantee the old ADR provided:** nothing in this codebase or in a consumer's tooling keys off `ocx.toml`'s inode number (unlike a lock fd, which genuinely would strand). A rotated inode is invisible to every real reader — an editor and `tail -f` both re-open on the next read, and `git diff` reads the working-tree path, not the inode. A truncated manifest after a `SIGKILL` is not invisible: it is a broken checkout a human has to notice and repair from VCS.

### Consequences

**Positive:**
- Windows and Unix now share one publish path with no OS-conditional branch in the writer.
- A concurrent unlocked reader can never observe a torn or spliced `ocx.toml`, closing the ocx#441 defect class for this file.
- `kill -9` mid-mutation leaves `ocx.toml` exactly as it was before the write started — never a half-written document.
- An editor's or `flock(1)`'s hold on `ocx.toml` no longer blocks `ocx add`/`ocx lock`/`ocx update` — only another `ocx` process holding the `project-mutate` mutex does.
- Unix file mode survives every mutation (`fchmod` on the open temp file, not `tempfile::Builder`'s create-time mode, which `umask` would clip).

**Negative:**
- `ocx.toml`'s inode is no longer stable across a mutation — any future tooling that keyed on inode identity (none does today) would need to switch to content or mtime.
- One more `lock_scoped` entry accumulates under `$OCX_HOME/locks` per project ever mutated on a machine; consistent with every other entry there, cleaned the same way (safe to delete when no `ocx` process is running).

**Risks:**
- A hand-rolled tool outside `ocx` that still locks `ocx.toml` directly (mirroring the pre-#494 contract) gets no serialization against `ocx`'s own writers — accepted, since nothing in this codebase or its documented integrations ever advertised that contract as public API.

## Links

- Issue [ocx#494](https://github.com/ocx-sh/ocx/issues/494) — this decision
- Issue [ocx#441](https://github.com/ocx-sh/ocx/issues/441) — the defect class, the reverted `identity re-verify` fix (PR [#492](https://github.com/ocx-sh/ocx/pull/492)), and the Windows `ERROR_ACCESS_DENIED` postmortem this decision resolves for `ocx.toml`
- [`adr_project_lock_inplace.md`](./adr_project_lock_inplace.md) — superseded by this ADR
- [`adr_file_lock_unification.md`](./adr_file_lock_unification.md) — Decision 3 amendment retracting the in-place rewrite for `ocx.toml`; `config.toml`'s 2026-09-15 amendment is this decision's direct precedent
- `.claude/rules/arch-principles.md` § Locking Policy — the general rule this decision instantiates
- `.claude/artifacts/plan_issue_batch_477_494.md` — D-8, C-008, C-009 (WP-6 toml-rename-publish)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-09-21 | worker-doc-writer | Initial draft — `ocx.toml` rename-publish + `lock_scoped` mutex, documenting WP-6's shipped decision and its #441/#492 precedent. |
