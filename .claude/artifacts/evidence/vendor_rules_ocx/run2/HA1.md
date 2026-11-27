# HA1 evidence: launcher acceptance tests, red first

Pipeline HA1, branch `hex/bazel-rbe-rules-ocx--HA1`. Build under test: `ocx` at pipeline base `333a5264` (CW2). One consistent run of all seven modules with the common brief's recipe (scratch `HOME` and `OCX_HOME`, `-q -p no:cacheprovider --tb=line`): **69 failed, 49 passed, 28 skipped**. Red lines are `file:line` of the failing assertion; a path of `<tmp>` is the test's private directory.

State legend: **red** = fails against this build by design; **green-by-design** = passes today (shared with the old layout, or guards a property that already holds); **Windows claim** = skipped here, runs in act M6 (`deep-repo.yml` acceptance-native windows-latest, and `build-windows-shims.yml` for the two shim modules).


## HA1.1 (`682f8f2e`, `448f03fa`): `test_entrypoints.py`

| Test | Row | Red line | State |
|---|---|---|---|
| `test_entrypoint_launcher_files_created_after_select` | - | `test/tests/test_entrypoints.py:60` AssertionError: expected exactly one launchers/<G> under <tmp> found [] | red |
| `test_entrypoint_unix_launcher_is_executable` | - | `test/tests/test_entrypoints.py:60` AssertionError: expected exactly one launchers/<G> under <tmp> found [] | red |
| `test_deselect_removes_current_symlink` | - | - | green-by-design |
| `test_install_without_entrypoints_leaves_current_entrypoints_absent` | - | - | green-by-design |
| `test_invalid_entrypoint_name_rejected_at_validation` | - | - | green-by-design |
| `test_install_without_select_does_not_trigger_collision_check` | - | - | green-by-design |
| `test_reselect_to_package_without_entrypoints_drops_entrypoints_dir` | - | `test/tests/test_entrypoints.py:60` AssertionError: expected exactly one launchers/<G> under <tmp> found [] | red |
| `test_launcher_invocation_runs_target_and_forwards_args` | - | `test/tests/test_entrypoints.py:60` AssertionError: expected exactly one launchers/<G> under <tmp> found [] | red |
| `test_baked_args_resolved_and_prepended_before_user_args` | - | `test/tests/test_entrypoints.py:60` AssertionError: expected exactly one launchers/<G> under <tmp> found [] | red |
| `test_launcher_dispatches_divergent_command` | - | `test/tests/test_entrypoints.py:60` AssertionError: expected exactly one launchers/<G> under <tmp> found [] | red |
| `test_root_package_entrypoints_appear_in_self_env` | - | `test/tests/test_entrypoints.py:451` AssertionError: expected a launchers/<G> PATH entry in env output; PATH values:  | red |
| `test_synthetic_entrypoints_path_emitted_after_declared_bin` | - | `test/tests/test_entrypoints.py:513` AssertionError: synthetic launchers/<G> PATH entry missing; PATH values: ['<tmp> | red |
| `test_exec_dep_launcher_via_path` | - | - | green-by-design |
| `test_exec_resolves_native_exe_shim_on_windows` | - | skipped: Windows launcher test | Windows claim |
| `test_env_two_roots_with_same_entrypoint_name_errors` | - | - | green-by-design |
| `test_exec_two_roots_with_same_entrypoint_name_errors` | - | - | green-by-design |
| `test_install_intra_closure_collision_aborts_before_candidate_symlink` | - | - | green-by-design |
| `test_install_transitive_closure_collision_aborts_before_disk` | - | - | green-by-design |
| `test_suite_a_entrypoint_collision_gated_on_interface_projection[sealed-no-c...` | - | - | green-by-design |
| `test_suite_a_entrypoint_collision_gated_on_interface_projection[private-no-...` | - | - | green-by-design |
| `test_suite_a_entrypoint_collision_gated_on_interface_projection[interface-c...` | - | - | green-by-design |
| `test_suite_a_entrypoint_collision_gated_on_interface_projection[public-coll...` | - | - | green-by-design |
| `test_suite_e_mixed_edge_private_seals_c_from_interface_no_collision` | - | - | green-by-design |
| `test_self_env_token_in_baked_args_is_refused_at_runtime` | C-028 | - | green-by-design |
| `test_entry_point_named_ocx_starts_the_real_ocx_without_a_loop` | V-L13 | `test/tests/test_entrypoints.py:1139` AssertionError: expected exactly one launchers/<G> PATH entry; PATH values: ['<t | red |
| `test_two_packages_with_an_ocx_entry_point_on_path_start_the_real_ocx` | V-L13 | `test/tests/test_entrypoints.py:1139` AssertionError: expected exactly one launchers/<G> PATH entry; PATH values: ['<t | red |
| `test_previous_release_install_gains_launchers_dir_at_next_exec_and_keeps_en...` | C-127, C-128, V-L20 | `test/tests/test_entrypoints.py:1236` AssertionError: first use must publish exactly one launchers/<G>; found [] | red |
| `test_concurrent_first_use_publishes_one_launchers_dir_and_both_succeed` | S-104 | `test/tests/test_entrypoints.py:1267` AssertionError: exactly one <G> and no .staging-*/temp debris under launchers/;  | red |
| `test_private_dependency_binary_is_reachable_from_the_program_an_entry_point...` | C-141 | `test/tests/test_entrypoints.py:1139` AssertionError: expected exactly one launchers/<G> PATH entry; PATH values: ['<t | red |
| `test_entry_point_is_started_by_bare_name_through_the_composed_path` | C-141 | `test/tests/test_entrypoints.py:1139` AssertionError: expected exactly one launchers/<G> PATH entry; PATH values: ['<t | red |

## HA1.2 (`5cb2c5fe`): `test_launcher_exec.py`

| Test | Row | Red line | State |
|---|---|---|---|
| `test_launcher_exec_success` | - | - | green-by-design |
| `test_launcher_exec_forwards_args` | - | - | green-by-design |
| `test_launcher_exec_missing_pkg_root_exits_64` | - | - | green-by-design |
| `test_launcher_exec_relative_path_exits_64` | - | - | green-by-design |
| `test_launcher_exec_outside_packages_root_exits_64` | - | - | green-by-design |
| `test_launcher_exec_missing_metadata_json_exits_64` | - | - | green-by-design |
| `test_launcher_hop_refuses_a_non_executable_dispatch_target` | - | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_launcher_reentry_layers_the_state_like_exec_does` | - | - | green-by-design |
| `test_package_launcher_builds_the_frozen_argv_and_counts_one_hop` | C-107, V-L07 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_package_launcher_record_is_a_package_record_for_the_installed_root` | C-106 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_package_launcher_increments_an_existing_depth_counter` | C-107, V-L07 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_package_launcher_keeps_the_tier_selectors_in_the_environment` | C-107 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_launcher_depth_of_32_exits_78_naming_the_loop` | C-109, V-L14 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_launcher_depth_of_31_still_starts_the_program` | C-109, V-L14 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_a_relative_pin_exits_78` | C-108, V-L10 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_an_empty_pin_counts_as_unset_and_the_path_walk_finds_ocx` | C-108, V-L10 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_the_path_walk_skips_the_launchers_own_directory_and_other_launchers` | C-108, V-L10 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_the_path_walk_with_no_ocx_exits_78` | C-108, V-L10 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_a_launcher_beside_only_a_retired_sidecar_exits_78_naming_the_record[shim]` | C-106, S-112 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_a_launcher_beside_only_a_retired_sidecar_exits_78_naming_the_record[sh...` | C-106, S-112 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_a_launcher_beside_only_a_retired_sidecar_exits_78_naming_the_record[exec]` | C-106, S-112 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_a_launcher_beside_only_a_retired_sidecar_exits_78_naming_the_record[di...` | C-106, S-112 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_a_launcher_reached_through_a_symlink_passes_the_targets_name` | C-110 | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |

## HA1.2 (`5cb2c5fe`): `test_trampoline_exec.py`

Windows-only cases of this module were rewritten in HA1.3 (see the note under "HA1.3").

| Test | Row | Red line | State |
|---|---|---|---|
| `test_item22_a_name_the_composition_does_not_provide_exits_65_naming_the_search` | C-010, C-069 | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_item22_nothing_is_spawned_on_the_refusal` | - | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_item22_vl40_two_project_homes_on_one_path_exit_65_through_native_launc...` | C-010, C-069 | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_item22_a_marker_bearing_project_path_is_not_refused` | - | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_c057_launcher_exec_propagates_65_for_a_name_that_resolves_to_a_trampoline` | C-011, C-057, C-069 | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_item1_a_trampoline_invoked_through_a_symlink_elsewhere_uses_the_baked_...` | - | - | green-by-design |
| `test_item38_an_exported_ocx_project_does_not_override_the_baked_selector` | S-009 | - | green-by-design |
| `test_item38_a_global_trampoline_under_an_exported_ocx_project_does_not_exit_64` | - | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_item38_a_tool_under_global_exec_sees_no_ocx_global_and_may_name_its_ow...` | - | - | green-by-design |
| `test_item38_a_tool_under_project_exec_sees_no_ocx_project` | - | - | green-by-design |
| `test_item36_a_trampoline_whose_home_lost_its_ocx_toml_exits_64_naming_the_path` | C-068 | - | green-by-design |
| `test_item36_a_trampoline_whose_home_lost_its_lock_exits_78` | C-068 | - | green-by-design |
| `test_item36_a_trampoline_whose_home_carries_a_stale_lock_exits_65` | C-068 | - | green-by-design |
| `test_item16_a_package_claiming_the_name_ocx_renders_a_trampoline_and_runs` | C-010 | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_item16_two_packages_claiming_one_name_render_one_trampoline_last_wins` | C-024 | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_item33_flipping_pinned_changes_the_composed_path_without_re_rendering` | C-046 | - | green-by-design |
| `test_vl07_a_project_toolchain_launcher_builds_the_project_argv_and_strips_t...` | C-107, V-L07 | `test/tests/test_trampoline_exec.py:1084` AssertionError: assert '3' == '4' | red |
| `test_vl07_a_global_toolchain_launcher_builds_the_global_argv` | C-107, V-L07 | `test/tests/test_trampoline_exec.py:136` AssertionError: `active` must point at shells/default-<16 hex>; resolved to <tmp | red |
| `test_vl07_a_toolchain_launcher_without_a_pin_starts_the_record_ocx_not_the_...` | C-108, V-L10 | `pathlib.py (test_trampoline_exec.py read_text of the missing record):771` FileNotFoundError: [Errno 2] No such file or directory: '<tmp> | red |
| `test_item15_dispatch_the_committed_blob_builds_the_project_wire_line` | C-032 | skipped: the committed shim blob is a Windows PE; it can only be executed on Windows | Windows claim |
| `test_item15_dispatch_the_committed_blob_builds_the_global_wire_line` | C-032 | skipped: the committed shim blob is a Windows PE; it can only be executed on Windows | Windows claim |
| `test_item38_the_committed_blob_strips_ocx_global_and_ocx_project_before_the...` | C-033 | skipped: the committed shim blob is a Windows PE; it can only be executed on Windows | Windows claim |
| `test_v20_a_deferred_record_does_not_strip_the_tier_selectors` | - | skipped: the committed shim blob is a Windows PE; it can only be executed on Windows | Windows claim |
| `test_v9_a_co_resident_ocx_exe_cannot_capture_a_trampoline_spawn` | C-010, C-069 | skipped: the committed shim blob is a Windows PE; it can only be executed on Windows | Windows claim |
| `test_v9_a_toolchain_record_without_ocx_still_dispatches` | - | skipped: the committed shim blob is a Windows PE; it can only be executed on Windows | Windows claim |

## HA1.2 (`5cb2c5fe`): `test_lazy_loading.py`

| Test | Row | Red line | State |
|---|---|---|---|
| `test_s001_lazy_mode_always_puts_a_shim_dir_on_path_with_no_content` | S-001 | `test/tests/test_lazy_loading.py:96` AssertionError: S-001: a deferred tool publishes a shim tree: expected exactly o | red |
| `test_s002_invoking_a_shimmed_binary_materializes_and_execs_the_target` | S-002 | - | green-by-design |
| `test_c011_a_claimed_name_the_package_does_not_ship_exits_instead_of_looping` | C-011 | `test/tests/test_lazy_loading.py:96` AssertionError: C-011: the unfulfillable name is shimmed: expected exactly one ` | red |
| `test_c011_a_shim_tree_reached_by_a_second_spelling_exits_instead_of_looping` | C-011 | `test/tests/test_lazy_loading.py:96` AssertionError: C-011: the unfulfillable name is shimmed: expected exactly one ` | red |
| `test_s002_invoking_a_shimmed_binary_offline_exits_81` | S-002 | `test/tests/test_lazy_loading.py:96` AssertionError: S-002: the shim exists before the offline trigger: expected exac | red |
| `test_s003_invoking_a_shimmed_entrypoint_applies_the_real_launcher_dispatch` | S-003 | - | green-by-design |
| `test_s004_a_second_invocation_of_the_same_name_does_not_re_enter_the_shim` | C-012, S-004 | `test/tests/test_lazy_loading.py:96` AssertionError: S-004: the first call must go through a shim: expected exactly o | red |
| `test_deferred_launcher_is_published_with_a_deferred_record` | C-106 | `test/tests/test_lazy_loading.py:96` AssertionError: the compose deferred the tool: expected exactly one `deferred/<G | red |
| `test_deferred_launcher_builds_the_frozen_argv_and_keeps_the_tier_selectors` | C-107, V-L07 | `test/tests/test_lazy_loading.py:96` AssertionError: the compose deferred the tool: expected exactly one `deferred/<G | red |
| `test_s005_env_output_is_byte_identical_cold_store_and_warm` | C-013, S-005 | `test/tests/test_lazy_loading.py:96` AssertionError: S-005: the cold compose deferred the tool: expected exactly one  | red |
| `test_s006_frozen_run_materializes_by_digest_and_writes_nothing_under_index` | S-006 | `test/tests/test_lazy_loading.py:96` AssertionError: S-006: the frozen run composed the tool as a shim: expected exac | red |
| `test_s007_which_answers_all_four_policy_and_state_cells` | C-016, S-007 | `test/tests/test_lazy_loading.py:96` AssertionError: S-007: the tool under test is deferred: expected exactly one `de | red |
| `test_s008_clean_keeps_a_lock_pinned_shim_and_the_next_compose_is_unchanged` | S-008 | `test/tests/test_lazy_loading.py:96` AssertionError: S-008: composed before the clean: expected exactly one `deferred | red |
| `test_s009_no_pull_composes_shims_for_local_metadata_and_omits_the_rest` | S-009 | `test/tests/test_lazy_loading.py:96` AssertionError: S-009: the first tool's metadata is now local: expected exactly  | red |
| `test_s013_lazy_mode_always_defers_on_every_host` | C-026, C-027, S-013 | `test/tests/test_lazy_loading.py:96` AssertionError: S-013: every host defers: expected exactly one `deferred/<G>/…/l | red |
| `test_s012_progress_report_degrades_silently_without_a_controlling_terminal` | S-012 | `test/tests/test_lazy_loading.py:96` AssertionError: S-012: the trigger goes through a shim: expected exactly one `de | red |
| `test_s012_errors_still_reach_stderr_under_progress_reporting` | S-012 | - | green-by-design |
| `test_s012_lazy_report_selects_the_controlling_terminal_channel[progress-ren...` | S-012 | `test/tests/test_lazy_loading.py:96` AssertionError: S-012: the trigger goes through a shim: expected exactly one `de | red |
| `test_s012_lazy_report_selects_the_controlling_terminal_channel[silent-stays...` | S-012 | `test/tests/test_lazy_loading.py:96` AssertionError: S-012: the trigger goes through a shim: expected exactly one `de | red |
| `test_sequence_1_compose_clean_compose_keeps_one_shim_tree_throughout` | S-008 | `test/tests/test_lazy_loading.py:96` AssertionError: sequence 1: after the first compose: expected exactly one `defer | red |
| `test_sequence_2_clean_after_materialization_keeps_both_the_package_and_the_...` | - | `test/tests/test_lazy_loading.py:96` AssertionError: sequence 2: composed before the trigger: expected exactly one `d | red |
| `test_sequence_3_removing_the_tool_lets_clean_collect_its_shim` | - | `test/tests/test_lazy_loading.py:96` AssertionError: sequence 3: composed before the removal: expected exactly one `d | red |
| `test_sequence_4_force_clean_collects_the_shim_and_the_next_compose_regenera...` | C-013 | `test/tests/test_lazy_loading.py:96` AssertionError: sequence 4: composed before the force clean: expected exactly on | red |
| `test_sequence_5_clean_collects_neither_an_eager_package_nor_a_lazy_shim` | - | `test/tests/test_lazy_loading.py:96` AssertionError: sequence 5: the lazy tool defers: expected exactly one `deferred | red |
| `test_sequence_6_force_clean_after_materialization_returns_the_tool_to_shim_...` | - | `test/tests/test_lazy_loading.py:96` AssertionError: sequence 6: composed before the trigger: expected exactly one `d | red |
| `test_sequence_7_the_deferred_advisory_is_raised_once_and_names_the_deferred...` | C-015 | - | green-by-design |
| `test_c015_pull_serializes_the_advisories_it_warns_about` | C-015 | - | green-by-design |
| `test_pull_materializes_a_package_any_selected_group_wants_eager` | - | - | green-by-design |
| `test_sequence_8_offline_regenerates_a_collected_shim_when_the_metadata_is_l...` | S-009 | `test/tests/test_lazy_loading.py:96` AssertionError: sequence 8: composed online first: expected exactly one `deferre | red |
| `test_sequence_9_recomposing_a_published_shim_stages_nothing` | - | `test/tests/test_lazy_loading.py:96` AssertionError: sequence 9: the first compose publishes the tree: expected exact | red |
| `test_lock_with_lazy_mode_always_leaves_the_content_in_the_registry` | - | - | green-by-design |
| `test_lock_without_lazy_mode_materialises_the_content` | - | - | green-by-design |
| `test_lock_no_pull_names_the_tools_it_could_not_render_and_renders_the_rest` | - | - | green-by-design |

## HA1.3: `test(launcher): one launcher per home kind`

Windows bodies of `test_trampoline_exec.py` (HA1.2's module, in the pipeline file set): the six `windows_only` cases no longer pin an `ocx.cmd` (a batch program is refused with 78 on every rung, C-108); they pin a native argv recorder compiled once with `rustc` (`launcher_fixtures.build_recorder`) and assert the recorded argv list and `OCX_*` environment. Windows claim, act M6 (`build-windows-shims.yml` has `rustc`; `acceptance-native` needs it on `PATH`).


## HA1.3: `test_entrypoints_crossplat.py`

| Test | Row | Red line | State |
|---|---|---|---|
| `test_linux_unix_launcher_exists_and_is_executable` | C-128 | `test/tests/test_entrypoints_crossplat.py:75` Failed: current/launchers/<G> not reachable after install --select; expected the | red |
| `test_linux_install_writes_only_launcher_and_record` | V-L38 | `test/tests/test_entrypoints_crossplat.py:107` Failed: current/launchers/<G> not reachable after install --select | red |
| `test_linux_every_launcher_of_a_package_is_the_same_bytes` | C-128 | `test/tests/test_entrypoints_crossplat.py:128` Failed: current/launchers/<G> not reachable after install --select | red |
| `test_macos_unix_launcher_exists` | - | skipped: macOS-specific launcher test | Windows claim |
| `test_macos_path_with_spaces_in_ocx_home` | - | skipped: macOS path-with-spaces test | Windows claim |
| `test_windows_native_launcher_exists_no_script_no_cmd` | C-128, V-L38 | skipped: Windows-only launcher test | Windows claim |

## HA1.3: `test_windows_shim.py`

| Test | Row | Red line | State |
|---|---|---|---|
| `test_launcher_starts_from_each_shell_with_only_exe_and_launch_on_disk[cmd]` | V-L38 | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_starts_from_each_shell_with_only_exe_and_launch_on_disk[pwsh]` | V-L38 | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_starts_from_each_shell_with_only_exe_and_launch_on_disk[git-b...` | V-L38 | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_argv0_is_real_target_not_cmd_exe` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_forwards_args_verbatim` | C-107 | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_ampersand_arg_not_executed` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_propagates_child_exit_code` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_forwards_ctrl_c` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_directory_holds_exe_and_launch_only_and_bare_name_reaches_the...` | C-108, V-L38 | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_never_starts_a_planted_ocx_exe_in_cwd_or_its_own_directory` | V-L11 | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_missing_record_exits_78` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_malformed_record_exits_78` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_honours_ocx_binary_pin` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_refuses_a_batch_file_pin_with_78` | C-108 | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_runs_without_console` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |
| `test_launcher_empty_pin_falls_through_to_the_path_walk` | - | skipped: Windows native shim behaviour; Win32-only (registry-independent, runs on windows | Windows claim |

## HA1.3: `test_exec_env_compat.py`

| Test | Row | Red line | State |
|---|---|---|---|
| `test_a_hook_shell_holding_an_older_format_state_stands_down_once_and_unset_...` | - | - | green-by-design |
| `test_a_nested_exec_past_the_cap_drops_the_outer_records_first_and_warns_onc...` | - | - | green-by-design |
| `test_a_hook_state_past_the_cap_drops_every_record_and_a_later_spawn_keeps_t...` | - | - | green-by-design |
| `test_a_secret_named_project_constant_reaches_the_child_and_is_recorded_verb...` | - | - | green-by-design |
| `test_a_secret_named_project_constant_reaches_the_child_and_is_recorded_verb...` | - | - | green-by-design |
| `test_a_launcher_in_an_existing_environment_follows_current_to_the_updated_b...` | - | - | green-by-design |
| `test_a_launcher_under_another_majors_state_keeps_offline_finds_the_home_and...` | - | `test/src/launcher_fixtures.py:98` AssertionError: <tmp> must hold exactly one 16-hex generation directory; got [] | red |
| `test_a_launcher_under_another_majors_state_keeps_offline_finds_the_home_and...` | - | - | green-by-design |
| `test_a_previous_release_launcher_in_the_middle_of_a_chain_hands_the_outer_s...` | - | - | green-by-design |
| `test_a_previous_release_trampoline_in_the_middle_of_a_chain_layers_its_proj...` | - | - | green-by-design |
| `test_an_old_package_launcher_an_older_ocx_wrote_starts_its_tool_through_thi...` | S-101, V-L09 | - | green-by-design |
| `test_an_old_toolchain_trampoline_with_an_exec_sidecar_starts_its_tool_throu...` | V-L09 | skipped: `.exec` trampolines are Windows launchers; Windows claim | Windows claim |
| `test_a_state_at_the_cap_fits_one_cmd_set_line` | - | skipped: cmd.exe's 8191-character command line exists only on Windows; no POSIX shell has | Windows claim |

## Not run

- Bazel lanes (`bazel:test:accept`, `bazel:tag:guard`, unit): none; no Bazel call. `task verify:scoped`: not run, it routes a Bazel lane (rc-file rule). `scripts/scoped_gate.py --plan` ran and lists the seven modules.
- Windows and macOS legs: the Windows-only cases above were written and only proven to import and collect (`--collect-only`) here.
- `ruff format --check`: not a gate (base files are not format-clean); `ruff check` on the five touched files: clean.

## For PF

Re-run the seven modules green after LA, EP, TR, TD and act B (run from `test/`, scratch `HOME` and `OCX_HOME`, `OCX_COMMAND` the rebuilt `ocx`, `OCX_TEST_ZOT` and `OCX_TEST_PREVIOUS_OCX` the test-group tools):

```sh
uv run pytest tests/test_entrypoints.py tests/test_entrypoints_crossplat.py tests/test_launcher_exec.py tests/test_trampoline_exec.py tests/test_lazy_loading.py tests/test_windows_shim.py tests/test_exec_env_compat.py -q -p no:cacheprovider --tb=line -rfs
```

