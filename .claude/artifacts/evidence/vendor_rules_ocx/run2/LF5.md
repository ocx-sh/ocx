# LF5 evidence (test-only fixes; Windows: not run, CI only for every item)

Compiled here: `cargo check -p ocx_shim --tests --target x86_64-pc-windows-gnullvm` green (the edited test compiles for Windows).
`cargo check -p ocx_package_manager --tests --target x86_64-pc-windows-gnullvm` blocked by a C dependency build: not compiled for Windows.
Ran: `cargo test -p ocx_package_manager --lib launcher::` 65 passed, 0 failed (Linux), after the change.

## 1 junction test (commit 82d01ed9)
- Cause: the invocation path through the single junction was still past MAX_PATH, so `CreateProcessW` failed with code 3 before the launcher ran.
- `CreateProcessW` now receives `base\b\tool.exe`, under 200 units (asserted); the typed `mklink` targets are under 250 (asserted).
- The mapped image name is the real path: the loader records the file object's final name, with every junction on the way resolved, so the launcher sees `base\root\<6 segments>\tool.exe` (over 260, asserted).
- The launcher takes the vendor root from the started path minus `self` (`own_root`) and accepts it only if the record there is byte-identical to the record beside the image; the root is `base\b`, a link, so `self` is relative to it: `tool`.
- Cleanup: both links removed with `remove_dir` before `remove_dir_all`, so nothing is deleted through a link.

## 2 batch past MAX_PATH (same commit)
- Premise "never runs" is a host property. Test renamed `a_batch_target_past_max_path_runs_with_its_argument_or_names_the_length_limit`: success asserts `ARG1=hello`; failure asserts the `ocx-shim: ` line with the length clause. Exact text: host unit `a_batch_start_failure_line_names_the_program_the_exit_and_the_length_limit` in `crates/ocx_shim/src/direct.rs` (exists).

## 3 unix target test (commit 2a93e1fb)
- `is_fully_qualified(root, windows=false)` wants a POSIX-absolute root; the product check is untouched. The test now passes `/ocx/pkg` (as the Windows sibling `a_windows_target_lists_only_exe_and_launch_per_name` passes `C:/ocx/pkg`), so it runs on every host.
- Same file, same mistake: `a_package_root_holding_the_vendor_token_is_refused_before_anything_is_written` passed on Windows for the wrong reason (root not qualified); now targets `cfg!(windows)`, so it refuses on the token.

## 4, 5 selector fixture (commit 682d166d)
- `/opt/ocx/bin/ocx` replaced by `<project_root>/bin/ocx`, absolute on the host; both tests still bake and resolve the project root.
- Other `/…` literals in the three files: `/no/drive` (Windows-target refusal, intended), `/p` (record text), `/definitely/not/here` (an equality probe on a missing path, never validated as absolute), `/nowhere` (inside `#[cfg(unix)]`).
