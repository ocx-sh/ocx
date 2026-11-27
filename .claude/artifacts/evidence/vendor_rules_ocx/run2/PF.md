# PF evidence: Run 2 local verification rows and mutations

Pipeline PFB, worktree `.agents/worktrees/run2-PFB`, branch `hex/bazel-rbe-rules-ocx--PFB`, started at d5969ec5.
Environment for every cargo run: scratch `CARGO_TARGET_DIR`, `CARGO_BUILD_JOBS=4`, debug info off, `CARGO_INCREMENTAL=0`,
scratch `HOME` and `OCX_HOME`, one crate per run (`-p`), every run under `timeout 600`.

## Green baselines (full per-crate runs, exit 0)

| Crate | Command | Result |
|---|---|---|
| ocx_env_apply | `cargo test -p ocx_env_apply` | 78 passed plus 1 doctest |
| ocx_shim | `cargo test -p ocx_shim --bins` and its integration tests | 164 unit passed; 18 integration passed, 1 ignored |
| ocx_store | `cargo test -p ocx_store --lib` | 398 passed |
| ocx_config | `cargo test -p ocx_config --lib` | 871 passed, 1 ignored |
| ocx_package_manager | `cargo test -p ocx_package_manager --lib` | 1638 passed, 1 ignored (includes the two tests added here) |

`ocx_shim` is a bin crate, so `--lib` has no target there. A first batch that used `--lib` showed rc=101 for red and green alike
and is discarded.

## Mutation protocol

Each row: replace one exact text once, prove it landed (`git diff --stat` non-empty and the new text present), run the named
test scope, record the first failing test, `git checkout`-equivalent restore, `git status --porcelain` on the file empty, run the
same scope again and require green. A mutation that did not compile counts as no result and was redone in a compile-safe form
(`!strips`, `|| true`, `&& false`). The scope is the module filter or crate run named per row, so each red and green below is a
filtered run of that scope and not a whole-crate green.

## Rows proven red then green

Counts are passed and failed of the mutated run, then of the restored run. The test named is the first of the failing tests.

