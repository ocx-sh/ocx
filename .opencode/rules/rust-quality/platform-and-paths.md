# Platform, Paths and Filenames

Paths from outside the process, filenames the extractor must survive, the
Windows file lifecycle, and the clock. Loads whenever a change touches a path,
a filename, an archive entry, a rename, a link, `cfg(target_os)`, or `now()`.

Contents: [Paths and Containment](#paths-and-containment) · [Filenames and the Extractor](#filenames-and-the-extractor) ·
[Canonicalisation and Identity](#canonicalisation-and-identity) · [Windows File Lifecycle](#windows-file-lifecycle) ·
[Reparse Points and Containment](#reparse-points-and-containment) · [Time and Clocks](#time-and-clocks) ·
[Platform Divergence and CI](#platform-divergence-and-ci) · [What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

## Paths and Containment

`Path::join` is never a security boundary — rust-lang closed that WONTFIX saying
the guard is the caller's job. Strictness tracks provenance: a locally-authored
tree keeps canonicalize-and-compare with a named CWE-367 residual-risk comment;
registry-supplied entries get a directory-handle resolver (PLAT-40).

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PLAT-01 | Never join an external-origin component — archive entry, manifest field, lockfile value, CLI sub-path, env var — onto a trusted root directly; route every one through the single shared containment helper. `join`/`push` return the RHS unchanged when it is absolute or carries a Windows drive prefix. | `git diff -U0 -G'\.join\(' -- '*.rs'` — the gate is the change, not the tree: on every extraction, install or cache-write diff, each added `.join(` reaches `contain(`/`AnchoredPath`/`escapes_root`, carries `// TRUSTED:`, or joins a compile-time literal | MUST |
| PLAT-02 | Reject `Component::ParentDir`, `RootDir` and `Prefix`, and require at least one `Normal` component, **before** any filesystem call — the only layer that works on a path that does not exist yet, which is the extraction case. | `rg -n --type rust --glob '!external/**' -e 'Component::ParentDir' -e 'Component::RootDir' -e 'Component::Prefix' .` — hits confined to the guard module | MUST |
| PLAT-03 | `debug_assert!` is never the sole guard on an external-origin path: compiled out in release, and `is_relative()` admits `..` regardless. | `rg -n --type rust --glob '!external/**' -e 'debug_assert.*is_relative' -e 'debug_assert.*is_absolute' .` — each hit paired with a real guard | MUST |
| PLAT-04 | Deny `clippy::join_absolute_paths`. It fires only on string literals, so it complements PLAT-01 and never substitutes for it. | `cargo clippy --workspace --all-targets -- -D clippy::join_absolute_paths` | SHOULD |
| PLAT-07 | Build paths with `join`/`push` only. Never `format!("{}/{}", p.display(), x)`. Under a `\\?\` prefix forward slashes and `.`/`..` stop being resolved, so a formatted path that works on Linux breaks the moment the base came from canonicalize. | `rg -n --type rust --glob '!external/**' 'format!\("\{\}[/\\]' .`; `rg -n --type rust --glob '!external/**' 'display\(\)\)' .` — a hit outside a log macro or `write!` is a finding | MUST |
| PLAT-08 | Every check-then-act pair on the same path is a bug until proven otherwise. Replace with one handle-based operation: `create_new(true)`, a `Dir`-relative open, or `File::metadata` on the already-open handle. | `rg -n --type rust --glob '!external/**' -e 'if !?[\w.()]+\.exists\(\)' -e 'if !?[\w.()]+\.is_dir\(\)' -e 'if !?[\w.()]+\.is_file\(\)' .` — discard hits outside the module the change touches; in what is left, a guard whose body then acts on the same path is a finding | MUST |
| PLAT-09 | Create files and directories with their final permissions in the creation call (`OpenOptionsExt::mode`, `DirBuilderExt::mode`). Never create then `set_permissions`: between the two the entry exists at the default umask and any local user can open it, and the later chmod does not revoke an already-open handle. | `rg -n -B4 --type rust --glob '!external/**' 'fs::set_permissions\(' .` — no preceding `create_dir`/`File::create` on the same path; `rg -n --type rust --glob '!external/**' -e 'DirBuilderExt' -e 'OpenOptionsExt' .` confirms the atomic form | MUST |
| PLAT-10 | Every `fs::`/`File::` error carries the path it was operating on — `fs_err as fs`, or one internal wrapper; partial coverage is worse than none. `io::Error` carries neither path nor backtrace, so a production failure degrades to `os error 2`. `fs-err` adds no Windows rename or durability logic of its own: it composes with PLAT-34/PLAT-35, it does not cover them. | `rg -n --type rust --glob '!external/**' '^use std::fs' .` — empty under fs-err, otherwise every `std::fs::` outside the wrapper is a finding | SHOULD |
| PLAT-20 | Never construct a path under a `\\?\` prefix by string manipulation: apply the prefix to an already fully-qualified, backslash-only path and append every component with `join`. The prefix disables all string parsing, so traversal validation must run *before* it is applied, and stripping four characters off a `\\?\UNC\server\share\…` result yields a wrong path, not a legacy one. | `rg -n --type rust --glob '!external/**' '\\\\\?\\' .` — every hit builds its tail via `Path`/`PathBuf` | MUST |

## Filenames and the Extractor

An entry name is attacker-controlled data, not a filename. Reserved device
names are live Win32 aliases; NTFS/APFS resolve a case-variant pair to one file
with no error. Neither is reachable from Linux CI.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PLAT-17 | Every extractor validates each entry name against the reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9`, superscript variants, **and with any extension attached** — `NUL.txt` *is* `NUL`), the reserved characters `< > : " / \ ? *`, the pipe, plus control bytes, and a trailing dot or space — before the join and again after normalization, on the name prefix before the first `.`, at **every** directory level. The `:` ban also closes Alternate Data Streams: `readme.txt:evil:$DATA` is a sanctioned exception to Windows' own character rule and passes any "reasonable" filter. | `rg -n --type rust --glob '!external/**' -e 'fn \w*extract' -e 'fn \w*unpack' .`, then confirm one shared validator runs unconditionally on every entry | MUST |
| PLAT-41 | An extractor rejects — or applies a written last-wins policy to — an entry whose name collides **case-insensitively** with a name already materialized in the same destination directory. Do not rely on the filesystem to report the collision: the second write silently overwrites the first, which for a package manager means one entry's content under another entry's name. | Fixture archive containing `Foo.txt` and `foo.txt`; assert a defined, tested outcome rather than incidental filesystem behaviour | MUST |
| PLAT-11 | Choose lossy vs strict by call-site class, never globally. Display → `to_string_lossy()`. Comparison → stay in `OsStr`/`Path`. On-disk or wire record → `to_str().ok_or(…)?`, schema documented UTF-8-only. Lossy rewrites invalid bytes to U+FFFD: two distinct paths compare equal, and a written record is corrupted irreversibly. | Reading pass on every `to_string_lossy()` hit; a record-class hit is a finding | MUST |
| PLAT-12 | Every deliberate lossy conversion carries a `// LOSSY-OK: <class>` marker, so "was this intentional" is answerable by grep. | `rg -n --type rust --glob '!external/**' 'to_string_lossy\(\)' .` — every hit without a same-line `LOSSY-OK` marker is triage; restrict to added lines on a diff | MUST |

## Canonicalisation and Identity

PLAT-05 and PLAT-06 are two rules, not one. `dunce` is a *string-level
re-spelling* layered on an already-resolved path — not a resolver, not a
containment mechanism — and it is conditional (verbatim `\\?\UNC\…` survives), so
one resolver can hand back root and candidate spelled differently. Compare the
raw canonical output; display the dunce form. **Pinned:** camino is not adopted.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PLAT-05 | Never decide containment or path identity with `==`, a string comparison, or a string prefix. Resolve **both** sides through the same function, then `Path::starts_with`. For a containment or identity *decision* that function is `std::fs::canonicalize` or a handle-based resolution — never a `dunce`-rewritten path and never a raw string. `PartialEq` on `Path` is lexical, a string prefix passes `/base` against `/base-evil`, and raw strings additionally lose to an 8.3 short-name alias and to a trailing dot or space the Win32 layer strips after your check ran. | `rg -n --type rust --glob '!external/**' -e '== Path::new' -e 'starts_with\(&format!' .` — any path comparison without prior canonicalization of both sides; a `starts_with` whose operands came from `dunce::canonicalize` is also a finding | MUST |
| PLAT-06 | Paths that will be **displayed, re-joined, written to a record, or handed to a spawned process** are canonicalized through `dunce::canonicalize`; bare `fs::canonicalize`/`tokio::fs::canonicalize` for one of those uses requires an inline comment naming why a verbatim `\\?\` path is genuinely wanted. A containment comparison is the converse case: it uses the bare canonical output (PLAT-05) and applies dunce afterwards, if at all. | `rg -n --type rust --glob '!external/**' 'fs::canonicalize' .` — every hit is a containment comparison (correct), a `dunce::` call, or carries the comment | MUST |
| PLAT-23 | Case-fold and NFC-normalize at construction any key identifying a package, component, or cache entry that originates from a path or a user-typed name. NTFS and APFS are case-insensitive but case-preserving: `Foo` and `foo` are two map keys and one file. HFS+ additionally stores NFD. ext4 CI reproduces neither. | Reading pass on every `HashMap<String, _>` keyed by package or path identity; normalization happens in the constructor | MUST |

## Windows File Lifecycle

The recipe here is one step *shorter* than the unix one and one longer than the
naive port: flush the file's data, rename, stop. NTFS's `$LogFile` is a
**metadata** journal — the rename can be durably recorded while the file's
buffered bytes are lost, so temp-then-rename alone is no durability story.

Five things are documented gaps, not answers; lean on none of them. There is no
Windows analogue to fsyncing the parent directory. `MOVEFILE_WRITE_THROUGH` on a
same-volume metadata rename is unstated — Microsoft scopes it in writing to
copy-and-delete moves. Same-volume rename atomicity against a concurrent reader
is observed NTFS behaviour, never a contract. ReFS crash-consistency is
unclaimed: no journaling claim, Transactions unavailable, no hardlinks. And 8.3
short-name presence is unknowable in advance.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PLAT-14 | Every rename, replace, or delete on a path another process might hold open retries `ERROR_SHARING_VIOLATION` (32) **and `ERROR_ACCESS_DENIED` (5)** with jittered backoff, through one shared helper. Code 5 is not optional: `CreateFileW` documents that opening a file pending deletion returns 5, not `ERROR_DELETE_PENDING` (303). An indexer, antivirus, or a second instance transiently holds a handle without `FILE_SHARE_DELETE`; this is routine. SQLite's Windows VFS is the reference budget: 10 attempts, +25 ms linear, ~1.4 s total. | `rg -n --type rust --glob '!external/**' -e 'fs::rename\(' -e 'fs::remove_file\(' .` — every hit outside `rename_with_windows_retry` is a finding. Then, because the retry predicate must name **both** codes and one union grep cannot say so: `rg -n --type rust --glob '!external/**' 'ERROR_SHARING_VIOLATION' .` and `rg -n --type rust --glob '!external/**' 'ERROR_ACCESS_DENIED' .` must each return at least one hit | MUST |
| PLAT-15 | Cache and blob replacement writes to a temp name, **flushes the temp file's data (PLAT-34)**, and renames into place, or moves the old entry aside and deletes it later. Never overwrite in place: renaming an open file usually succeeds on Windows where delete and overwrite do not, and that asymmetry is the entire pattern. Temp-then-rename without the flush publishes a path that can hold truncated or absent content after a crash. | `rg -n --type rust --glob '!external/**' -e 'persist\(' -e 'NamedTempFile' .` accounts for every cache-write path, and each syncs the file before persisting | MUST |
| PLAT-34 | On Windows the durable-publish sequence is: write the temp file, **`File::sync_all()` before the rename**, then rename. Never substitute `MOVEFILE_WRITE_THROUGH` for that flush — its documented guarantee is scoped to a copy-and-delete move, and `std::fs::rename` never sets it. `atomicwrites` sets it and still has no Windows data-flush equivalent to the `sync_all` its Unix path performs. | `rg -n -B6 --type rust --glob '!external/**' -e 'persist\(' -e 'fs::rename\(' .` — every publish-rename preceded by `sync_all`/`sync_data` on the temp handle in the same function. A crash-injection test (write N MB unflushed, rename, hard-kill, reboot, hash) is the only real proof | MUST |
| PLAT-35 | The Windows publish-rename is `std::fs::rename`, or an exact reimplementation of its fallback sequence: `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` primary, `FileRenameInfoEx` only as an `ERROR_ACCESS_DENIED` recovery. Do not reach for `ReplaceFileW` — it merges the replaced file's DACLs, compression, object ID and named streams into the replacement, and with a `NULL` backup path error 1176 is documented to leave **the target path with no file at all**. If it is used anyway, `lpBackupFileName` is never `NULL`. `FileRenameInfoEx` needs Windows 10 1607+ and is unsupported by some drivers (FAT32). | `rg -n --type rust --glob '!external/**' -e 'ReplaceFile' -e 'FileRenameInfoEx' -e 'SetFileInformationByHandle' .` — ideally empty; any `ReplaceFile` hit passes a backup path and handles 1175/1176/1177 by name | MUST |
| PLAT-36 | Never call `FlushFileBuffers` on a `FILE_FLAG_BACKUP_SEMANTICS` directory handle as a stand-in for the POSIX parent-directory fsync. The Windows branch flushes the file and stops. No Microsoft source states the call is supported, rejected, or a no-op on a directory; a fabricated analogue is either an unverified no-op or a hang, and reads to the next maintainer as a guarantee nobody obtained. | `rg -n --type rust --glob '!external/**' -e 'FlushFileBuffers' -e 'sync_all' .` inside any `cfg(windows)` block — any invocation against a directory handle cites a primary source, or is deleted | MUST |
| PLAT-16 | Self-update never deletes or overwrites the currently-executing `.exe` — executing a binary takes a read lock, so a direct overwrite fails every time. Exactly two shapes are sanctioned: **serialize** — spawn the new binary, block until the old process has fully exited, then replace the file (rustup's `install_bins()` does `remove_file` then `copy`, gated on `wait_for_parent()`); or **rename-aside** — rename the running image to a same-volume side name and spawn a `FILE_FLAG_DELETE_ON_CLOSE` + `FILE_SHARE_DELETE` helper to remove it (the `self-replace` mechanism, and rustup's uninstall path). `MOVEFILE_DELAY_UNTIL_REBOOT` is never the primary path: it needs admin and its return value reports only that a registry entry was written. | Read the self-update path: the Windows branch serializes on parent exit or renames-then-schedules, never `fs::write`/`remove_file` on a live `current_exe()` | MUST |
| PLAT-19 | Any comment, doc string, or commit message calling a Windows operation "atomic" names the specific API guarantee behind the claim. `ReplaceFileW` is documented as multi-step with three named partial-failure codes. `MoveFileExW`'s page **never uses the word "atomic"** — for any flag, on any code path; same-volume atomicity is universal practice with no Microsoft sentence behind it. | `git diff -U0 -G'[Aa]tomic' -- '*.rs'` — the gate is the change, not the tree: every added comment or doc line calling a file operation atomic names the API guarantee behind it. A `std::sync::atomic` line is not this rule | SHOULD |
| PLAT-37 | Every open of a file the publish or self-update pipeline might later rename or delete out from under a reader specifies `FILE_SHARE_DELETE`; any omission is deliberate and commented. `CreateFileW` states that delete access allows both delete and rename, so a handle held without it blocks every other process's rename *and* delete until it closes — the one instance of PLAT-14's failure we control. cap-std's `Dir::from_std_file` needs the opposite, and that is what the comment is for. | `rg -n --type rust --glob '!external/**' -e 'share_mode' -e 'FILE_SHARE_' .` — every share mode omitting `FILE_SHARE_DELETE` in an install-tree or blob-store read path is a review question | SHOULD |
| PLAT-43 | Windows hardlink placement treats the documented limits as ordinary outcomes with a copy fallback, not errors to propagate: NTFS only (ReFS is a documented `No`, FAT never), files only, same volume only, and a hard cap of **1023 links per file**. A store hardlinking one popular blob into many install trees hits 1023 in normal use, surfacing as an opaque link error on one user's machine. | `rg -n --type rust --glob '!external/**' -e 'hard_link' -e 'CreateHardLink' .` — every call site has a copy fallback and does not treat link failure as fatal | MUST |

## Reparse Points and Containment

Windows containment is a userspace component walk, and that is the ceiling:
there is no `openat2`/`RESOLVE_BENEATH`, and `FILE_FLAG_OPEN_REPARSE_POINT`
governs only the *final* component. cap-std is the maintained implementation of
that walk, not a kernel guarantee — airtightness against every tag is not
certifiable, and payload formats are undocumented, so an unrecognized reparse
point is refused, never parsed.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PLAT-18 | Windows link placement tries hardlink (files, same volume) or junction (directories) first; a true symlink is an opportunistic upgrade behind a capability probe, never the sole implementation — `CreateSymbolicLinkW` needs elevation or Developer Mode, which no stock user machine or hosted runner has, while junctions need neither. Containment code relying on `is_symlink()` states the *correct* limitation: it catches every **name-surrogate** tag and misses the non-surrogate ones. `LX_SYMLINK` and `WCI_LINK` are surrogates and *are* caught; the uncovered set is `APPEXECLINK`, plain `WCI`, `CLOUD*`, `PROJFS`. | `rg -n --type rust --glob '!external/**' -e 'symlink_file' -e 'symlink_dir' -e 'CreateSymbolicLink' .` — every call site has a documented fallback, not a bare `?`. Grep the codebase's own doc comments for the stale `LX_SYMLINK, APPEXECLINK, WCI` phrasing and correct it | MUST |
| PLAT-38 | Classify reparse points by the name-surrogate bit (`tag & 0x2000_0000`), never by matching a list of known tags — the bit test is what `std::fs` does and it fails safe for every future Microsoft surrogate tag. Separately, before writing to a destination, check `FILE_ATTRIBUTE_REPARSE_POINT` on that path **and every ancestor**: a non-surrogate reparse point cannot redirect resolution but is not a plain file either, and writing through a cloud placeholder or a ProjFS virtual file triggers a provider round-trip. | `rg -n --type rust --glob '!external/**' -e 'IO_REPARSE_TAG' -e 'reparse' .` — any tag allowlist is a finding; discard hits outside the module the change touches. The shape is a conjunction, so check it as two: `rg -n --type rust --glob '!external/**' '0x2000_0000' .` and `rg -n --type rust --glob '!external/**' 'FILE_ATTRIBUTE_REPARSE_POINT' .` must each return at least one hit. Unit-test the classification against the full tag table | MUST |
| PLAT-39 | Never call `read_link()` unconditionally after `is_symlink()` returns true on Windows. Handle "Unsupported reparse point type" as refuse-to-follow, not an unreachable state: `readlink` decodes only `SYMLINK` and `MOUNT_POINT` payloads, so a WSL-created `LX_SYMLINK` reports as a symlink and refuses to say where it points. | `rg -n -B3 --type rust --glob '!external/**' 'read_link\(' .` — discard hits outside the module the change touches; in what is left, every hit preceded by an `is_symlink()` branch matches the `Err` arm rather than `?`-propagating into code that assumes a target was obtained | MUST |
| PLAT-40 | Containment resolution for registry-supplied entries on Windows walks the path component by component against open directory handles, re-verifying at every segment; use `cap-std` rather than hand-rolling it. A single up-front canonicalize-then-compare is not a containment mechanism there. Every tar/zip extraction CVE is one shape: an entry validated once, then written through something an *earlier entry in the same archive* placed on disk. | `rg -n --type rust --glob '!external/**' -e 'fn \w*extract' -e 'fn \w*unpack' .` — the extraction function takes a `cap_std::fs::Dir` and never constructs an absolute `PathBuf` from an entry name; `rg -n --type rust --glob '!external/**' -e 'std::fs::File::open' -e 'std::fs::File::create' .` — discard every hit outside the extraction module, and inside it there must be none | MUST |
| PLAT-42 | For an archive `hardlink` or `symlink` entry, apply the full containment and reparse check to the link's **source** path, not only to the new name being created. `CreateHardLinkW` documents that hardlinking a path which is itself a symlink creates a hardlink *to the symlink* — a second reparse-carrying name no is-the-new-name-a-symlink check ever sees. "It passed containment when it was created" is not evidence it still does. | Fixture: entry N creates a link at `root/link` pointing outside `root`; entry N+1 names `root/link` as a hardlink source. Assert refusal | MUST |

## Time and Clocks

`Instant` measures, `SystemTime` records. A backwards wall clock is routine —
NTP, VM live migration, a container booting wrong — so a `duration_since`
failure means "cannot prove freshness", never "panic". **Pinned:** one datetime
crate, `chrono`, and RFC 3339 with a literal `Z` on every persisted timestamp,
which renders chrono's offset-only serde inert.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PLAT-27 | `Instant` for every elapsed-time, TTL, timeout, backoff and rate-limit decision. `SystemTime` only for values persisted or crossed a process boundary — `SystemTime` is not monotonic, and the stdlib documents that two sequential writes can read back out of order. | `rg -n -A3 --type rust --glob '!external/**' 'SystemTime::now\(\)' .` — no surrounding code computes an elapsed duration for a TTL or timeout | MUST |
| PLAT-28 | Never `.unwrap()`/`.expect()` a `SystemTime::duration_since` or `elapsed`. Match the `Err` and treat it as "cannot prove freshness" → stale, logged, non-fatal. | `rg -nU --type rust --glob '!external/**' -e 'duration_since\([^)]*\)\s*\.unwrap' -e 'duration_since\([^)]*\)\s*\.expect' -e 'elapsed\(\)\s*\.unwrap' -e 'elapsed\(\)\s*\.expect' .` — `-U` with `\s*` rather than `.*`, because the chain is usually broken across lines and `.` stops at the newline; ocx has four such sites — every non-test hit is a panic-on-clock-step bug | MUST |
| PLAT-29 | Filesystem mtime is never the sole gate for staleness. Use the content digest, a monotonic generation counter, or an explicit cache-entry record written atomically alongside the artifact. mtime is permitted only paired with a stronger check, or as a throttle window where being wrong costs one redundant operation. FAT buckets write time into 2 seconds, NTFS delays access-time writeback by up to an hour, and extraction resets mtime to *now* — the exact inverse of "unchanged". | `rg -n -B3 -A3 --type rust --glob '!external/**' '\.modified\(\)' .` — every production hit paired with a digest/counter check, or an explicit throttle | MUST |
| PLAT-30 | Exactly one datetime crate in the graph. Two crates modelling "instant in time" emit two serde representations of the same lockfile field; a binary linking one and reading a file written by the other either fails to parse or silently misinterprets. Adding `time` or `jiff` alongside chrono is a `cargo deny` bans finding, not a style nit. | `cargo tree -e normal -i chrono` resolves; `cargo tree -e normal -i jiff` and `cargo tree -e normal -i time` each print nothing — exactly one family | MUST |
| PLAT-31 | Every persisted timestamp is RFC 3339 with an explicit `Z`. Never a local offset, never an unzoned string, never a bare epoch integer in a file a human will `cat` or a tool will diff — an epoch integer is opaque in a stale-cache report and discards sub-second precision. | Grep the golden fixtures for the `Z` suffix on every timestamp field; each field pins its format via `#[serde(with = …)]`, not the derive default | MUST |
| PLAT-32 | Registry-supplied time values are untrusted readings of a remote clock, never a synchronization source. Never diff `Date`/`Last-Modified` against local `now()`; measure `max-age` and `Retry-After` against an `Instant` taken when the response arrived, and prefer `ETag` + `If-None-Match` when both are sent. RFC 9111 §4.2 is normative that clock skew must not corrupt freshness math. | `rg -n -A5 --type rust --glob '!external/**' -e 'Last-Modified' -e 'max-age' -e 'Retry-After' .` — the value feeds only relative or validator logic | MUST |

## Platform Divergence and CI

Linux CI cannot see any of this. Sharing violations, case collisions, reserved
device names, symlink privilege, Gatekeeper, NFD drift — not under-tested on
Linux, structurally invisible to it. A `windows-latest` `cargo check` buys none.

| ID | Rule | Verification | Severity |
|---|---|---|---|
| PLAT-21 | The launcher for a downloaded or cached executable on Windows is the job-object shim, never a plain symlink and never a bare `CreateProcess` — `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is the only mechanism giving the POSIX process-group guarantee that the child dies with the launcher. | `rg -n --type rust --glob '!external/**' 'CreateProcessW' .` — accompanied by `CreateJobObjectW`/`AssignProcessToJobObject` | MUST |
| PLAT-22 | The `com.apple.quarantine` posture is one explicit, documented decision in one module — apply, strip, or never touch — stated in the security docs. A Rust client writing bytes with `fs::write` never sets the xattr, so silence produces "never quarantines" that looks identical to a deliberate choice. | `rg -n --type rust --glob '!external/**' -e 'quarantine' -e 'xattr' .` — **zero hits is itself the finding to raise**, not evidence the topic was settled | MUST |
| PLAT-24 | Cache and config directories come from one platform-conventions module (`directories` or `etcetera`, chosen once), never a per-call-site `cfg(target_os)` branch or a raw `HOME`/`APPDATA` lookup. XDG vs native, and SIP-exempt vs SIP-adjacent, are project-wide policy, not per-call-site accidents. | `rg -n --type rust --glob '!external/**' -e '"HOME"' -e '"USERPROFILE"' -e '"APPDATA"' -e '"LOCALAPPDATA"' -e 'dirs::home_dir' -e 'Library/Caches' .` — the quotes keep `OCX_HOME`/`GRIM_HOME` out and still catch a hand-rolled `cfg!(windows)` USERPROFILE-else-HOME branch; any hit outside the conventions module, or not routed through `directories::`/`etcetera::`, is a finding | SHOULD |
| PLAT-25 | New platform divergence in file identity and lifecycle — replace, link, lock-error classification, executable resolution — goes behind a named platform module exposing outcome-shaped functions (`replace_file`, `link_blob`, `is_locked_err`). Call sites branch on the outcome, not on `cfg(windows)`: the reasoning is identical across platforms even though the syscalls are not. | `git diff -U0 -G 'cfg\(windows' -- '*.rs'`, and again with `-G 'cfg\(target_os'` — a new hit outside the platform module in a file-lifecycle path is a review question | SHOULD |
| PLAT-26 | CI **runs** — not merely compiles — the cache-replace, archive-extraction, link-placement and self-update paths on Windows and macOS runners. The Windows fixture set is: a case-variant filename pair, a reserved-device-name entry with an extension, an entry name containing a colon, a locked-file contention scenario, **a junction swapped under an already-validated ancestor mid-extraction, and a hardlink fan-out past 1023 links**. Junctions need no privilege, so "reparse containment can't be tested without admin" is false. Symlink-creation fixtures sit behind a startup capability probe reporting *skipped-with-reason*, never a bare `#[ignore]`. | Read the CI matrix: a `windows-latest`/`macos-latest` job running only `cargo check`/`build` does not satisfy this. Confirm the probe exists and its skip is visible in the log | MUST |

## What Agents Get Wrong Here

1. **`dest.join(entry_name)` for an untrusted name** — the cross-language
   "join just concatenates" model. PLAT-01's grep on every extraction, install
   or cache-write diff is the highest-value check in this file.
2. **Ports "fsync the parent directory" to Windows** as `FlushFileBuffers` on a
   directory handle: one obvious step, it compiles, it is never questioned
   again, and the correct Windows branch is *shorter*. PLAT-36.
3. **"Makes it atomic on Windows too" by swapping the API name.** That drops the
   durability half — Windows needs an *added* flush with no unix line to copy
   from, because unix's `fsync(file)` was already there. PLAT-34.
4. **Narrows a reparse check to `match tag { SYMLINK | MOUNT_POINT }`.** Reads as
   a tightening; stops catching `WCI_LINK`, `GLOBAL_REPARSE`, `PROJFS_TOMBSTONE`
   and every future surrogate tag. PLAT-38.
5. **`to_string_lossy()` as the friction-free `Path` → `String`** — the option
   with no `Result`. Check the containing function for `Serialize`, `fs::write`
   or a network client on the same value.
6. **Treats a Windows rename failure like a unix one** — log and abort. Error 32
   or 5 from Defender or an indexer is routine and transient, and the flake gets
   misdiagnosed as a race in our own code. PLAT-14.
7. **`SystemTime::now()` to measure elapsed time, `duration_since(x).unwrap()`,
   `format!("{}/{}", dir.display(), name)`, and `.exists()` immediately before
   the act.** All four pass a manual test on Linux and mis-decide elsewhere.
8. **Bare `fs::canonicalize` on "make this cross-platform"**, dunce on a
   containment compare, a Windows symlink call with no fallback beyond `?`, and
   "atomic" in a doc comment with no API guarantee behind it.

## Sources

- [rust-lang/rust#16507](https://github.com/rust-lang/rust/issues/16507) — `join` declined as a security boundary, WONTFIX, maintainers' words
- [Naming Files, Paths, and Namespaces](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file) — `\\?\`, reserved device names with any extension, forbidden characters, trailing dot/space
- [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw) — never says "atomic"; scopes `MOVEFILE_WRITE_THROUGH` to copy-and-delete; documents the delay-until-reboot registry mechanism
- [ReplaceFileW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-replacefilew) / [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers) — multi-step with 1175/1176/1177; file and volume scopes only, directories never mentioned
- [CreateFileW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew) / [CreateHardLinkW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createhardlinkw) — `FILE_SHARE_DELETE`, delete-pending → error 5; NTFS-only, 1023-link cap, hardlink-to-symlink
- [\[MS-FSCC\] Reparse Tags](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/c8e77b37-3909-4fe6-a4ea-2b9d423b1ee4) / [`std/src/sys/fs/windows.rs`](https://github.com/rust-lang/rust/blob/main/library/std/src/sys/fs/windows.rs) / [cap-std](https://github.com/bytecodealliance/cap-std) — the name-surrogate bit per tag, what `std` tests and decodes, and the Windows component walk with its "not a sandbox" caveat
- [rustup#4181](https://github.com/rust-lang/rustup/issues/4181) / [sqlite `os_win.c`](https://github.com/sqlite/sqlite/blob/master/src/os_win.c) — sharing violations as a live 2025 production event, and the best-tested public retry schedule
- [SystemTime](https://doc.rust-lang.org/std/time/struct.SystemTime.html) / [Instant](https://doc.rust-lang.org/std/time/struct.Instant.html) — non-monotonicity, fallible `duration_since`, saturating arithmetic
