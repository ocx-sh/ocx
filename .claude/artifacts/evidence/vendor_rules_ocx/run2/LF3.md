# LF3: launcher image path past 260 characters (Windows)

Cause: `GetMappedFileNameW` wraps `NtQueryVirtualMemory` with a `MAX_PATH`-sized buffer of its own (ReactOS psapi.c), so it returns 0 for every caller buffer once the image path passes 260 characters (CI: error 234, then "longer than 64K units").

Fix (commit with subject `fix(launcher): a launcher under a path longer than 260 characters starts on Windows`): `mapped_image_path()` calls `NtQueryVirtualMemory(MemoryMappedFilenameInformation)` (windows-sys `Wdk_Storage_FileSystem`, one feature added; `Cargo.lock` unchanged), grows the `u64`-backed buffer from status and `ReturnLength` (cap 64K units), reads the `UNICODE_STRING` through the safe `section_name_units`. The rest (GLOBALROOT, `CreateFileW`, `GetFinalPathNameByHandleW`) is unchanged. Step name in the stderr line is now `NtQueryVirtualMemory`.

Red: mutated `end > bytes.len()` to `+ 4096` -> `a_section_name_is_read_past_max_path_and_hostile_structures_are_refused` failed; disabled the short-buffer status match -> `a_section_query_grows_on_a_short_buffer_status_and_stops_at_its_limit` failed. Both reverted.
Green (host): `cargo test -p ocx_shim --locked`: 164 + 18 passed, 1 ignored. clippy -D warnings, fmt, code-docs ratchet function: pass.
Windows compile: clippy `--all-targets -D warnings` clean for x86_64-pc-windows-msvc, x86_64-pc-windows-gnullvm, aarch64-pc-windows-gnullvm.
Not run, CI only: the Windows tests, incl. new `a_direct_launcher_started_through_a_junction_finds_its_record_past_max_path`.

## Follow-up: junction test made to prove its case
The first junction test started a launcher under a short path (only the child was long), so it passed with the old `GetMappedFileNameW`. `a_launcher_whose_real_path_is_past_max_path_starts_through_a_junction` now puts the launcher image and its `.launch` record under nested directories (real path asserted over 260 UTF-16 units), starts it through a short `mklink /J` junction (asserted a reparse point), and asserts exit 0, the short batch target's `ARG1=hello` on stdout, and no "cannot resolve the launcher image" on stderr. With the old call it fails on the image lookup. The batch-target test's doc now says it proves only the length-limit line. Comment-stripped comparison of `crates/ocx_shim/src` against `859e0144`: identical. Host tests 164 + 18 pass; clippy clean for host, x86_64-pc-windows-msvc, x86_64-pc-windows-gnullvm, aarch64-pc-windows-gnullvm. Windows execution: not run, CI only.
