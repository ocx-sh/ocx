# R1U: non-ASCII paths in the launcher

Tests (all in `crates/ocx_shim`): unit `non_ascii_manifest_target_and_key_resolve_to_the_real_root`,
`a_runfiles_tree_under_a_non_ascii_directory_is_searched`, `a_launcher_in_a_non_ascii_directory_finds_the_record_beside_it`,
`a_record_program_with_non_ascii_segments_is_accepted_and_planned`, Windows-only `a_wide_image_path_with_non_ascii_text_converts_whole`;
integration (unix) `the_root_is_found_through_a_runfiles_tree_under_a_non_ascii_directory`, `a_non_ascii_vendor_directory_and_program_path_launch`.

No product defect: all green on first run (Linux; `cargo test -p ocx_shim`: 175 unit + 21 integration pass).

Mutations (text confirmed present by grep, each restored):
- manifest reader skips non-ASCII lines: `non_ascii_manifest_target_and_key_resolve_to_the_real_root` red.
- `RealFs::list_dir` drops non-ASCII names: `a_runfiles_tree_under_a_non_ascii_directory_is_searched` red.
- `RealFs::read_sidecar` returns None for a non-ASCII path: beside, tree and manifest unit tests red; integration tree and vendor tests red.
- `relative_path_ok` refuses non-ASCII: `a_record_program_with_non_ascii_segments_is_accepted_and_planned` red.

Clippy: `-D warnings` fails on `clippy::type_complexity` at `direct.rs:387` (`split_manifest_line`, untouched, pre-existing); clean with that lint allowed.
