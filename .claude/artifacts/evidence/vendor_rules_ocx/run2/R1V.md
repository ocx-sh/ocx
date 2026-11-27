# R1V: red and green for the open verification checks

Branch `hex/bazel-rbe-rules-ocx--R1V`, base `a74b5427`. Linux host only; no Windows or macOS run, no CI run started.

## Method

- `ocx` built once from the clean base: `cargo build -p ocx --features __testing --locked` (debug, 4 jobs, scratch target); the binary is kept as `ocx-base`.
- Per mutation: one text replacement (exactly one match asserted), the replaced line shown by `git diff -U0` (the landed check), a rebuild, the binary copied aside, `git checkout -- <file>`, `git status --short` empty. The mutated acceptance run uses the copied mutated binary; every green run uses `ocx-base`, which was built from the same text the checkout restores. No green binary was rebuilt after a restore.
- Acceptance runs: `cd test && python -m pytest -q -p no:cacheprovider --tb=line tests/<module>.py` with scratch `HOME` and `OCX_HOME`, `OCX_COMMAND` = the binary under test, the suite's own zot registries, no Docker started by this worker. The venv is the one already present under the `uno` checkout (`uv sync` offline failed: a wheel is not in the uv cache).
- `OCX_TEST_PREVIOUS_OCX` = the installed `ocx` 0.6.8 release (read-only use of the binary; the locked `ocx-previous` package would need a registry fetch outside the suite). The branch version is also 0.6.8.
- Unit runs: `cargo test -p <crate> --locked`, scratch target, a filtered scope named per row.

## Rows

### V-L34 (HA2.1, `test_toolchain_activate.py::test_two_ocx_versions_alternate_on_one_home_and_every_launcher_starts`)
- Mutation 1: `render_toolchain.rs` the commit call `switch_active(home, &shell_dir).await;` becomes `switch_active(home, "default").await;`. Result: the check did NOT go red (1 passed). The second render, which takes the stamp-current arm at line 675 and re-switches `active` to the stamp's directory, restores the link: two independent guards.
- Mutation 2: `render_toolchain.rs` inside `switch_active` the tuple `(home.root().to_path_buf(), shell_dir.to_owned())` becomes `(home.root().to_path_buf(), { let _ = shell_dir; "default".to_owned() })`, so both switch sites write `default`. Red: 1 failed, `test_toolchain_activate.py:1547` `S-102 — the new render's active names shells/default-<hex>; it names 'default'`.
- Green: the same test 1 passed; the whole module `test_toolchain_activate.py` 35 passed, 0 failed. Windows deep leg: not shown, needs a Windows host or a deliberately failing CI run.

### V-L42 (TR.4, HA2.2, `test_self_update.py::test_self_update_renders_the_global_toolchain_into_a_new_directory`)
- Mutation: `ocx_setup/src/lib.rs` phase 1.6 `if !options.dry_run` becomes `if options.dry_run`.
- Red: 1 failed, `test_self_update.py:903` `V-L42 -- phase 1.6 publishes one new directory; shells/ holds []`.
- Green: the test 1 passed; the module `test_self_update.py` 28 passed, 0 failed (the test is not skipped here: the self-update seam runs on this host).

### V-L40 (HA1.2, `test_trampoline_exec.py::test_item22_vl40_two_project_homes_on_one_path_exit_65_through_native_launchers`)
- Mutation: `ocx_config/src/env.rs` `is_ocx_trampoline` `|| record_signal(path) ||` becomes `|| (record_signal(path) && false) ||` (a plain `false` body does not compile: dead-code lints).
- Red: 1 failed, `test_trampoline_exec.py:444` `assert 78 == 65`, stderr `ocx-shim: launcher loop: __OCX_LAUNCHER_DEPTH is 32 ...`. The depth counter now stops the A to B to A loop with exit 78, so the check reds as a wrong exit code, not as the timeout its docstring names.
- Green: `test_trampoline_exec.py` 19 passed, 0 failed, 6 skipped.

