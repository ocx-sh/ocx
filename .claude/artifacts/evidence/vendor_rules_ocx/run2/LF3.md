# LF3: launcher image path past 260 characters (Windows)

Cause: `GetMappedFileNameW` wraps `NtQueryVirtualMemory` with a `MAX_PATH`-sized buffer of its own (ReactOS psapi.c), so it returns 0 for every caller buffer once the image path passes 260 characters (CI: error 234, then "longer than 64K units").

Fix (commit with subject `fix(launcher): a launcher under a path longer than 260 characters starts on Windows`): `mapped_image_path()` calls `NtQueryVirtualMemory(MemoryMappedFilenameInformation)` (windows-sys `Wdk_Storage_FileSystem`, one feature added; `Cargo.lock` unchanged), grows the `u64`-backed buffer from status and `ReturnLength` (cap 64K units), reads the `UNICODE_STRING` through the safe `section_name_units`. The rest (GLOBALROOT, `CreateFileW`, `GetFinalPathNameByHandleW`) is unchanged. Step name in the stderr line is now `NtQueryVirtualMemory`.

Red: mutated `end > bytes.len()` to `+ 4096` -> `a_section_name_is_read_past_max_path_and_hostile_structures_are_refused` failed; disabled the short-buffer status match -> `a_section_query_grows_on_a_short_buffer_status_and_stops_at_its_limit` failed. Both reverted.
Green (host): `cargo test -p ocx_shim --locked`: 164 + 18 passed, 1 ignored. clippy -D warnings, fmt, code-docs ratchet function: pass.
Windows compile: clippy `--all-targets -D warnings` clean for x86_64-pc-windows-msvc, x86_64-pc-windows-gnullvm, aarch64-pc-windows-gnullvm.
Not run, CI only: the Windows tests, incl. new `a_direct_launcher_started_through_a_junction_finds_its_record_past_max_path`.
