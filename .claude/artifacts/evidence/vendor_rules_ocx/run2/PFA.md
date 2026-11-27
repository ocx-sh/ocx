# PFA — fixes for the CI failures at f58247ee

Build: `cargo build -p ocx --features __testing` (the `__OCX_TESTING_*` seams in `test_execution_records` need it; a plain build fails two cases). Linux only; Windows and macOS not run, CI only.

| Group | Commit | Red (local, before) | Cause | Fix |
|---|---|---|---|---|
| E | 9eb38a16 | unit `kind_is_launcher_when_the_resolved_path_is_under_a_launcher_generation`: `sh.ocx.kind` missing from the block | `executable_kind` matched only `entrypoints` | arm `launchers` added; `entrypoints` kept for a package an older `ocx` installed |
| A | 325f8504 | `current/entrypoints/hello` does not exist (6 tests) | tests read the pre-launcher layout | `install_entrypoint_package` / `only_generation`; the exec_forwarding case reads the `NAME.launch` record instead of a script body |
| A2 | 325f8504 | three hardening cases ran the ELF launcher through `sh` (UnicodeDecodeError on stderr) | tests assumed a shell-script launcher | run the launcher directly; no ocx on PATH exits 78 `no ocx found on PATH` (record C-108), a deleted pin exits 74 naming the path |
| F | 6914e012 | `nested entry under e/: p/<hex>/e/alpha` | `assert_short_layout` rejected depth 4 under `e/`; the system design (§ layout) puts `NAME[.exe]` and `NAME.launch` directly under `e/` | reject depth 5 and deeper |
| B | 3191a499 | remove: trampoline still present; re-add: file missing | both tests computed the render path before the render. A render publishes a new `shells/default-<hex>` and switches `active`; the old directory is never edited (`ocx clean` retires it); before the first render `shell_bin` falls back to `shells/default` | path recomputed after the command. Test-side only; product behaves as the launcher record says |
| C | 8c7bdb29 | `test_windows_amd64_manifest_equals_golden` schema 1 / `packages/...` layout | stale golden | regenerated from a real `ocx vendor -p windows/amd64` run (manifest text through the test's own masking, `p/@HEX16@`) |
| Glossary | 9ca36515 | CI: definition has 35 words | | 23 words |

Item 9 (`toolchain_fixtures.shell_bin` fallback to `shells/default`): reachable (the pre-render precondition in `test_project_add_render`, the older-layout plant at `toolchain_fixtures.py:340`); kept. The local `shell_bin` in `test_trampoline_exec.py` asserts the `default-<16 hex>` spelling and differs on purpose; kept.

## Whole-module runs (scratch HOME and OCX_HOME, own zot, no Docker; `OCX_TEST_PREVIOUS_OCX` = the installed 0.6.8 release)
passed / failed / skipped: test_execution_records 41/0/3, test_execution_record_standards 16/0/0, test_hardening_limits 18/0/0, test_exec_forwarding 36/0/0, test_self_candidate_switch 2/0/0, test_toolchain_cli 35/0/0, test_project_add_render 1/0/0, test_vendor 44/0/0, test_vendor_parity 8/0/1, test_package_vendor 20/0/0, test_clean 13/0/0, test_lazy_loading 33/0/0, test_lazy_direnv 4/0/0, test_patches 132/0/0, test_progress_channel 6/0/0, test_entrypoints 29/0/1, test_launcher_exec 23/0/0, test_toolchain_render 77/0/1, test_trampoline_exec 19/0/6.
`task test:lint:structure`: 808 passed. `cargo test -p ocx_package_manager --lib execution_record`: 58 passed.

Not run: Windows and macOS legs (six `test_trampoline_exec` and the other skips are platform or host skips); `test_launcher_starts_on_image_without_a_shell`.
