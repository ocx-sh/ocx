# R2A evidence (deferred tool survives clean, launcher-dir listing fails closed, loop-guard test)

Environment: Linux, scratch `HOME`/`OCX_HOME`, scratch `CARGO_TARGET_DIR`, `OCX_COMMAND` = the dev `ocx`; the `external/*` submodule
trees were copied in for the build and removed afterwards. Windows and macOS: not run, CI only.

1. `ocx clean` kept a deferred tree's config blobs but not its manifests; the offline claim check reads the manifest, so a
   never-started tool refused after an ordinary `ocx clean`. Root: `link_closure_config_blobs` in `tasks/prepare_lazy.rs` rooted
   config blobs only. It now also links each node's manifest and each declared dependency edge's manifest (an index for an index
   dependency) into the tree's `refs/blobs/` (the same GC root, no second mechanism). Doc comment on `claimed_shim_names` corrected.
   Test `test_a_deferred_tool_never_started_survives_ocx_clean_and_then_starts` (no store probe between clean and start: `package which`
   reads the manifest back). Red on the old code: `rc=79 ... failed to find package: ...@sha256:524bf5d4... — package not found`
   (exit 79, not 81, in a home whose registry is reachable). Green after: `2 passed` (this test and item 4's).
   Trees published before this change keep their old links; they are reused as complete and are not relinked.
2. Test `test_a_deferred_launcher_whose_manifest_blob_was_removed_by_hand_refuses_and_downloads_nothing`: the launcher exits 79, the
   `--format json` error document has `error.kind` `not_found`, `error.detail` `package_not_found`, and the message names the pinned
   digest; the package is not materialized. The message carries no `ocx install <id>` remedy text; the binary was not changed.
   The exit is not 81 and the slug is not `offline_manifest_missing` (that applies only when `--offline` blocks the read).
3. `existing_launcher_dirs` (`command/launcher/shim.rs`) returns `anyhow::Result`: an absent `deferred/` or candidate is empty, any
   other listing or stat error is returned with the path as context; the caller uses `?`. Unit test
   `the_launcher_dirs_treat_an_absent_deferred_root_as_empty_and_refuse_an_unlistable_one` (file where the directory belongs).
4. Docstring of `test_a_name_the_package_does_not_declare_is_refused_through_an_older_tree` corrected (refused by `validate_argv0`,
   never reaches the guard). New `test_a_declared_name_the_package_lacks_ends_when_started_through_an_older_tree_still_on_path`
   (declared `ghost`, package ships only `real`, started from `shims/<id>/bin` first on `PATH`, own process group, 90 s deadline):
   the guard strips the old directory, finds nothing, exit 65 naming package and name. Skipped on win32 (the older release's
   launcher is a POSIX script; same reason and marker as its sibling).
5. Limits: three cases added, one skipped on win32. `test/SUITE_FLOOR` 4603 to 4606; per-OS `SUITE_FLOOR` macos, windows,
   macos-release, windows-release 3956 to 3959, linux-release 3949 to 3952; `SKIP_CEILING` windows 1927 to 1928, windows-release
   2170 to 2171, macos and linux-release unchanged; census `PINNED` tests 3803 to 3806, skipped 157 to 158.

Commands: `cargo test -p ocx_package_manager prepare_lazy` 13 passed; `cargo test -p ocx --lib launcher::shim` 26 passed;
`cargo clippy -p ocx_package_manager -p ocx --all-targets -- -D warnings` clean; `cargo fmt` no change;
`pytest tests/test_lazy_loading.py` 39 passed; `pytest lint/ -m "not deep"` 813 passed; `suite_census.py --check` as pinned.
