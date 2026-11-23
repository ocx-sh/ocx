# P-ACC-EXP red proof against A0 stubs

Command: `cd test && OCX_COMMAND=<a0-ref-build>/target/debug/ocx uv run pytest tests/test_package_export.py -q -p no:randomly`
Result: 8 failed, 1 passed.

The A0 stub panics with `not yet implemented: P-EXPORT: export` (rc=101) at `tasks/export.rs:84`. The panic is the command under test, reached after fixture publishing succeeded.

- test_export_tagged_package_layout · red: `_export` check=True, rc=101 · the export command is the scenario's step
- test_export_bare_identifier_exports_latest · red: first `_export`, rc=101 · same
- test_export_keeps_registries_apart · red: `_export` with two packages, rc=101 · same
- test_export_platform_filter · red: first `_export` (no `-p`), rc=101 · same
- test_export_slug_collision_refused · red: `assert result.returncode == 65` got 101 · scenario's own exit-code assertion
- test_export_commits_first_sight_pin · red: `_export`, rc=101 · same as layout
- test_offline_export_of_unknown_tag_is_81 · red: `assert result.returncode == 81` got 101 · scenario's own exit-code assertion
- test_export_report_fields · red: first `_export`, rc=101 · same as layout
- test_export_requires_output_and_package · passes against A0: guard (mutation red in P-PROOF) · clap grammar is already real in A0, so exit 64 holds on a stub; mutation: drop `required = true` on `-o`
