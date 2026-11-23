# P-ACC-PROJ — red proof against the A0 stubs

Command: `cd test && OCX_COMMAND=<a0-ref-build>/target/debug/ocx uv run pytest tests/test_project_export_import.py -q -p no:randomly`
Result: 5 failed, 1 passed (6 tests).

- `test_import_then_project_exec_survives_clean` · red: `_export` asserts exit 0, got 101 (`P-EXPORT: export` todo panic) · the export is the scenario's own first step; the import half (offline import, delete export, exec, clean, exec) is not reached against stubs
- `test_root_import_patch_snapshot_round_trip` · red: `_export` exit 101, same panic · the export is a scenario step; the snapshot, import and offline-env comparison are not reached against stubs
- `test_global_export_import_registers_nothing` · red: `--global export` exit 101, same panic · the export under `--global` is the scenario's command
- `test_root_export_smoke` · red: `_export` exit 101 · the export is the command under test
- `test_root_import_smoke` · red: `_export` exit 101 (setup step) · import itself is not reached against stubs; its red is the export panic
- `test_root_export_grammar_errors` · guard (mutation red in P-PROOF) · the stub already runs the project load before its todo, so 64 and 78 hold; red comes only from removing the grammar checks
