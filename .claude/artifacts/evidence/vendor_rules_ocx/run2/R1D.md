# R1D evidence (launcher program and record)

Cargo: `-p ocx_shim`, `-p ocx_env_apply`, `-p ocx_package_manager --lib launcher`, `-p ocx --lib exit`; every run on Linux only.

1. Diagnostics, 767be683. Wording tests updated to the new text; new tests `a_missing_ocx_names_the_source_the_program_came_from`
   (main.rs), `a_refusal_about_the_program_names_the_remedy_for_its_source` (core.rs), red by compile before `ProgramSource`
   existed. `v45_no_root_names_the_missing_tree_and_manifest_on_every_host` replaces the `--enable_runfiles` test. Green: ocx_shim 167.
   Windows spawn path (`OcxNotFound` source): not run, CI only.
2. Manifest, 74af321d. `a_manifest_path_already_scanned_is_not_read_again`: red with the path dedupe mutated away
   (`left: 2 right: 1`, reads listed the MANIFEST twice), green with it; `a_manifest_after_the_accepted_root_is_not_read`.
   `vl44_an_inherited_manifest_that_is_not_a_regular_file_is_refused` now starts from a world with no accepted root, because an
   accepted root ends the scan before a later manifest is opened. Green: 169.
3. V-L18, 61a7a1cd. `a_launcher_whose_file_was_deleted_exits_78_saying_so` (direct_launch.rs, Linux): with the
   `checked_image_link` call replaced by a plain string the run exited 78 with "is not the file this launcher runs from" and the
   assertion on "was deleted after it started" failed; restored, green.
4. RecordError, 4ab104fa and 49763d01. `Error` impl added; `Error::LauncherRecordRefused { path, #[source] source }`, DataError (65),
   slug `launcher_record_refused`; `validate_for_write` and `write_direct_launcher_for` use it. Test
   `a_record_the_launcher_would_refuse_is_a_data_error_whose_source_is_the_record_error` (red by compile). The vendor writer's
   non-path record errors now leave as `VendorError::Package(LauncherRecordRefused)` instead of `ValueUnrepresentable`.
   Registry: `exit::tests::detail_rows_snapshot_matches_the_registry` is RED until the hub row is added (only that row differs):
   `launcher_record_refused<TAB>DataError<TAB>A launch record breaks a rule the launcher enforces when it reads it`, after
   `launcher_path_not_utf8`. 340 other exit tests pass.
5. launch.rs nits, 4ab104fa. `decode_reports_a_newer_schema_version_before_it_reads_the_strict_shape`: red with the version check
   disabled (Malformed instead of SchemaVersion(2)), green with it. `separator` doc states what is enforced. "first seen" comment
   fixed in f27622b2.
6. Old-model text, c0345f76. `'` and `"` have no remaining reason: JSON escapes both and `build_child_command_line` quotes every
   token by the CommandLineToArgvW rules; kept refused by decision. `\n \r \0` are still refused by `LaunchRecord::validate`.
7. body.rs now uses `ocx_env_apply::launch::is_absolute_any_host` (edge allowed in `scripts/crate_map.toml`); identical logic.
8. f27622b2. Plain duplicate: the candidate-(a) path equals `record_path`, the bytes are compared with themselves; no
   time-of-check purpose. `the_record_beside_the_started_path_is_not_read_again_for_the_own_root` red (read twice), green after;
   control `a_record_under_another_root_is_still_read_and_compared`.