| Row | File | First failing test | Red run | Green run |
|---|---|---|---|---|
| V-L01 | `ocx_env_apply/src/launch.rs` | `launch::tests::contract_decode_refuses_an_unknown_key_in_an_env_entry` | 77 passed; 1 failed | 78 passed; 0 failed |
| V-L02 | `ocx_env_apply/src/launch.rs` | `launch::tests::contract_encode_is_byte_equal_to_the_four_examples` | 77 passed; 1 failed | 78 passed; 0 failed |
| V-L03 | `ocx_env_apply/src/launch.rs` | `launch::tests::contract_toolchain_tier_mismatch_is_refused` | 77 passed; 1 failed | 78 passed; 0 failed |
| V-L04 | `ocx_env_apply/src/launch.rs` | `launch::tests::contract_token_outside_direct_is_refused` | 77 passed; 1 failed | 78 passed; 0 failed |
| V-L05 | `ocx_env_apply/src/launch.rs` | `launch::tests::contract_record_path_for_unix_keeps_the_name` | 77 passed; 1 failed | 78 passed; 0 failed |
| V-L06 | `ocx_shim/src/core.rs` | `core::tests::vl06_an_empty_pin_resolves_to_the_next_rung` | 161 passed; 3 failed | 164 passed; 0 failed |
| V-L07 | `ocx_shim/src/core.rs` | `core::tests::vl07_the_selector_strip_folds_case_on_windows_only` | 162 passed; 2 failed | 164 passed; 0 failed |
| V-L08msg | `ocx_shim/src/direct.rs` | `direct::tests::a_missing_sidecar_names_the_path_it_tried` | 162 passed; 2 failed | 164 passed; 0 failed |
| V-L10a | `ocx_shim/src/core.rs` | `core::tests::vl10_the_walk_skips_empty_relative_own_and_other_launcher_entries` | 163 passed; 1 failed | 164 passed; 0 failed |
| V-L10b | `ocx_shim/src/core.rs` | `core::tests::vl10_a_path_with_the_launchers_directory_first_still_finds_the_real_ocx` | 161 passed; 3 failed | 164 passed; 0 failed |
| V-L12 | `ocx_shim/src/core.rs` | `core::tests::vl12_a_batch_program_is_refused_on_every_rung` | 163 passed; 1 failed | 164 passed; 0 failed |
| V-L14 | `ocx_shim/src/core.rs` | `core::tests::vl14_a_depth_past_the_counter_range_is_a_launcher_loop` | 161 passed; 3 failed | 164 passed; 0 failed |
| V-L15 | `ocx_shim/src/direct.rs` | `direct::tests::a_home_launcher_started_by_a_bare_argv0_off_path_runs_its_record` | 157 passed; 7 failed | 164 passed; 0 failed |
| V-L16 | `ocx_shim/src/direct.rs` | `direct::tests::a_home_launcher_started_by_a_bare_argv0_off_path_runs_its_record` | 157 passed; 7 failed | 164 passed; 0 failed |
| V-L17a | `ocx_shim/src/direct.rs` | `direct::tests::v61_a_direct_launcher_whose_started_path_is_not_the_image_is_refused` | 162 passed; 2 failed | 164 passed; 0 failed |
| V-L17b | `ocx_shim/src/direct.rs` | `direct::tests::vl17_a_home_kind_makes_no_identity_check_between_the_started_path_and_the_image` | 163 passed; 1 failed | 164 passed; 0 failed |
| V-L18 | `ocx_shim/src/direct.rs` | `direct::tests::vl18_an_image_path_the_kernel_marks_deleted_is_refused` | 163 passed; 1 failed | 164 passed; 0 failed |
| V-L21 | `ocx_package_manager/src/launcher/generate.rs` | `launcher::generate::tests::a_read_only_package_directory_without_entrypoints_fails_naming_the_path` | 36 passed; 6 failed | 42 passed; 0 failed |
| V-L22 | `ocx_package_manager/src/launcher/generate.rs` | `launcher::generate::tests::two_concurrent_first_uses_converge_on_one_complete_directory` | 40 passed; 2 failed | 42 passed; 0 failed |
| V-L23 | `ocx_package_manager/src/launcher/generate.rs` | `launcher::generate::tests::first_use_replaces_a_symlinked_directory_and_leaves_its_target_alone` | 40 passed; 2 failed | 42 passed; 0 failed |
| V-L24 | `ocx_package_manager/src/launcher/generate.rs` | `launcher::generate::tests::first_use_of_a_removed_package_fails_and_creates_nothing` | 40 passed; 2 failed | 42 passed; 0 failed |
| V-L25 | `ocx_package_manager/src/launcher/generate.rs` | `launcher::generate::tests::a_read_only_package_directory_falls_back_to_a_complete_older_entrypoints` | 41 passed; 1 failed | 42 passed; 0 failed |
| V-L26 | `ocx_store/src/file_structure/temp_store.rs` | `tasks::prepare_lazy::tests::a_deferred_tree_being_staged_survives_the_temp_sweep_and_publishes_under_deferred` | 10 passed; 1 failed | 11 passed; 0 failed |
| V-L27 | `ocx_package_manager/src/tasks/vendor.rs` | `tasks::vendor::tests::a_name_ending_in_the_record_suffix_is_refused` | 50 passed; 1 failed | 51 passed; 0 failed |
| V-L28 | `ocx_package_manager/src/tasks/render_toolchain.rs` | `tasks::render_toolchain::tests::a_second_render_changes_nothing_and_replaces_no_inode` | 113 passed; 5 failed | 118 passed; 0 failed |
| V-L29 | `ocx_package_manager/src/tasks/clean.rs` | `tasks::clean::tests::clean_retires_render_directories_the_stamp_does_not_keep` | 35 passed; 4 failed | 39 passed; 0 failed |
| V-L30 | `ocx_package_manager/src/tasks/clean.rs` | `tasks::clean::tests::clean_retires_render_directories_the_stamp_does_not_keep` | 35 passed; 4 failed | 39 passed; 0 failed |
| V-L33 | `ocx_package_manager/src/activation.rs` | `activation::bin_mode_entry_tests::v2_stamp_refuses_an_active_that_names_another_complete_directory` | 1637 passed; 1 failed | 1638 passed; 0 failed |
| V-L35 | `ocx_package_manager/src/tasks/render_toolchain.rs` | `tasks::render_toolchain::tests::the_staging_directory_is_created_exclusively` | 117 passed; 1 failed | 118 passed; 0 failed |
| V-L37 | `ocx_package_manager/src/launcher/generate.rs` | `launcher::generate::tests::a_launch_suffixed_name_is_skipped` | 37 passed; 5 failed | 42 passed; 0 failed |
| V-L39 | `ocx_config/src/env.rs` | `env::tests::a_launch_path_is_peeked_directly` | 57 passed; 3 failed | 60 passed; 0 failed |
| V-L41 | `ocx_package_manager/src/tasks/clean.rs` | `tasks::clean::tests::clean_retires_launcher_generations_other_than_the_hosts` | 37 passed; 2 failed | 39 passed; 0 failed |
| V-L43a | `ocx_shim/src/direct.rs` | `direct::tests::vl43_an_escaped_line_unescapes_its_key_and_target_and_splits_at_the_first_space` | 163 passed; 1 failed | 164 passed; 0 failed |
| V-L43b | `ocx_shim/src/direct.rs` | `direct::tests::vl43_duplicate_roots_are_deduplicated_before_the_cap_of_4096` | 163 passed; 1 failed | 164 passed; 0 failed |
| V-L44 | `ocx_shim/src/direct.rs` | `direct::tests::vl44_an_inherited_manifest_that_does_not_list_the_started_path_is_ignored` | 162 passed; 2 failed | 164 passed; 0 failed |
| V-L45 | `ocx_shim/src/direct.rs` | `direct::tests::a_windows_manifest_root_on_the_current_drive_is_rejected` | 160 passed; 4 failed | 164 passed; 0 failed |
| V-L51 | `ocx_store/src/file_structure/vendor_layout.rs` | `file_structure::vendor_layout::tests::contract_package_keys_refuses_a_hex16_clash_naming_both` | 397 passed; 1 failed | 398 passed; 0 failed |
| V-L52 | `ocx_package_manager/src/tasks/vendor.rs` | `tasks::vendor::tests::a_declared_binary_the_content_lacks_names_the_package_and_the_claim` | 50 passed; 1 failed | 51 passed; 0 failed |
| V-L53 | `ocx_package/src/vendor.rs` | `vendor::tests::example_round_trips` | 3 passed; 1 failed | 4 passed; 0 failed |
| V-L54 | `ocx_package_manager/src/tasks/vendor.rs` | `tasks::vendor::tests::the_manifest_lists_record_paths_and_the_selecting_index` | 51 passed; 1 failed | 52 passed; 0 failed |
| V-L55 | `ocx_package_manager/src/tasks/vendor.rs` | `tasks::vendor::tests::the_manifest_lists_index_lookups_only_with_a_recorder` | 50 passed; 1 failed | 51 passed; 0 failed |
| V-L56 | `ocx_store/src/file_structure/vendor_layout.rs` | `file_structure::vendor_layout::tests::contract_check_relative_path_refuses_reserved_segment` | 396 passed; 2 failed | 398 passed; 0 failed |
| V-L57 | `ocx_store/src/shim.rs` | `shim::tests::blob_for_serves_the_six_launcher_targets` | 36 passed; 2 failed | 38 passed; 0 failed |
| V-L59 | `ocx_package_manager/src/tasks/vendor.rs` | `tasks::vendor::tests::a_flat_manifest_offers_any_and_an_index_offers_its_candidates` | 51 passed; 1 failed | 52 passed; 0 failed |
| V-L60 place_package | `ocx_package_manager/src/tasks/vendor.rs` | `tasks::vendor::tests::placing_a_package_writes_nothing_into_the_index_directory` | 0 passed; 1 failed | 1 passed; 0 failed |
| V-L60 verify_selection | `ocx_package_manager/src/tasks/vendor.rs` | `tasks::vendor::tests::verifying_a_selection_reads_the_index_only_through_the_read_only_view` | 0 passed; 1 failed | 1 passed; 0 failed |