### V-L50 (VF.3, VF.4, VA.1, `test_vendor.py`, `test_package_vendor.py`, `test_vendor_parity.py`)
- Mutation: `ocx_package_manager/src/tasks/vendor.rs` the record path `Some(checked_path(VendorLayout::relative_record(name), windows)?),` becomes `None,`.
- Red: 11 failed, 61 passed, 1 skipped over the three modules. First failing line of the named cases: `test_vendor.py:303` `KeyError: 'record_path'` (`test_vendor_writes_manifest_content_and_launchers`) and `vendor_fixtures.py:204` (`test_closure_and_entrypoints_land_in_the_short_layout`); also `test_vendor_parity.py:597`.
- Green: 72 passed, 0 failed, 1 skipped.

### V-L20 (HA1.1, `test_entrypoints.py::test_previous_release_install_gains_launchers_dir_at_next_exec_and_keeps_entrypoints`, needs the previous release)
- Mutation: `ocx_package_manager/src/launcher/generate.rs` `first_use` returns `EntryPointDir::Legacy` before publishing when a complete `entrypoints/` exists (3 lines added before the `match publish_after_retiring`).
- Red: 2 failed, 27 passed, 1 skipped. `test_entrypoints.py:1236` `first use must publish exactly one launchers/<G>; found []`, and `:1267` for the concurrent case.
- Green: `test_entrypoints.py` 29 passed, 0 failed, 1 skipped, with `OCX_TEST_PREVIOUS_OCX` set.

### V-L13 (HA1.1, the two `ocx`-named entry point cases)
The launcher is a committed blob, so the mutation is in `crates/ocx_shim/src/core.rs`, built with `cargo build -p ocx_shim --profile shim --target x86_64-unknown-linux-musl`, swapped over `crates/ocx_store/src/shims/ocx-shim-x86_64-unknown-linux-musl`, `ocx` rebuilt, the blob restored by `git checkout`.
- Control (unmutated source, locally built blob swapped in): the two cases 2 passed.
- Mutation A: `find_on_path` `&& !fs.is_regular_file(&record_path_for(&candidate, windows))` becomes `&& !(fs.is_regular_file(&record_path_for(&candidate, windows)) && false)`. Red: 1 failed, 28 passed, 1 skipped; `test_entrypoints.py:1219` `rc=78 ... launcher loop` for `test_two_packages_with_an_ocx_entry_point_on_path_start_the_real_ocx`. The single-package case `test_entry_point_named_ocx_starts_the_real_ocx_without_a_loop` did NOT go red under A: the own-directory skip also guards it.
- Mutation B: A plus `|| comparable_directory(entry, windows) == own` becomes `|| (comparable_directory(entry, windows) == own && false)`. Red: both cases fail, `test_entrypoints.py:1196` and `:1219` (`rc=78 ... launcher loop`).
- Green: `test_entrypoints.py` 29 passed, 0 failed, 1 skipped with the committed blob.

