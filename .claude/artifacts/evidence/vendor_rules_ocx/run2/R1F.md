# R1F evidence (CI surface fixes)

1. Recency gate empty source list. Red: `scripts/tests/test_shim_recency_gate.py::test_a_source_pathspec_that_matches_nothing_fails_and_names_the_paths` failed with `assert 0 == 1` ("no shim source history; nothing to compare"). Green after the step exits 1 naming the three paths; the pass and stale-blob cases stay green (3 passed).
2. Names kept: file `build-windows-shims.yml` (20+ references outside artifacts: taskfile, shim.rs, tests, rules, test_shim_inputs.py). Display `name:` "Build Windows Shims" -> "Launcher Build" (notify-discord.yml `workflow_run` list updated); job "Windows Acceptance (shim)" -> "Windows Acceptance (launcher)". `basic-gate` is the only required check, so no required-check name moved. `SHIM_SHA256` comments -> `RECORDED_SHA256`.
3. Both `paths:` lists gain `crates/ocx_package_manager/src/launcher.rs` and `launcher/**`; recency source list unchanged. `.claude/tests/test_workflows.py`: 442 passed.
4. No change. `acceptance.yml` shards install no Rust toolchain (rule), and `build_recorder` asserts `rustc` on PATH; every one of the module's 14 test functions takes `shim_entrypoint` (first one has 3 shell arms), so the committed blob alone would not make them run. Delta: 0.
5. suite_census comment: two lines added.
6. quarantine-start: now info only, owes no case (floor 8+1+3 on Apple silicon). Red: `test_the_quarantine_start_is_reported_and_owes_no_case` saw `ok   quarantine-start`; green after. `test_a_quarantine_attribute_that_cannot_be_set_fails` shows it can fail (fake xattr exit 1). 23 passed in that module set.
Not run: actionlint reported exit 0 via `ocx exec`; shellcheck not available on PATH (not run).