V-L22, V-L24 and V-L41 (constant G, `clean` generations) and V-L60 are in the table. V-L33 went red only when the whole
`bin_mode_entry` v2 gate was removed: the first mutation removed `active_target_name` alone and stayed green because
`active_is_valid` also refuses an active link naming another directory. Two guards defend that property, and the red test is
`v2_stamp_refuses_an_active_that_names_another_complete_directory`.

## Tests added

1. `tasks::vendor::tests::verifying_a_selection_reads_the_index_only_through_the_read_only_view` (commit 65ec7918). V-L60 for
   `verify_selection`: verification only reads, so a byte comparison of the index directory cannot go red. The reachable
   property is that every index read goes through `read_only_view()`. A probe index counts reads on the writable handle and on
   the view. Red by removing `read_only_view()` in `verify_selection` (writable reads become non-zero); green restored.
   `place_package` already had `placing_a_package_writes_nothing_into_the_index_directory`; removing its `read_only_view()`
   turns that red (row V-L60 place_package).
2. `tasks::vendor::tests::a_flat_manifest_offers_any_and_an_index_offers_its_candidates` (commit 93dc6aa6). V-L59. The mapping
   inside `index_platforms` is extracted into `offered_platforms` so the `any` offer is observable without a registry. Red by
   making a flat manifest offer an empty set; green restored.

