# Durable State, Stores and Teardown

Writes that must survive a power cut, a content-addressed store that hardlinks
into install trees, staging only recovery cleans up, and what runs on the way
down. Loads on any change to a file write, blob publish, install-tree link,
lock, GC pass, or `Drop` impl. Ownership shape — `.clone()`, `&mut self`
getters, `Arc<Mutex<_>>` — lives in [api-and-idioms.md](api-and-idioms.md).

Contents: [Durable Writes](#durable-writes) ·
[The Content-Addressed Store](#the-content-addressed-store) ·
[Staging, Orphans and Locking](#staging-orphans-and-locking) ·
[Drop, Panics and Poisoning](#drop-panics-and-poisoning) · [Platform Gaps](#platform-gaps) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

## Durable Writes

Atomic means one thing: a reader sees the old bytes or the new. Durability is
the fsync, and the one that counts is on the *parent directory* — that is what
makes the rename survive power loss. Windows has no documented equivalent, so
this sequence is a Unix durability story and a Windows atomicity-for-readers
story. Nothing in the tree may claim otherwise ([gaps](#platform-gaps)).

```rust
let tmp = NamedTempFile::new_in(parent)?; // same filesystem by construction
tmp.as_file().write_all(bytes)?;
set_mode(&tmp)?;                          // before the sync: sync_data may skip metadata
tmp.as_file().sync_all()?;                // failure here is fatal, never retried
tmp.persist(target)?;                     // persist itself syncs nothing
#[cfg(unix)]
File::open(parent)?.sync_all()?;          // what makes the rename survive power loss
```

| ID | Rule | Verification | Severity |
|---|---|---|---|
| STATE-1 | Route every write to a cache, lockfile, or install-tree path through one crate-local durable-write helper. Fifty ad-hoc sites cannot each carry their own correctness story. | `git diff -U0 -G'fs::write\(' -- '*.rs'`; `git diff -U0 -G'File::create\(' -- '*.rs'` — restrict to added lines on a diff (a mature tree carries hundreds of legitimate test-module writes, so a whole-tree scan is not a gate); an added write outside the crate-local durable-write helper is a finding, and hits in modules the change does not touch are discarded. Enforce with `clippy.toml` `disallowed-methods = ["std::fs::write", "std::fs::File::create"]` | MUST |
| STATE-2 | Create the temp file in the final target's own directory (`NamedTempFile::new_in(parent)` / `Builder::tempfile_in`), never `NamedTempFile::new()`, `tempfile::tempfile()`, or `env::temp_dir()` — that makes the rename same-filesystem by construction. The global temp dir is a tmpfs in containers and CI, so `EXDEV` fires in production and never in dev. Same rule carries the Windows half: `CreateHardLinkW` is same-volume-only, so a blob staged in `%TEMP%` cannot be linked into place at all. `link(2)` is stricter still — same *mount*, not merely same filesystem; a bind-mounted second view of one filesystem returns `EXDEV`. | `rg -n --type rust --glob '!external/**' -e 'NamedTempFile::new\(\)' -e 'tempfile::tempfile\(\)' -e 'env::temp_dir\(\)' .` — zero hits on any path that is later persisted; a hit that only feeds a throwaway test fixture is not one | MUST |
| STATE-3 | Sync the temp file before `persist`/`rename`, and fsync the parent directory after it, `#[cfg(unix)]`-gated. `persist` documents that it syncs neither contents nor directory. Use `sync_all`; `sync_data` only when no metadata mutation (`set_permissions`, `set_times`) follows the sync, otherwise the mode change is not durable. There is no Windows equivalent of the parent fsync: omit the step rather than stubbing a no-op that reads as done — **and do not then describe the Windows write as durable.** Governs *mutable* targets (cache, lockfile, index, pointer); a digest-named path takes STATE-28's persist-if-absent step in place of the replacing rename. | Read the one helper from STATE-1; plus an integration test that writes, hard-exits via `std::process::exit` (no unwind), and re-reads the target in a fresh process | MUST |
| STATE-4 | Treat a failed `fsync`/`sync_all` as fatal for that data. Never retry it and continue — Linux may mark the failed page clean, so a subsequent successful `fsync` is a false signal. This is the fsyncgate consensus adopted by Postgres, InnoDB and WiredTiger. | `rg -n --type rust --glob '!external/**' -B3 -A3 -e 'sync_all\(\)' -e 'sync_data\(\)' .` — no hit is inside a retry loop or followed by `.ok()`/`let _ =` | MUST |
| STATE-5 | Make a multi-file install atomic with a content-addressed store plus exactly one externally visible rename (pointer file or staged-directory swap) as the final step. `rename` is atomic per path only; N independent renames means a concurrent reader can observe a half-installed package. **The one permitted replacing rename is the pointer, never a blob:** a digest-named path is written once via STATE-28 and never replaced, so "exactly one rename" counts pointer swaps only. | Reading heuristic — `rg -n --type rust --glob '!external/**' -e '\.persist\(' -e 'fs::rename\(' .`; discard every hit outside the install module the change touches, then within that module exactly one hit may target a path a concurrent reader consults, and no hit may target a digest-named path | MUST |
| STATE-6 | Verify the digest of the fully assembled blob after a resumed download, never only the newly fetched byte range or the `Content-Range` header. Corruption in the first attempt survives a clean resume of the remainder; a suffix check verifies nothing. | Trace the hasher's input in the resume path — it must be fed from byte 0 or the file re-opened end-to-end. Fault-injection test: corrupt byte 0 of a partial file, resume, assert rejection | MUST |

## The Content-Addressed Store

A hardlink is not a copy, and a digest is a claim about bytes, not about an
inode. Two hazards follow, failing differently: in-place mutation of a blob
corrupts every install sharing that inode, instantly and silently;
replace-by-rename keeps the bytes but rebinds the canonical name to a
*different* inode than the one installs hold, so a mark-and-sweep GC deletes a
live install's target. Write-once publish (STATE-28) closes the second;
read-only blobs (STATE-31) close the first, and are the only real containment
control against an agent editing a hardlinked file in place.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| STATE-28 | Publish a CAS blob with a persist-if-absent primitive; a digest-named path is written exactly once and never replaced. Content is determined by the digest, so there is never a legitimate reason to overwrite. Primitives: `std::fs::hard_link` (documented to error on an existing destination on every platform backend — the portable choice), `renameat2(RENAME_NOREPLACE)` (Linux ≥3.15, per-filesystem, not NFS), `renamex_np(RENAME_EXCL)` (macOS, gated on `VOL_CAP_INT_RENAME_EXCL`), `open(2)` with `O_CREAT`+`O_EXCL`, `clonefile(2)` (macOS, also `EEXIST`), or `CreateHardLinkW` + `DeleteFile` of the temp name on Windows. The temp name lives in the canonical path's own directory (STATE-2). | Every write targeting a digest-named path uses a NOREPLACE/EXCL/link primitive; a plain `rename`/`persist`/`File::create` onto such a path is a bug. Race test: two writers persisting the same digest, both succeed, no temp file left behind | MUST |
| STATE-29 | Treat `EEXIST`/`ERROR_ALREADY_EXISTS` from a CAS persist as success: discard the loser's temp file and continue. Under content addressing the destination already existing means someone else published byte-identical content — this is `cacache-rs`'s literal implementation. Surfacing it as an error invites the "fix" that reintroduces replace semantics (`--force`, overwrite-on-conflict), which is the bug STATE-28 exists to prevent. Agent-facing messages read "already published, expected under concurrency", not as a failure to route around. | The persist call site maps `AlreadyExists` to `Ok`; grep the publish path for `force`/`overwrite` flags reachable from a conflict handler — there should be none | MUST |
| STATE-30 | A blob's canonical digest path must not exist until its content is completely written and verified; absence means "not yet published", never "corrupt". This is what makes "the path exists" a usable readiness signal for every other process: a linker finding the path missing waits, polls, or fetches, and never concludes the store is damaged. | Kill the writer mid-write in a test; assert the canonical path never appears. Read paths treat `NotFound` on a digest path as not-yet-published | MUST |
| STATE-31 | Mark store blobs read-only immediately after persist, on every platform, and say why when a write to one is refused. The read-only bit is the only thing that turns "someone edited an installed, hardlinked file in place" from silent corruption of every project on the machine into an immediate `EACCES`/`EPERM`. Nothing about the path tells an agent the file is shared, so the failure must come from the filesystem. The error names the cause ("shared, immutable package-store entry — change the source package or use a local override"), not a bare errno. Cap permissions *before* the sync, or use `sync_all` (STATE-3). | After a normal install, open an installed file for writing outside the package manager and assert it fails; `rg -n --type rust --glob '!external/**' 'set_permissions' .` — discard hits outside the store's publish module, and the ones left must include the read-only cap | MUST |
| STATE-32 | Compute GC liveness by scanning what references a blob (lockfiles, manifests, install-tree state), never from `st_nlink`; run reachability under a shared lock, delete under an exclusive lock, and re-check liveness for anything newly referenced between the two phases. `st_nlink` counts directory entries the package manager does not know about and goes stale against its own bookkeeping in both directions. The re-check is mandatory here rather than inherited from ostree's escalation pattern on faith. | Race test — start the reachability scan, install a new object concurrently, let the delete phase run, assert the new object survives. Second test: uninstall one of two projects sharing a digest, run GC, assert the digest survives | MUST |
| STATE-33 | Verify a blob's digest on write (mandatory), never on link (pointless), and make on-read verification an explicit opt-in command. On write you already hold the bytes and both a size and a hash mismatch are hard errors before persist. A hardlink touches zero bytes, so re-verifying on link re-reads every blob on every install and destroys the reason for hardlinking. `pnpm store status` is a separate command precisely because it is not implicit. | The write path hashes before persist; the link path contains no hashing; an explicit `verify`/`status` subcommand exists and is not called from the normal install path | SHOULD |
| STATE-34 | Materialise into an install tree by trying reflink/clonefile, then hardlink, then copy — selected by catching the failure of each, not by pre-detecting filesystem type. CoW clones (`FICLONE` on btrfs/XFS-with-reflink/OCFS2, `clonefile(2)` on APFS, ReFS block cloning on Server 2016+) make the "user edits the installed file" hazard structurally impossible rather than merely permission-denied, but their support matrix is far narrower than hardlinks and ReFS block cloning carries real constraints (cluster alignment, 4 GB region cap, 8175 shares per region, matching integrity-stream and sparse settings). Never let a stage of the ladder silently no-op. | Test matrix across same-fs, different-fs and different-device targets; assert correct content in all three and that the chosen stage is observable (logged/reported), never skipped silently | SHOULD |
| STATE-35 | Do not depend on undocumented or unavailable Windows link behaviour. (a) `CreateHardLinkW`'s reference page is silent on an existing destination, so route through `std::fs::hard_link`, whose error-on-existing contract *is* documented, rather than branching on a raw `GetLastError()`; (b) `CreateHardLinkW` is explicitly unsupported on ReFS, so a volume chosen for block cloning cannot also serve STATE-34's hardlink rung; (c) links cap at 1023 per file — a hard limit that must fall back to copy, not a performance cliff; (d) Windows updates a link's directory-entry size and attribute data only through the link the change was made on, so cached directory-listing metadata about a hardlinked file is not trustworthy — open it. | A CI test on the actual Windows runner that hardlinks onto an existing destination and asserts+logs the observed behaviour, re-run when the image changes; `rg -n --type rust --glob '!external/**' -e 'GetLastError' -e 'ERROR_ALREADY_EXISTS' .` — no publish-path logic keys on the literal code, hits confined to a Windows shim are not findings; the 1024th link falls back to copy in a test | MUST |

## Staging, Orphans and Locking

`Drop` does not run on `SIGKILL`, the default `SIGINT` disposition, past
`process::exit` or `abort`, under `panic = "abort"`, or after `mem::forget` —
so no guard is the crash-safety story: one staging place, swept at every
startup. The blob store needs no lock, and not because "the last rename wins,
the bytes are identical either way" (false once hardlinks exist) but because
under STATE-28 the loser never writes at all.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| STATE-7 | Put every staging file/directory under one fixed, well-known location, and never rely on a signal handler, panic hook, or `Drop` guard to remove it. Recovery-as-the-only-cleanup-path collapses every interruption mode onto one tested code path. | `rg -n --type rust --glob '!external/**' -A10 'impl Drop for' .` — any body calling `remove_dir_all`/`remove_file` as its *correctness* story (not as best-effort tidying) is a finding | MUST |
| STATE-8 | Run the orphan sweep at the start of every run, before any new work — not only from an explicit `clean` subcommand. STATE-7's recovery path only exists if it actually executes; sweeping on demand means interrupted runs never converge. | `rg -n --type rust --glob '!external/**' -e 'stale_entries' -e 'fn sweep_' -e 'fn gc_' -e 'fn prune_' .` — `prune` is as common a spelling as `sweep`, so both run; confirm a caller on the normal startup path. Integration test: plant a plausible orphan, run the normal command, assert it is gone | SHOULD |
| STATE-9 | No lock for content-addressed blob writes. One exclusive advisory lock per *mutable* artifact, and choose its location by inode stability: lock the data file itself when its inode is stable, a dedicated locks directory when the data is atomic-rename-replaced. Rename-replaced mutable data rotates its inode, so a lock held on the old inode guards nothing; a sidecar next to the guarded data is the specific broken form. The lock-free claim rests on STATE-28's write-once publish, not on rename semantics. | `rg -n --type rust --glob '!external/**' -e 'lock_exclusive\(' -e 'lock_shared\(' -e '[.:]try_lock\(' .` — `fs4` renamed `lock_exclusive` to `try_lock`, so a two-spelling grep reports zero on a crate that locks; hits cluster on lockfile/index/pointer code, not blob writes; each lock target is in the locks directory unless its inode provably never rotates | SHOULD |
| STATE-10 | Either detect a network-filesystem cache directory and degrade explicitly, or document that the cache must be local. `rename(2)` failure over NFS is documented as ambiguous (the file may or may not have been renamed), `flock`/NLM has known split-brain behaviour, `renameat2(RENAME_NOREPLACE)` returns `EINVAL` on NFS, and `O_EXCL` is unreliable on pre-NFSv3 — silently reusing the local-disk code path there is an untested assumption. `link(2)` is the portable create-if-absent primitive the `open(2)` man page itself recommends. | Documentation/design review of the cache-directory-selection function | CONSIDER |

## Drop, Panics and Poisoning

| ID | Rule | Verification | Severity |
|---|---|---|---|
| STATE-11 | No `.unwrap()`, `.expect()`, or `panic!` in a `Drop::drop` body — a panic there during an in-progress unwind is a double panic and aborts the process immediately, discarding every remaining guard. | `rg -n --type rust --glob '!external/**' -A8 'impl Drop for' .` — no printed body contains `.unwrap()`, `.expect(` or `panic!` | MUST |
| STATE-12 | Fallible or blocking teardown gets an explicit `close()`/`commit()`/`shutdown()` returning `Result`; `Drop` is a synchronous, non-blocking, best-effort backstop only. `Drop` cannot be `async` and there is no stable `AsyncDrop`, so awaiting teardown from `drop` is not expressible, and `block_in_place` panics on a `current_thread` runtime. | `rg -n --type rust --glob '!external/**' -A10 'impl Drop for' .` — a printed body reaching `.lock()`, `std::fs::`, `reqwest::`, `block_on` or `block_in_place` is acceptable only if provably fast and local, and says so in a comment | SHOULD |
| STATE-13 | A "you forgot to `commit()`" `debug_assert!` bomb in `Drop` must be guarded by `!std::thread::panicking()`. Unguarded, it fires *during* an unrelated unwind and converts one failure into an abort — exactly the failure it was meant to surface. | `rg -n --type rust --glob '!external/**' -A6 'impl Drop for' .` — every `debug_assert` in a printed body sits with a `thread::panicking()` guard | MUST |
| STATE-14 | `panic = "abort"` may only be set on a profile whose binary owns no `Drop`-based cleanup and no `resume_unwind` propagation, and the manifest must say why. Abort skips every `Drop` on every thread, silently disabling temp-file, lock-file and partial-write guards. Legitimate for a self-contained shim; not a routine size tweak, and not a workspace-wide ban either — it is a per-profile decision. | `rg -n --glob '**/Cargo.toml' --glob '!external/**' 'panic\s*=\s*"abort"' .` — each hit sits under a profile with a comment naming the binary and asserting it is guard-free | MUST |
| STATE-15 | Never call `std::process::exit` (or `libc::_exit`) after any `Drop`-bearing guard has been constructed — the same hazard as STATE-14, triggered by a call instead of a build profile. | `rg -n --type rust --glob '!external/**' 'process::exit\(' .` — hits allowed only in `main`'s final statement after all guards have dropped | MUST |
| STATE-16 | Every `.lock().unwrap()` / `.read().unwrap()` / `.write().unwrap()` carries a one-line poison-policy comment: fatal, recover, or non-poisoning. A blanket unwrap conflates "this corruption must halt the process" with "this is fine to keep using", so one panicking thread wedges state that was never corrupted. Poison detection is documented as best-effort, so an unpoisoned lock is not proof of consistency. | `rg -n --type rust --glob '!external/**' -e '\.lock\(\)\.unwrap\(\)' -e '\.write\(\)\.unwrap\(\)' -e '\.read\(\)\.unwrap\(\)' .` — a hit with no adjacent `// poison-policy: …` is a finding; restrict to added lines on a diff. Recovery shape is `unwrap_or_else` returning the `PoisonError`'s `into_inner()`, optionally with `clear_poison()` | SHOULD |
| STATE-17 | Do not migrate `once_cell::sync::Lazy`/`OnceCell` to `std::sync::LazyLock`/`OnceLock` without auditing the init closure for panics. `once_cell` leaves the cell empty on a panicking init and retries on next access; `LazyLock` poisons **unrecoverably** — every future access panics forever, with no `into_inner` escape. A mechanical "drop the dependency" refactor silently changes recoverability. | `rg -n --type rust --glob '!external/**' -e 'once_cell::sync::Lazy' -e 'once_cell::sync::OnceCell' .` before any such PR — zero hits means the crate has no direct `once_cell` use and the rule is moot, not satisfied; any init closure containing I/O, parsing, `?`, `unwrap` or `expect` is a do-not-migrate | SHOULD |

## Platform Gaps

Not caveats to compress away. Each is a place the platform makes no guarantee,
and a rule that implies one is the defect.

- **Windows durability is not guaranteed.** The parent-directory fsync is
  `#[cfg(unix)]` and no Windows analogue is documented, so STATE-3 delivers
  atomicity-for-readers there and **no researched power-loss guarantee at
  all**. `ReplaceFileW` is a documented multi-step operation with
  partial-failure error codes, not a kernel transaction. Whether
  `FlushFileBuffers` suffices for a `MoveFileExW` publish on NTFS/ReFS is
  unanswered, and transactional-NTFS's deprecation leaves no replacement. Any
  comment, message or changelog claiming Windows durability is wrong.
- **`CreateHardLinkW`'s behaviour on an existing destination is undocumented.**
  Microsoft's reference page is silent; `ERROR_ALREADY_EXISTS` is community
  observation, not a guarantee. `std::fs::hard_link` documents
  error-on-existing on every backend — route through it (STATE-35).
- **No single-call rename-no-replace is confirmed on Windows.**
  `FILE_RENAME_INFO_EX` is unverified; that is why STATE-28's Windows
  primitive is link-then-delete rather than a rename flag.
- **Tampering is out of scope by default.** No surveyed CAS — cacache-rs,
  pnpm, Nix — re-verifies the existing blob on `EEXIST`; all three trust the
  addressing scheme. Nothing here defends against modification between the
  winner's write and a later read; a threat model needing that adds the
  comparison deliberately.
- **Cited upstream patterns are unconfirmed.** Whether ostree's
  shared→exclusive escalation closes the "object linked after the reachability
  snapshot" window was not verifiable — hence STATE-32's mandatory re-check.
  Nix chmodding store paths to `444` is folklore; only the immutability
  invariant is sourced. **Do not cite cargo as a hardlink-CAS reference** — its
  registry-cache internals were unreachable in research.

## What Agents Get Wrong Here

1. **`persist()` read as "durable"** because the temp-file idiom looks
   familiar. The docs' disclaimer is one sentence and it gets skipped; whole
   trees ship with zero `sync_all` calls. → STATE-1, STATE-3.
2. **Editing a file inside an install tree in place**, because nothing in the
   path says it is a hardlink into a shared store. The blast radius — every
   project on the machine using that digest — is invisible from the directory
   the agent is in, and the corruption is instant. → STATE-31.
3. **"Fixing" an `AlreadyExists` from a publish step** with a force/overwrite
   path, reintroducing the replace semantics the CAS design rules out. The
   error is the concurrency protocol working. → STATE-28, STATE-29.
4. **A `Drop` impl deleting the staging directory, offered as the
   interruption-safety story.** "Runs on unwind" is not "runs always".
   → STATE-7.
5. **`NamedTempFile::new()` instead of `new_in(parent)`** — what autocomplete
   surfaces; passes every test where `/tmp` and `$HOME` share a filesystem.
   → STATE-2.
6. **`.lock().unwrap()` everywhere**, because it is what every tutorial shows;
   `into_inner()`/`clear_poison()` never appear unprompted. → STATE-16.
7. **`st_nlink` as the "is anything still using this blob" signal** in GC. The
   obvious kernel counter, and authoritative-looking: it counts entries the
   package manager never recorded and misses ones it did. → STATE-32.
8. **`panic = "abort"` because a summary called it smaller and faster**, with
   no link drawn to Drop-based cleanup; and `.unwrap()` in `Drop::drop` for
   "cleanup that handles errors" — the `?` form fails to compile, the unwrap
   form only misbehaves during an unwind. → STATE-14, STATE-11.
9. **Branching on a Windows error code copied from a forum answer.**
   `ERROR_ALREADY_EXISTS` from `CreateHardLinkW` is in no vendor doc, and it
   passes on whichever image CI runs. → STATE-35.
10. **`once_cell::Lazy` → `LazyLock` as a "modernize to std" pass.** Compiles,
    passes happy-path tests, bricks a static forever on a panicking init.
    → STATE-17.
11. **An advisory lock around blob-store writes "to be safe"** instead of
    reasoning that persist-if-absent makes them non-conflicting. → STATE-9.

## Sources

- [rename(2)](https://man7.org/linux/man-pages/man2/rename.2.html) — the atomicity contract, `EXDEV`, the NFS ambiguity caveat, `renameat2(RENAME_NOREPLACE)`'s per-filesystem matrix
- [link(2)](https://man7.org/linux/man-pages/man2/link.2.html) — "if newpath exists, it will not be overwritten", the persist-if-absent guarantee, and the same-*mount* restriction
- [`std::fs::hard_link`](https://doc.rust-lang.org/std/fs/fn.hard_link.html) — documents error-on-existing-destination uniformly across platform backends
- [`tempfile::NamedTempFile`](https://docs.rs/tempfile/latest/tempfile/struct.NamedTempFile.html) — states plainly that `persist` syncs neither contents nor directory
- [PostgreSQL's fsync surprise](https://lwn.net/Articles/752063/) — the incident behind fatal-not-retriable fsync failure
- [ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew) — proof Windows' closest rename analog is multi-step with partial-failure codes
- [CreateHardLinkW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createhardlinkw) — the 1023-link cap, the no-ReFS support table, and the documented silence on existing destinations
- [Rust API Guidelines: dependability](https://rust-lang.github.io/api-guidelines/dependability.html) — C-DTOR-FAIL and C-DTOR-BLOCK, the normative basis for the `Drop` rules
