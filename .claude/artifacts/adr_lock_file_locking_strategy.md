# ADR: `ocx.lock` Writer Serialization Strategy

## Metadata

**Status:** Accepted
**Date:** 2026-04-29
**Deciders:** mherwig
**Beads Issue:** N/A
**Related Plan:** [`/home/mherwig/.claude/plans/auto-findings-md-eventual-fox.md`](file:///home/mherwig/.claude/plans/auto-findings-md-eventual-fox.md) Unit 5
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` — Rust 2024 + Tokio, no new external dep (reuses `fs4` already pinned in `Cargo.toml`).
**Domain Tags:** architecture | storage | concurrency | dx
**Supersedes:** Implicit decision in commit `7f81111 feat(project): add load_exclusive sidecar lock for ocx.lock writes`
**Working note:** [`research_lockfile_locking_primitives.md`](./research_lockfile_locking_primitives.md)

## Context

`ocx.lock` is the canonical, machine-written, committed-to-VCS pin file produced by `ocx lock` and rewritten by `ocx update` (and, after Unit 7, by `ocx add` / `ocx remove`). It must:

1. Survive concurrent `ocx lock` invocations from CI (parallel jobs in the same checkout) without last-writer-wins corruption.
2. Stay readable by concurrent readers (`ocx pull`, IDEs, `git`, future `ocx env`/`ocx exec` from-lock paths) at every point in time — they must always observe a complete, parseable file.
3. Survive a crash mid-write — partial bytes on disk are a regression, never a recovery state.

The current implementation (`crates/ocx_lib/src/project/lock.rs:199-272`, `:328-424`) does this with two co-operating mechanisms:

- **Atomic data-file replacement.** `ProjectLock::save` writes a tempfile in the same directory, `sync_data`s it, `persist`s (rename) onto `ocx.lock`, then `sync_all`s the parent directory. Concurrent readers always see either the old inode or the new inode in full — never partial bytes. This part is correct and remains untouched by this ADR.

- **Sidecar advisory lock at `ocx.lock.lock`.** `ProjectLock::load_exclusive` opens a sibling file at `<path>.with_added_extension("lock")` (so `ocx.lock` → `ocx.lock.lock`) under `O_NOFOLLOW`, takes an exclusive `fs4` advisory lock via the shared `FileLock` primitive (`crates/ocx_lib/src/file_lock.rs:17-22`), and reads the data file inside the same `spawn_blocking` so no async yield ever lives between lock and read. Used by `crates/ocx_cli/src/command/lock.rs:84` and `crates/ocx_cli/src/command/update.rs:101`. The sidecar's stable inode is what makes the advisory lock meaningful when the data file is being atomically replaced underneath — locking the data file directly would put the writer's lock on a soon-to-be-orphan inode.

### Why redesign

User-reported friction (findings.md): the literal filename `ocx.lock.lock` is cosmetically surprising, pollutes `git status` after the first `ocx lock` run, and trips users into committing the sidecar by accident — which then becomes a stale lock file in another developer's checkout. The research artifact (`research_lockfile_locking_primitives.md`) ranks the existing pattern correct on the merits and recommends "do nothing or rename the sidecar". The user has explicitly asked for a redesign, so doing nothing is off the table; the open question is which redesign keeps the safety properties the current implementation already pays for.

### Why this is a One-Way Door Medium

The sidecar pathname is part of OCX's durable on-disk contract: any older `ocx` binary running in a checkout written by a newer binary will look at the *old* sidecar location and (a) acquire a different advisory lock or (b) skip serialization entirely. We cannot retroactively change minds about the chosen path without staging a downgrade-incompatible release. The reversal cost is bounded — one release cycle, one migration line in `.gitignore` — but real, so the decision belongs in an ADR rather than a commit message.

## Decision Drivers

- **No regression on the security gates already paid for** — `O_NOFOLLOW` (Block #6 from earlier review), the contention-vs-I/O-error discriminator (Warn #14), permission capping at 0o644 (Warn #8), `tools_content_equal` ordering invariants.
- **Reuse, not reinvent.** `FileLock` (fs4 wrapper) and `Path::with_added_extension` are already in place per `arch-principles.md` Utility Catalog. No new locking crate.
- **Cosmetic + git-status hygiene.** `ocx.lock.lock` is the user complaint; whatever replaces it should not appear as an untracked file by default.
- **Cross-platform.** macOS, Linux, Windows are all first-class targets. NFS-blind-spot caveats from the research must be acknowledged but cannot be made worse than today.
- **Crash safety.** A `ocx lock` killed at SIGKILL must leave the next invocation in a recoverable state — no orphan lock that traps subsequent runs.
- **Concurrent readers stay unblocked.** Pull / IDE / `git diff` workflows must not stall behind a writer.
- **No central daemon.** OCX is a single static binary; any solution that needs a long-running coordinator is rejected up front.
- **No optional fields, no fallback paths.** Per project memory (`project_breaking_compat_next_version.md`): the next version is allowed to break compat. A clean rename with a one-line gitignore note beats a runtime probe that supports both names.
- **Future reuse.** Unit 7 will introduce `ocx add` / `ocx remove`, both of which mutate `ocx.toml`. They will reuse this primitive; whatever name and primitive we pick must extend cleanly to a `ocx.toml`-side lock, not just `ocx.lock`.

## Industry Context & Research

Full research artifact: [`research_lockfile_locking_primitives.md`](./research_lockfile_locking_primitives.md).

**Trending approach:** uv (the closest peer Rust-ecosystem tool) uses a sidecar advisory lock named `<dir>.lock`, pattern-identical to OCX today. Cargo's cache layer uses two sentinel files. Poetry has no advisory lock on the lockfile itself. npm/pnpm/yarn rely on user discipline + atomic rename, with documented race conditions in pnpm.

**Key insight:** the *mechanism* (advisory lock on a stable-inode sidecar + atomic rename of the data file) is correct. The *name* `ocx.lock.lock` is what the user is reacting to. The research's Rank 2 — keep the mechanism, rename the sidecar to a hidden dotfile — is a one-line change with no semantic delta.

## Considered Options

### Option 1 — In-place advisory lock on `ocx.lock` itself (read-then-rename)

**Description:** Drop the sidecar. Acquire a shared `fs4` lock on `ocx.lock` for readers; acquire an exclusive lock on `ocx.lock` for writers, write a tempfile, atomically rename it onto `ocx.lock` while still holding the lock on the *old* inode.

| Pros | Cons |
|------|------|
| No surprising sidecar file; nothing extra in `git status`. | After the writer renames, its lock is on an orphan inode. A second writer that opens `ocx.lock` immediately afterwards opens the *new* inode and sees no lock — losing the mutual-exclusion guarantee for the window between rename and the new inode being locked. |
| One file in the project; mental model trivial. | Concurrent readers acquiring `lock_shared` on the old inode block the writer's `lock_exclusive` (or vice versa), making the simple "readers never block" invariant much harder to preserve. |
| | Crash mid-write leaves the *tempfile* on disk; the old inode survives. Recovery model differs from the current sidecar (where tempfile cleanup is the only concern). |
| | First-run race: writer A and writer B both `OpenOptions::create(true)` on a non-existent `ocx.lock` and race the lock acquisition. Requires careful primitive-level handling. |

### Option 2 — Sidecar advisory lock, status quo (`ocx.lock.lock`)

**Description:** Keep current implementation verbatim. Document `ocx.lock.lock` in `.gitignore` and the user guide.

| Pros | Cons |
|------|------|
| Zero risk; already shipping; passes all current security regression tests. | The user complaint that motivated this ADR survives. |
| Matches uv's pattern; familiar to peers. | Filename is genuinely ugly when written verbatim; users hit it in `git status` immediately after the first `ocx lock`. |
| | Each future tool the user adopts that uses the same pattern (e.g. ocx.toml save in Unit 7) doubles the visible cruft (`ocx.toml.lock`, `ocx.lock.lock`). |

### Option 3 — Sidecar advisory lock, dotfile rename (`.ocx.lock`)

**Description:** Mechanism-identical to Option 2. Rename only the path that `lock_path_for_sidecar(data_path)` returns from `data_path.with_added_extension("lock")` to `data_path.parent().join(".ocx.lock")`. Add `.ocx.lock` to the project's `.gitignore` (auto-generated by `ocx init` when Unit 7 lands, manual for existing projects).

| Pros | Cons |
|------|------|
| Cosmetic complaint resolved: `ls` does not show it by default; `git status` does not show it (most editors and `git` treat dotfiles as hidden by convention or `.gitignore` rule). | Hidden files are mildly less discoverable when something goes wrong (e.g. a stale lock on a CI runner). Mitigation: `ocx clean` documents it; `ocx info` could surface it. |
| All security/durability properties of Option 2 carry over byte-for-byte. The only diff vs current code is the path computation. | One-time migration: existing checkouts that committed `ocx.lock.lock` need to remove it from VCS and add `.ocx.lock` to `.gitignore`. |
| Single project-wide hidden lock used for both `ocx.lock` writes today and `ocx.toml` writes in Unit 7 — natural extension if we make the sidecar key off the *project directory*, not the data file (see Decision below). | Filename `.ocx.lock` could conflict on case-insensitive filesystems with a future `.ocx-Lock`-style artifact. None planned; not a real risk. |
| Consistent with the `.envrc` / `.editorconfig` / `.gitattributes` family — users already accept dotfiles in repo roots as tooling state. | |

### Option 4 — Lock on a parent-directory sentinel (`.ocx-lock` per project)

**Description:** Single per-project sentinel at `<project_root>/.ocx-lock`, used to serialize all `ocx`-driven mutations of project state (today: `ocx.lock`; tomorrow: `ocx.toml`). The lock is keyed off the project directory rather than the file being mutated. Identical primitive (`fs4` advisory exclusive), identical `O_NOFOLLOW` posture.

| Pros | Cons |
|------|------|
| Single source of mutual exclusion for all project-state writes. `ocx add`, `ocx update`, `ocx lock` all serialize against each other automatically — no per-file lock to coordinate later. | Coarser granularity. A future feature that wants to write two project files concurrently (e.g. `.ocx/cache/foo` while `ocx.lock` is being rewritten) is forced to wait. Speculative cost — there is no such feature today. |
| Matches the "mutex per project" mental model users have for `cargo`, `poetry`, `uv`. | Sentinel name has to be picked carefully. `.ocx-lock` looks file-shaped, not directory-shaped, which mildly leaks the impl. |
| Exactly one hidden file in `git status` regardless of how many project-state files OCX learns to write. | Slightly less obvious connection between the lock and the file being protected, when debugging. |

### Option 5 — Lock under `OCX_HOME` keyed by project-config-path digest

**Description:** Place the advisory lock at `$OCX_HOME/locks/<sha256(absolute_project_path)[:16]>.lock`. No artifact in the working tree at all.

| Pros | Cons |
|------|------|
| Working tree is pristine; nothing for `git status` or `ls` to show. | Cross-volume failure mode: if `$OCX_HOME` is on NFS or a different filesystem from the project, advisory-lock semantics may degrade silently (research §2 on flock+NFS). Cargo and uv both keep their advisory lock on the same volume as the protected file for this exact reason. |
| | Requires `$OCX_HOME` to be set / created before *any* project-state write. Adds an init-order dependency to commands that today work in a bare project directory with no OCX state. |
| | Stale-lock cleanup becomes a problem: a project that is moved/renamed/deleted leaves an orphan lock under `$OCX_HOME` keyed by its old path. Needs a sweeper, which is its own project (see `adr_clean_project_backlinks.md` for how much that costs). |
| | Debuggability: a user looking at `ocx.lock` cannot find the protecting lock in the same directory. Surprising to debug. |

## Decision Outcome

**Chosen Option:** **Option 4 — single per-project hidden sentinel at `<project_root>/.ocx-lock`.**

### Rationale

- It addresses the user's complaint directly (no `ocx.lock.lock` in `git status`, no double-`.lock` extension) without losing any of the security/durability properties the current implementation pays for.
- It keeps the lock on the same volume as the data — preserving cargo's and uv's correctness argument from the research, and avoiding Option 5's NFS / `$OCX_HOME`-init-order pitfalls.
- It anticipates Unit 7. `ocx add` / `ocx remove` mutate both `ocx.toml` and `ocx.lock` in a single transaction; with a per-project sentinel, that transaction is automatically atomic against any concurrent `ocx lock` / `ocx update` without introducing a second lock file. With Option 3 (per-data-file sidecar), Unit 7 would either need a second `.ocx.toml.lock` or an out-of-band rule about lock ordering.
- It picks the *coarsest correct* granularity. The research did not surface any case where finer-grained locking on `ocx.lock` vs `ocx.toml` would matter — both are sub-kilobyte writes that complete in microseconds.
- It overrides the research's Rank-1 recommendation ("keep current sidecar") on a single ground: the research evaluated the mechanism in isolation, not the cosmetic cost. With the user explicitly requesting redesign and Unit 7 about to add a second writeable project file, the rename is the lowest-risk way to honour both signals.

I considered Option 3 (dotfile rename of the per-data-file sidecar) as a near-tie: it is the smallest possible diff and survives the cosmetic complaint. Option 4 wins because of the Unit 7 consolidation argument — when we add `ocx.toml` writes, Option 3 forces a second sidecar (`.ocx.toml.lock` or similar), while Option 4 reuses the same `.ocx-lock` for both. Two-files-down-to-one is worth the slightly coarser granularity.

### Quantified Impact

| Metric | Before | After | Notes |
|--------|--------|-------|-------|
| Visible files in fresh project after `ocx lock` | `ocx.lock`, `ocx.lock.lock` | `ocx.lock` (and `.ocx-lock` hidden) | git status only shows `ocx.lock` |
| Lock contention granularity | Per data file (`ocx.lock`) | Per project directory | Coarser, but every project-state write today serializes through the same call site so no real loss |
| Lock files on disk after Unit 7 lands | `ocx.lock.lock` + `ocx.toml.lock` (hypothetical, sidecar-per-file extrapolation) | `.ocx-lock` (single) | |
| Cost of dependency churn | Adds nothing | Adds nothing | `fs4` already pinned; `with_added_extension` no longer needed for the lock path |

### Consequences

**Positive:**

- `git status` is clean after `ocx lock`. The only artifact a user sees in their working tree is the file they expected to see (`ocx.lock`).
- Single, project-wide serialization point. Future project-state writers (Unit 7's `ocx.toml` save, any future `.ocx/cache` writes if introduced) inherit the same primitive without coordinating new lock paths.
- No new dependency. Reuses `FileLock`, `O_NOFOLLOW` posture, contention/IO discriminator, all already covered by tests.
- The sentinel name is greppable — debuggers / forensic users can `find . -name .ocx-lock` from anywhere in a multi-project tree.

**Negative:**

- One-time user-facing migration: developers with `ocx.lock.lock` already committed must remove it from VCS and update `.gitignore`. Documented in the migration story below.
- Coarser granularity. Two unrelated project-state writes on the same project block each other. In practice both are sub-millisecond — no observed contention today, none expected.
- Hidden file is slightly less discoverable in normal browsing. Mitigated by documentation (`website/src/docs/user-guide.md` will gain a paragraph) and by `ocx clean`'s diagnostic surface.

**Risks:**

- **NFS / SMB silent degradation.** Per research §2, `flock` on NFS is silently emulated as `lockf` and the two do not conflict. This risk is unchanged from today; the lock is on the same volume as the data, so we are no worse off than the current sidecar. Mitigation: same as today — document `OCX_HOME` and project-on-network-FS as best-effort, with the same caveat the user guide already carries.
- **Older `ocx` binary reading newer checkout.** A user with the previous `ocx` build runs `ocx lock` in a directory that already has an `.ocx-lock` (from a teammate's newer build); the older binary opens `ocx.lock.lock`, not `.ocx-lock`, and writes concurrently to the same `ocx.lock`. Mitigation: this is a downgrade-incompatibility window of a single release. Pre-release notes in `CHANGELOG.md`/`workflow-release.md` cover it. The breaking-compat memo (`project_breaking_compat_next_version.md`) explicitly authorises this.
- **Filename collision** with a hypothetical user-authored `.ocx-lock`. Negligible — no documented `.ocx-lock` exists upstream; if someone has one, they are already using OCX wrong.

### How Would We Reverse This?

This ADR is a One-Way Door Medium. To reverse:

1. Pick a new sentinel name (back to `ocx.lock.lock`, or any other) and ship it as the next version.
2. Add a one-line note to `CHANGELOG.md` and the user-guide migration table: "the project lock sentinel name moved from `.ocx-lock` back to `ocx.lock.lock`; remove `.ocx-lock` from your `.gitignore` and add the new name."
3. No code-side compatibility shim — single name on each version, in keeping with `project_breaking_compat_next_version.md`.

The reversal cost is bounded: one release, one diff in `lock_path_for_sentinel(...)`, one user-guide paragraph. No durable state besides the sentinel filename itself depends on this choice.

## Migration Story

`ocx.lock.lock` exists today in any project that has run `ocx lock` or `ocx update` since commit `7f81111`. The first time the new binary runs in such a project:

1. **It does nothing automatic to `ocx.lock.lock`.** The new binary computes the sentinel as `<project>/.ocx-lock`, never opens `ocx.lock.lock`, and never writes it. The stale file is harmless (just an empty advisory-lock target nobody locks anymore) but cosmetically confusing.

2. **`.gitignore` rule.** OCX repos should add `.ocx-lock` to the project `.gitignore` so the sentinel never enters VCS. Two cases:
   - **`ocx init` (Unit 7) writes `.gitignore` entries.** New projects get `.ocx-lock` automatically.
   - **Existing projects.** Documented in the user-guide upgrade note: append `.ocx-lock` to `.gitignore`. One line. If the user previously gitignored `ocx.lock.lock` they should keep that line until they have removed `ocx.lock.lock` from VCS history; both lines coexist without conflict.

3. **Cleaning up `ocx.lock.lock`.** Manual one-liner per the user-guide migration table:
   ```sh
   git rm ocx.lock.lock 2>/dev/null || true
   rm -f ocx.lock.lock
   ```
   Not gated by code: the new binary keeps working whether or not `ocx.lock.lock` is present.

4. **`ocx clean` is NOT extended to remove stale `ocx.lock.lock`.** Cleaning a sibling file in the working tree is outside `ocx clean`'s charter (object-store GC). Keeping the cleanup manual avoids any accidental file deletion from `ocx clean --force`.

5. **`.gitattributes` advisory unchanged.** The existing `ocx.lock merge=union` advisory in `crates/ocx_cli/src/command/lock.rs:104-107` continues to apply only to `ocx.lock`. `.ocx-lock` is gitignored, so it never reaches the merge driver.

## Implementation Footprint (Unit 5b — design only here)

Bullet-level. No code in this ADR.

- **`crates/ocx_lib/src/project/lock.rs:222-272` (`load_exclusive`)**
  - Replace `let lock_path = path.with_added_extension("lock");` with a call to a new helper `sentinel_path_for(path) -> PathBuf` that returns `path.parent().unwrap_or_else(|| Path::new(".")).join(".ocx-lock")`.
  - Co-locate the helper next to `lock_path_for` (lines 16-34) so both file-naming functions live in one place.
  - Update the doc comment on `load_exclusive` to describe `<project>/.ocx-lock` instead of `<path>.lock`.
  - Update the symlink-rejection regression test fixture path (line 1273) accordingly.

- **`crates/ocx_lib/src/file_lock.rs:17-22`**
  - No code change. This module already exposes the right primitive; reuse `try_exclusive` exactly as today.

- **`crates/ocx_cli/src/command/lock.rs:84-98` and `crates/ocx_cli/src/command/update.rs:65,101`**
  - No code change at the call sites — both call `ProjectLock::load_exclusive(&lock_path)` and the new implementation continues to accept the data-file path. The sentinel resolution is internal.
  - Verify the eprintln messages still read sensibly when the `Locked` error fires (the path quoted in the error message becomes `<project>/.ocx-lock`, which is the truth — nothing to fix).

- **New helper module** (or addition to `lock.rs`): `pub(crate) fn sentinel_path_for(data_path: &Path) -> PathBuf`. Exact implementation:
  - `data_path.parent().unwrap_or_else(|| Path::new(".")).join(".ocx-lock")`.
  - Tested by a new unit test analogous to `lock_path_for_always_produces_ocx_lock_in_config_dir` (line 1300).

- **`.gitignore` (project-template)**: when Unit 7 lands `ocx init`, append `.ocx-lock` to the generated `.gitignore`. Unit 5b does *not* touch the user's `.gitignore`; that is a Unit-7 concern and a user-side migration concern.

- **Documentation** — Unit 5b emits the docs change in the same commit that flips the sentinel:
  - `website/src/docs/user-guide.md` — add a paragraph in the locking section describing `.ocx-lock` and the `git rm ocx.lock.lock` migration step.
  - `website/src/docs/reference/command-line.md` — clarify under `lock` and `update` that the project sentinel is `.ocx-lock` (visible to humans) rather than naming the old sidecar.
  - `CLAUDE.md` (Environment Variables / Storage Layout block in `product-context.md`): no change required — neither file currently names `ocx.lock.lock`.
  - `CHANGELOG.md` — one-line breaking-change note: "Project lock sentinel renamed from `ocx.lock.lock` to `.ocx-lock`. Add `.ocx-lock` to your `.gitignore`; `git rm ocx.lock.lock` if you committed it."

## Test Additions (Unit 5b)

Extend `test/tests/test_project_lock.py` with reader-during-write coverage on the new design. Concrete new test cases:

1. **`test_concurrent_reader_during_writer_does_not_block`** — start an `ocx lock` in a thread (or as a backgrounded subprocess) that sleeps inside the resolver while holding `.ocx-lock`. Concurrently call `ProjectLock::load(&path)` (the unlocked read path used by `ocx pull` and friends). The reader must complete promptly with either the old or new content — never partial bytes, never blocked on the writer.

2. **`test_concurrent_writer_blocks_with_locked_kind`** — start one `ocx lock`, then a second; assert the second exits with `ProjectErrorKind::Locked` (existing `Locked` path; renamed sentinel must continue to surface this same error class). Existing `load_exclusive_blocks_second_writer` (line 1239 in `lock.rs`) covers the unit-test side; the new acceptance test covers the CLI surface.

3. **`test_sentinel_is_dotfile_not_double_lock`** — after `ocx lock`, assert `<project>/.ocx-lock` exists and `<project>/ocx.lock.lock` does not. Locks-in the rename invariant.

4. **`test_sentinel_is_gitignored_when_ocx_init_runs`** — gated on Unit 7. After `ocx init` followed by `ocx lock`, assert `.ocx-lock` is matched by `.gitignore` (use `git check-ignore`). If Unit 7 hasn't landed when 5b is reviewed, defer this test to Unit 7's test additions.

5. **`test_stale_ocx_lock_lock_is_harmless`** — pre-create an empty `ocx.lock.lock` in the project (simulating a migration in progress), then run `ocx lock`. Expected: success; `ocx.lock.lock` is left untouched (we don't auto-delete user files); the new `.ocx-lock` is created alongside.

The existing Unix-only Block #6 regression (`load_exclusive_rejects_symlink_at_sidecar_path`, line 1267) MUST be updated to plant its symlink at the new sentinel path (`<dir>/.ocx-lock`) and assert the same `ProjectErrorKind::Io` outcome. The security gate is the contract, not the filename — the renamed test is a 1-line fixture change.

## Links

- Plan: [`/home/mherwig/.claude/plans/auto-findings-md-eventual-fox.md`](file:///home/mherwig/.claude/plans/auto-findings-md-eventual-fox.md) — Unit 5
- Research: [`research_lockfile_locking_primitives.md`](./research_lockfile_locking_primitives.md)
- Sibling routing ADR (shape reference): [`adr_index_routing_semantics.md`](./adr_index_routing_semantics.md)
- Storage subsystem rule: `.claude/rules/subsystem-file-structure.md`
- Utility catalog (FileLock row): `.claude/rules/arch-principles.md`
- Project memory:
  - `project_breaking_compat_next_version.md` — authorises clean break vs compat shim
  - `project_config_file_naming.md` — `ocx.toml` (project root) vs `config.toml` (tier) — does not collide with `.ocx-lock`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-29 | worker-architect (Opus) | Initial draft — Option 4 (`.ocx-lock` per-project sentinel) chosen. |