## Rows proven after the first pass

- V-L43c, key `\s` unescape. Test `vl43_an_escaped_key_reads_backslash_s_as_a_space` in `crates/ocx_shim/src/direct.rs` calls
  `split_manifest_line` directly, because the lookup only suffix-checks a key. Green: `cargo test -p ocx_shim
  vl43_an_escaped_key` gives 1 passed (filtered run). Mutation `Some(b's') if key => out.extend([b'\\', b's'])` landed
  (grep line 388): the test reds with left `a\sb/x`, right `a b/x`. File restored from its copy, test green again.

## CI only, or another pipeline

CI only (Windows, macOS or rules_ocx jobs, not run here): V-L09, V-L11, V-L19, V-L31, V-L32, V-L36, V-L38, V-L46 to V-L49,
V-L58. V-L42 needs a real `self update` against a registry: not run. The Python acceptance halves (V-L13, V-L20, V-L34, V-L40)
and the `test/src/helpers.py` `shim_bin_dirs` item are in `test/**`, which PFA owns: not run.

## PF-notes checks

- Flake `a13_a_grant_through_the_config_overlay`: 10 of 10 single-test runs exit 0 and 10 of 10 threaded runs of the module
  (`command::self_group::activate`, 94 tests each) exit 0. Not reproduced. The full `ocx` lib run (1822 tests, which includes
  the `app::seam` child re-run) exits 0.
- Bazel `rust_test` for crate `ocx`: not run. It needs the hermetic LLVM toolchain and a crate fetch with no bound on time or
  network here, and a shared remote cache is forbidden. Cargo ran the same tests instead (scope: the cargo harness, not Bazel).

## Findings

- V-L57 contradiction: the plan and ADR say each launcher blob is at most 512 KiB (524288 bytes), but `SHIM_SIZE_BUDGET` in
  `crates/ocx_store/src/shim.rs` is `640 * 1024` and the x86_64 Windows blob is 535040 bytes per its doc comment. The budget test
  is green against 640 KiB. Changing either means touching committed blobs or checksums, which are hub files: reported only.
- `crates/ocx_package_manager/src/tasks/vendor.rs` `closure_root_of` still names `fs.shims`, and `setup_vendored`'s `.content()`
  path is latent (from the PF notes). No behaviour test reaches either: reported only.