### V-L09 (HA1.3, Windows deep leg; platform-independent part only)
The Windows `.exe` + `.shim`/`.exec` case (`test_an_old_toolchain_trampoline_with_an_exec_sidecar_...`) is skipped off Windows. Platform-independent part: `test_exec_env_compat.py::test_an_old_package_launcher_an_older_ocx_wrote_starts_its_tool_through_this_ocx` (an old `entrypoints/NAME` script still reaches the tool through this `ocx`'s `launcher exec` grammar).
- Mutation: `ocx_cli/src/command/launcher.rs` `Exec(exec::LauncherExec),` gets `#[command(name = "exec-v2")]`.
- Red: `test_exec_env_compat.py` 5 failed, 6 passed, 2 skipped; the named test at `test_exec_env_compat.py:768`.
- Green: 11 passed, 0 failed, 2 skipped.
- Windows half: not shown: needs a Windows host or a deliberately failing CI run. The deep Windows leg result is in no evidence file.

### V-L11 (HA1.3, Windows deep leg; platform-independent part only)
Platform-independent part: the `ocx_shim` PATH walk (`find_on_path`, whose `windows` flag is a runtime parameter) that never starts an `ocx` from an empty or relative entry; `lpApplicationName` on `CreateProcessW` is Windows-only and not covered here.
- Mutation 1: drop `!is_fully_qualified(entry, windows) ||`. The check did NOT go red (175 passed): the later `is_fully_qualified(&candidate, windows)` check also guards it.
- Mutation 2: drop both `is_fully_qualified` checks. Red: `cargo test -p ocx_shim --locked`: 173 passed, 2 failed (`core::tests::vl10_the_walk_skips_empty_relative_own_and_other_launcher_entries` at `core.rs:650`, `core::tests::a_windows_program_on_the_current_drive_is_refused_on_every_rung` at `core.rs:796`).
- Green: 175 passed, 0 failed (plus 21 integration passed, 1 ignored).
- The Windows case `test_launcher_never_starts_a_planted_ocx_exe_in_cwd_or_its_own_directory`: not shown: needs a Windows host or a deliberately failing CI run.

### V-L38 (EP.1, HA1.3; writer half on Linux, Windows start half)
- Mutation: `launcher/generate.rs` `write_launcher` writes an empty `{name}.shim` before the record.
- Red: `test_entrypoints_crossplat.py` 1 failed, 2 passed, 3 skipped; `test_entrypoints_crossplat.py:109` `only the launcher and its record: no .cmd, .exe, .shim or .exec`.
- Green: 3 passed, 0 failed, 3 skipped.
- Windows start half (`test_windows_shim.py`): not shown: needs a Windows host or a deliberately failing CI run; the deep Windows leg is pending in X2.md.

### V-L31 and V-L32 (PB.3, CI2.3; the in-job junction proof)
The proof is two `#[ignore]` `cfg(windows)` tests (`junction_proof_retarget_under_lookup`, `junction_proof_retire_running_exe`) run only by `basic-repo.yml` `junction-proof`. Not runnable here.
- V-L31 platform-independent part: the junction reparse-buffer encoder `mount_point_buffer` (compiled under `cfg(any(windows, test))`). Mutation: `let plain = target.strip_prefix(verbatim.as_slice()).unwrap_or(target);` becomes `... .map(|_| target).unwrap_or(target);`. Red: `cargo test -p ocx_util --locked --lib fs::publish`: 21 passed, 1 failed (`fs::publish::tests::mount_point_buffer_drops_a_verbatim_prefix`). Green: 22 passed, 0 failed.
- V-L32 platform-independent part: `retire_dir` leaves a directory whole when the rename is refused. Mutation: `Err(error) if is_refused(&error) => return Ok(Retired::LeftForLater),` becomes `... Ok(Retired::Removed),`. Red: 21 passed, 1 failed (`fs::publish::tests::retire_leaves_dir_whole_when_rename_is_refused`; the test skips as root, this host runs as a normal user). Green: 22 passed, 0 failed.
- The in-job red (a retarget that fails under lookup, a retire that breaks a running `.exe`): not shown: needs a Windows host or a deliberately failing CI run. The unit parts do not exercise a reader against a retarget, so atomicity under lookup is not covered by them. Green on record: `Junction proof (standard)` and `(runner)` success, run 45973179707 at 04152fdc (X2.md).

### V-L57 (LA.7; CI half)
- Unit part (already recorded in PF.md), rerun: `ocx_store/src/shim.rs` `SHIM_SIZE_BUDGET` 640 KiB becomes 100 KiB. Red: `cargo test -p ocx_store --locked --lib shim::` 19 passed, 2 failed (`shim::tests::blob_for_serves_the_six_launcher_targets`, `shim::tests::every_blob_is_within_the_size_budget`). Green: 21 passed, 0 failed.
- Local replay of the size lines of `build-windows-shims.yml` "Validate shim blobs" (budget read from `shim.rs` by the same `grep`, size by `stat`) against the committed x86_64 Linux launcher: green `496960B <= 655360B`; with the budget at 100 KiB, red `::error::... 496960B exceeds SHIM_SIZE_BUDGET 102400B`. A replay of the step's lines, not the job.
- CI half (`Cross-build` jobs and their budget step): not shown: needs a deliberately failing CI run. On record the six `Cross-build` jobs of run 45973179806 failed at a different step (`Committed blobs must be re-cut when shim sources change`), so the budget step has no CI result.

## Not run
- Any Windows or macOS leg; any CI run; V-L46 to V-L49 (other repository).
- The whole acceptance suite (only the modules named above).

## Tree
Every mutation reverted by `git checkout -- <file>`; `git status --short` empty after each. Scratch about 4.3 GB.
