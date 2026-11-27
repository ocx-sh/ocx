# LA step 8: launcher security audit (§ 5, § 3.4 as implemented)

Scope: `crates/ocx_env_apply/src/launch.rs`, `crates/ocx_shim/src/{core,direct,main}.rs` at `f5408ef7`
(LA commits `21117206..f5408ef7`), plus the fixes below. Method: `security-auditor` skill (surface map,
STRIDE per read site and spawn site, data trace from environment and record to argv), `quality-security.md`.
Line numbers are at the audit head (after the fix commits).

Fix commits on `hex/bazel-rbe-rules-ocx--LA`:

| Commit | Finding | Tier |
|---|---|---|
| `46d8c41a` | F1 Windows driveless program and manifest root | Warn |
| `a6aa58d7` | F2 home launcher fails on a missing working directory | Warn |
| `c0049cc9` | F3 overflowing depth counted as 0 | Suggest (fixed, trivial) |
| `fb429893` | F4 control characters in refusal lines | Suggest (fixed, trivial) |

No Block finding.

## 1. CWE-427: uncontrolled search path

Checked:

- Windows spawn: `spawn_child` (`main.rs:339`) passes `lpApplicationName` on every call
  (`main.rs:507`, retry at `:548`); both callers pass `Some(program)`: home `run_home` (`main.rs:266`),
  direct `run_windows` (`direct.rs:1266`). No NULL-application rung remains. `lpCurrentDirectory` is
  NULL (inherited), `bInheritHandles` is limited by `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
- DLL search: `harden_dll_search_path` (`main.rs:217`) sets `LOAD_LIBRARY_SEARCH_SYSTEM32` first in
  `main`. It covers run-time loads only; static imports resolve before `main`. The committed (pre-LA)
  x86_64 blob imports `KERNEL32`, `ntdll` and `api-ms-win-*` API sets only (`objdump -p`). LA adds
  `psapi.dll` (`GetMappedFileNameW`, windows-sys `ProcessStatus`), which is a KnownDLL, so a planted
  copy is not loaded. See S1.
- Unix exec: `exec_program` (`direct.rs:1187`) is `Command::exec` of a program that always holds `/`
  (home: an absolute rung; direct: `@VENDOR_DIR@/…` expanded with an absolute root), so std does
  `execve` and never searches `PATH`.
- PATH walk `find_on_path` (`core.rs:275`): empty and relative entries skipped, own directory compared
  folded (`comparable_directory`), candidate `<entry>/ocx` or `<entry>\ocx.exe` only (no `PATHEXT`),
  a sibling regular `ocx.launch` skips the candidate. The own-directory compare is lexical, so `.`,
  `..`, 8.3 names or a symlinked entry defeat it; the sibling-record check is the second guard and the
  depth guard the third (V-L13).
- Batch refusal: `resolve_program` (`core.rs:239`) applies `is_batch_target` (`launch.rs:361`,
  trailing dots and spaces trimmed, ASCII case folded) to every rung's result. The record decoder
  refuses a batch `ocx` too (`launch.rs` `ToolchainRecord::validate`).
- `OCX_BINARY_PIN`: empty counts as unset (`core.rs:239`), non-Unicode is 78 (`direct.rs:1094`).
- Direct batch path (C-017): `run_windows` sends a `.bat`/`.cmd` program to `spawn_batch`
  (`direct.rs:1362`, std's batch escaping, line breaks refused with 64), after `batch_program_refusal`
  (`direct.rs:1172`) refuses a `%` in the script path. A direct `program` cannot end in a dot or space
  or hold `:` on Windows (`relative_path_ok` → `check_segment`, `launch.rs:508`), so `tool.cmd.` and
  `tool.cmd::$DATA` never reach the spawn.

Finding F1 (Warn, CWE-427), fixed in `46d8c41a`. "Absolute" was `is_absolute_any_host`, which takes a
leading `/` on every host. On Windows `/tools/ocx.exe` and `\tools` name a file on the drive of the
working directory, so a driveless `OCX_BINARY_PIN`, record `ocx` or `PATH` entry, and an entry of
separators alone (`\\`, `/`, trimmed to an empty directory, giving `\ocx.exe`), followed the caller's
current drive. The same held for a driveless runfiles-manifest root. New `is_fully_qualified`
(`direct.rs:264`, drive letter plus separator, or a two-separator prefix) now gates the pin, the
record `ocx`, each PATH entry and its candidate (`core.rs:239`, `:275`) and the manifest root
(`direct.rs:467`). Red: `core::tests::a_windows_program_on_the_current_drive_is_refused_on_every_rung`
panicked at `core.rs:435` ("the resolution is refused"; the pin was accepted) and
`direct::tests::a_windows_manifest_root_on_the_current_drive_is_rejected` at `direct.rs:1514` ("the
lookup must refuse"). Control `control_a_windows_drive_or_unc_program_is_accepted` (drive, `/` drive,
UNC, `\\?\`) green before and after. `absolutize` keeps the lenient check: it only joins paths that
come from `GetModuleFileNameW`, a runfiles tree or a checked manifest root.

Residue: the record decoder still accepts a driveless `package_root`, `project_root` and `ocx` on any
host (`check_absolute_path`, `launch.rs:312`); the launcher now refuses a driveless `ocx` at start, the
roots are passed to `ocx` as data. Reported, not changed: the writer lives outside LA's set.

## 2. Manifest parsing bounds

Checked against § 5 step 6 and § 9:

- 64 MiB file, 64 KiB line: `bounded_lines` (`direct.rs:192`) caps a line while it is read (no buffer
  past `max_line` for a file without a line feed) and the byte count including line feeds; first error
  ends the stream. `manifest_refusal` maps both to the frozen 78 lines.
- NUL anywhere in a line → 78 before the UTF-8 skip (`scan_manifest`, `direct.rs:501`).
- Escaping (`split_manifest_line`): split at the first space; leading space marks an escaped line; key
  `\s \n \b`, target `\n \b`, other `\x` two characters; unescaped line verbatim. Matches rules_cc.
- Candidate match needs both key and target to end with `/<self>.launch`, folded on Windows, target
  `\`→`/` on Windows (`manifest_root_bytes`).
- Dedupe before the 4096 cap: `RootSet::push` inserts into `seen` only while under the cap, file order.
- 16 MiB of candidate records: checked before each read (`direct.rs:723`); each read is itself at most
  1 MiB, so the total stays under 17 MiB.
- Root hygiene `checked_manifest_root` (`direct.rs:467`): UTF-8, fully qualified (F1), no NUL, CR, LF,
  list separator, `.` or `..` segment. `\\.\` device roots are refused by the `.` segment rule.
- Windows: a line that is not UTF-8 is skipped; the root is decoded once with `str::from_utf8`.
- `$RUNFILES_MANIFEST_FILE` contributes roots only when one of its targets equals `P` (folded on
  Windows); `RUNFILES_MANIFEST_ONLY` is read nowhere (grep over `crates/ocx_shim/src`: no hit).
- Trust anchor: every candidate is accepted only when `<C>/<self>.launch` is byte-identical to the
  record beside `P` (`accepts`, stages 0-2; the manifest loop compares the same bytes).

Verdict: conforms. Suggest S2 (ordering) below.

## 3. FIFO and non-regular files

Read sites: record beside `I` and beside `P` (`read_record`), candidate records (`candidate_record`),
manifests (`read_lines`). All go through `open_regular` (`direct.rs:132`): `O_NONBLOCK` on Unix,
`fstat` on the opened handle, regular file only, then a bounded read (`read_regular_bounded`, size
checked on the handle and re-checked with a one-byte-over `take`). No path is checked and then
reopened, so a swap after the open changes nothing. FIFO tests: `v60_a_fifo_at_the_sidecar_path_is_refused_without_blocking`,
`vl44_a_fifo_at_a_manifest_path_does_not_hang` (green).

Two path-based checks remain outside the read sites, both by design: the PATH walk's `is_program_file`
/ `is_regular_file` (`RealFs`, `direct.rs` `ProgramFs` impl), whose result is then passed to `execve` /
`CreateProcessW` by path, and the direct kind's `stat(P)` identity check (`is_running_image`), which
§ 5 step 4 keeps for kind `direct` only. A swap between check and spawn starts what the attacker who
can write that directory could start anyway.

Windows: no FIFO exists in the file namespace; `\\.\pipe\` and device roots cannot be reached from a
record or tree path (record names come from the image, manifest roots lose `\\.\` to the dot rule). See
S3 for the device-handle residue.

## 4. Depth guard

`home_environment` (`core.rs:178`) reads `__OCX_LAUNCHER_DEPTH` (case folded on Windows), refuses
depth ≥ 32 with 78 "launcher loop" before containment-independent program resolution (`plan_home`
calls it first, `direct.rs:1094`), removes every spelling of the key and appends `depth + 1`. Applied to
all three home kinds (one call site); `depth + 1` cannot overflow because depth < 32 there.

A crafted environment can only lower the count (set `0`, junk, or a negative), which is no bypass
beyond what the environment's owner can do directly: every launcher hop rewrites the value, so a loop
between launchers reaches 32 whatever the start value.

Finding F3 (Suggest, contract conformance), fixed in `c0049cc9`. Digits past the `u32` range parsed as
"not a number" and counted as 0, although the contract stops every depth of 32 or more. Overflow now
counts as the limit. Red: `core::tests::vl14_a_depth_past_the_counter_range_is_a_launcher_loop`
panicked at `core.rs:795` ("a huge depth is a loop"). The case `"99999999999999999999"` moved out of
`vl07_an_unparsable_depth_counts_as_zero`, whose other inputs (`""`, `x`, `-1`, `1.5`) still count as 0.

Residue (outside LA's set): the guard relies on every `ocx` spawn passing the variable on unchanged
(`ocx_env` registers it `propagation = UserInput`, `crates/ocx_env/src/vars.rs:548`). Whether
`ocx exec --clean` or any projection that drops the caller environment also drops it was not checked;
if one does, a loop through such a spawn restarts at 0.

## 5. Environment trust

Variables the launcher reads: `PATH`, `OCX_BINARY_PIN`, `OCX_HOME` (containment, home kinds),
`__OCX_LAUNCHER_DEPTH`, `RUNFILES_DIR`, `RUNFILES_MANIFEST_FILE`; `OCX_GLOBAL`/`OCX_PROJECT` are only
removed. `vars_os` is passed through otherwise (`SecretInput` variables reach the child, C-014).

- Strip: `strips_tier_selectors` (`core.rs:131`) is true for `toolchain` only; folded on Windows. Tests
  `vl07_only_a_toolchain_launch_removes_the_tier_selectors`, `vl07_the_selector_strip_folds_case_on_windows_only`.
- Record keys (`DirectRecord::validate`, `launch.rs` around `:445`): empty, `=` or NUL refused in
  `unset` and `env`; `env` keys `OCX_BINARY_PIN`, `__OCX_*` (folded on Windows), or colliding with an
  `unset` key (folded on Windows) refused. The NUL scan covers every string of every kind.
- `@VENDOR_DIR@`: single-pass `str::replace` (`expand`), no recursion; the root carries no list
  separator (`bind_root`, `direct.rs:635`); on Windows `/`→`\` in root, `program` and `path` values.
- Argv: home tokens are frozen, the record value is one token, an absolute root or `project_root`
  cannot start with `-`, a deferred identifier may not start with `-`, and `--` precedes `NAME` and the
  caller's arguments. Windows quotes every token with the `CommandLineToArgvW` quoter
  (`append_quoted_arg`, `core.rs:344`, control bytes force quotes); non-Unicode caller arguments are 64.
- Windows environment block (`environment_block`, `direct.rs:1322`): `KEY=VALUE\0` … `\0`; record keys
  hold no `=` or NUL; inherited `=C:` style keys keep their leading `=`.

Finding F2 (Warn, availability), fixed in `a6aa58d7`. `run()` on Unix and `run_windows` read the working
directory with `?` before the record decided the kind, so every launcher, home kinds included, exited 74
"cannot determine own path" from a deleted, unreadable or non-Unicode directory. `locate_with`'s own
contract says a started-path failure never reaches a home launcher. Now `working_directory()`
(`direct.rs:1216`) never fails, and only the direct started-path closure refuses an empty one. Red:
`tests/direct_launch.rs::a_home_launcher_starts_from_a_deleted_working_directory` failed with
`ocx-shim: cannot determine own path` (shown again against `HEAD:direct.rs` after hardening the test for
`ETXTBSY`). Unit guard `direct::tests::without_a_working_directory_only_a_direct_launcher_fails` went red
(`direct.rs:1914`) with the guard deleted, green with it.

Finding F4 (Suggest, CWE-150), fixed in `fb429893`. `DirectRefused` and `DirectArgumentRefused` lines
printed paths from `argv[0]`, the working directory, runfiles entries and manifest roots raw, while the
spawn-failure lines already escaped control characters. Red:
`tests::a_refusal_reaches_stderr_with_its_control_characters_escaped` failed at `main.rs:766`. Fix:
`escape_controls(reason)` (`main.rs:113`).

## 6. Image path

- Linux: `read_link("/proc/self/exe")`, ` (deleted)` → 78 (`checked_image_link`, `direct.rs:862`,
  test `vl18_…`). A file whose real name ends in ` (deleted)` is refused too (fail closed).
- macOS: `proc_pidpath` into `PROC_PIDPATHINFO_MAXSIZE`; `<= 0` is a failure; the kernel never writes
  past the size passed, so no truncation is silently accepted beyond the platform maximum.
  `is_running_image` uses `current_exe` = `_NSGetExecutablePath`, the path given to `execve`, not
  `argv[0]`, so the direct identity check still detects a spoofed `argv[0]`.
- Windows (`mapped_image_path`, `direct.rs:962`): `GetMappedFileNameW` grown while `written == capacity`
  up to 64K units; `\\?\GLOBALROOT` + NT path opened with `FILE_READ_ATTRIBUTES`, full sharing,
  `FILE_FLAG_BACKUP_SEMANTICS`; `GetFinalPathNameByHandleW(FILE_NAME_NORMALIZED | VOLUME_NAME_DOS)`
  retried with the returned size; the handle is closed on every path; `dunce::simplified` strips
  `\\?\` and `\\?\UNC\` where an ordinary path names the same file. Not executed here (no Windows host);
  V-L19 stays a claim for `basic-repo.yml › unit-native (windows-latest)`.
- No stat-then-open compare for home kinds: `locate_with` (`direct.rs:653`) reads the record beside `I`
  and returns before `started()` or `is_running_image` runs (tests `vl17_…`).

Earlier-step notes, judged:

- Linux/macOS image-path failure exits 74 (`SelfPathFailure`), Windows 78. The contract names only the
  ` (deleted)` case. More important than the code: the image path is read before the kind is known, so
  a host without `/proc` (a chroot) now fails kind `direct` too, which needs only `P`. Reported as a
  contract question (R1), not changed: picking a fallback is a design decision.
- A malformed record beside `I` is 78 even when it is kind `direct`: fail closed, kept.
- `RecordError::Malformed` for CR/LF and a batch `ocx`: the exit code and the message are right;
  a typed variant would change a contract-wave pub enum. Suggest S5, not changed.
- `__OCX_LAUNCHER_DEPTH` literal in `ocx_shim`: the name matches the `ocx_env` row; nothing binds them.
  Suggest S6.

## Suggest findings not fixed

- S1. Import `K32GetMappedFileNameW` from `kernel32.dll` instead of `GetMappedFileNameW` from
  `psapi.dll` (`direct.rs:962`): one import fewer before `main` can harden the search path. Not a
  vulnerability (KnownDLL); not done because the import table of a rebuilt blob cannot be checked here.
- S2. `manifest_roots` (`direct.rs:553`) reads all three sources before testing a candidate, so an
  oversized or NUL-bearing inherited `$RUNFILES_MANIFEST_FILE` turns a launcher that sources (i)/(ii)
  would root into a 78. Testing (i)/(ii) candidates first would keep a benign inherited variable from
  failing the launch.
- S3. Windows `open_regular` relies on std's `is_file()` (not a directory, not a symlink). A device
  handle that reports file attributes would pass and could block in `read`. A `GetFileType ==
  FILE_TYPE_DISK` check on the handle closes it; no record or manifest path an attacker controls
  without already owning the launcher directory or the environment reaches a device.
- S4. `O_NOCTTY` beside `O_NONBLOCK` in `open_regular` (`direct.rs:139`): opening a terminal device at a
  record path can make it the controlling terminal of a session leader. Needs write access to the
  launcher directory.
- S5. Typed `RecordError` variants for CR/LF in a path field and a batch `ocx` (contract-wave enum).
- S6. A test binding `core::LAUNCHER_DEPTH_VAR` to the `ocx_env` row (outside LA's set, e.g. a lint in
  `test/lint/` reading both sources).
- S7. A `list` env record with a custom separator is not checked against the root (`bind_root` checks
  the OS list separator only); a vendor directory whose path holds that separator splits into extra
  list items.
- S8. The started-path walk for a bare `argv[0]` (`started_path_unix`, `direct.rs:584`) joins empty and
  relative `PATH` entries onto the working directory. The device and inode check binds `P` to the
  running image, and the shell that started the launcher searched the same `PATH`, so no new exposure.

## Reported out of set

- R1. Image-path failure blocks kind `direct` on a host without `/proc` (Linux 74) and on a volume
  without a DOS name (Windows 78). Decision for the ADR: fall back to "no record beside `I`" (direct
  rules) when the image path cannot be read, or keep failing.
- R2. `crates/ocx_env_apply/src/launch.rs:312` `check_absolute_path`: a driveless `package_root`,
  `project_root` or `ocx` decodes on Windows. Proposed: the writer refuses it (65) on Windows hosts.
- R3. `crates/ocx_env/src/vars.rs:548` `__OCX_LAUNCHER_DEPTH`: verify every `ocx` child projection,
  `--clean` included, passes it on (see § 4 residue).

## Verification

- `cargo test -p ocx_shim -p ocx_env_apply --locked`: `ocx_env_apply` 77 passed; `ocx_shim` unit 158
  passed (155 at audit start + 3 new; 1 case moved); `tests/direct_launch.rs` 18 passed, 1 ignored
  (`v66` overhead); `batch_spawn` 0 on Linux; doctest 1. Baseline before the audit: 77 / 152 / 17+1.
- `cargo clippy -p ocx_shim -p ocx_env_apply --all-targets --locked -- -D warnings`: clean on the host
  and on `x86_64-pc-windows-gnullvm`, `aarch64-pc-windows-gnullvm`, `aarch64-apple-darwin`,
  `x86_64-apple-darwin` (check only; nothing linked or run for those targets).
- `cargo fmt -p ocx_shim -p ocx_env_apply --check`: clean.
- Not run: `task verify:scoped` (`scripts/scoped_gate.py --plan` escalates: the branch diff against
  `origin/main` touches workflows and hub crates, and routes a Bazel lane, rc-file rule); Bazel; Windows
  and macOS execution of any test (no host); the committed launcher blobs (act B rebuilds them).
