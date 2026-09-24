# Port evidence — WP-13 pilot

Plan `plan_test_speed_tiers.md` C-025 / ADR `adr_test_speed_tiers.md` C-RUBRIC. The mutation
proof for every acceptance case the pilot deleted: mutate the production code the case exercises,
the Rust port goes red; restore, it goes green; mutation and restore both proven landed.

- **Base:** the C-SEAM commit `test: add the in-process CLI seam for crate-level ports` (`da1d8a75`;
  the porters worked on its pre-split twin `dac88a67`, the same tree).
- **Port landed:** the Opus porter's (two-model comparison, `decision_port_down_pilot.md`), restricted
  to the 37 test functions an independent Opus audit judged `full`. 45 collected cases deleted.
- **How the proofs ran:** one mutation at a time, driven by a script that refuses to run unless the
  mutated needle is present and compares the restored bytes to the original (`mutdrive.py`); one
  `cargo nextest run -p <pkg> --lib -E 'test(=<path>)'` per mutation. 47 of 47 of the porter's ports
  went red on the first mutation tried for them, with no assertion changed afterwards.
- **Green:** `cargo nextest run -p ocx` (1459 passed), `cargo nextest run -p ocx_package --features
  ocx_oci/__testing,ocx_index/__testing,ocx_util/__testing` (921 passed), EXIT=0, on the porter's
  tree; on the landed tree `bazel test //crates/ocx_cli:ocx_cli_test` (1437 passed),
  `//crates/ocx_cli:ocx_cli_seam_test` (44 passed), `//crates/ocx_package:ocx_package_test`
  (915 passed), EXIT=0.

## Per-case proof (the 37 deleted test functions)

`first pass`: the first version of the port went red on the first mutation tried. ¹ `no` for nine
status ports only because their shared `drive()` helper was wrapped in `Box::pin` after the first
green run (4 of 10 overflowed libtest's 2 MiB stack) — before any mutation and without touching an
assertion.

| original case | Rust port (file :: test) | mutation (file:line — change) | red (command — failing line) | first pass |
|---|---|---|---|---|
| `test_platform_pairs.py::test_wasm_target_claims_no_binaries` | `crates/ocx_package/src/bin_scan.rs :: tests::wasm_target_claims_no_binaries_where_the_same_tree_under_linux_claims_hello` | crates/ocx_package/src/bin_scan.rs:235 — `if false && is_wasm_target(platform)` (wasm arm of claim_name disabled) | `cargo nextest run --workspace --lib -E 'package(ocx_package) & test(wasm_target_claims)'` — crates/ocx_package/src/bin_scan.rs:660 `assertion `left == right` failed: a wasm target must claim no binaries` | yes |
| `test_platform_pairs.py::test_unsupported_platform_pair_is_refused` | `crates/ocx_cli/src/options/platform.rs :: seam::an_unsupported_pair_is_refused_at_parse_naming_the_supported_pairs (all 4 params)` | crates/ocx_oci/src/platform.rs:54 — `if true \|\| SUPPORTED_PAIRS.contains(..)` (validate_pair accepts every pair) | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/options/platform.rs:93 `the refusal for wasip1/amd64 must list the supported pairs: ERROR `ocx package install` is` | yes |
| `test_platform_pairs.py::test_supported_pair_reaches_resolution` | `crates/ocx_cli/src/options/platform.rs :: tests::every_supported_pair_parses_as_a_platform_argument (all 5 params)` | crates/ocx_oci/src/platform.rs:54 — `if false && SUPPORTED_PAIRS.contains(..)` (validate_pair rejects every pair) | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/options/platform.rs:50 `wasip1/wasm is a supported pair and must parse; got a usage error: error: invalid value 'w` | yes |
| `test_status.py::test_status_without_lock_reports_declared_only` | `crates/ocx_cli/src/command/status.rs :: seam::status_without_lock_reports_declared_only` | crates/ocx_cli/src/api/data/status.rs:244 — absent lock reported `present: true` | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:207 `assertion `left == right` failed` | no¹ |
| `test_status.py::test_status_with_lock_reports_every_platform` | `crates/ocx_cli/src/command/status.rs :: seam::status_with_lock_reports_every_platform` | crates/ocx_cli/src/api/data/status.rs:226 — platforms map `.iter().take(1)` | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:253 `{"linux/amd64": String("sha256:11111111111111111111111111111111111111111111111111111111111` | no¹ |
| `test_status.py::test_status_reports_unreadable_lock` | `crates/ocx_cli/src/command/status.rs :: seam::status_reports_unreadable_lock` | crates/ocx_cli/src/command/status.rs:54 — unparseable lock swallowed as absent (`Err(_) => Ok(None)`) | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:329 `assertion `left == right` failed` | no¹ |
| `test_status.py::test_status_reports_env_verbatim_per_scope` | `crates/ocx_cli/src/command/status.rs :: seam::status_reports_env_verbatim_per_scope` | crates/ocx_cli/src/api/data/status.rs:198 — default-group env dropped (`BTreeMap::new()`) | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:364 `assertion `left == right` failed: {}` | no¹ |
| `test_status.py::test_status_reports_package_settings` | `crates/ocx_cli/src/command/status.rs :: seam::status_reports_package_settings` | crates/ocx_cli/src/api/data/status.rs:316 — `no_patches` inverted | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:405 `assertion `left == right` failed` | no¹ |
| `test_status.py::test_status_outside_a_project_exits_64` | `crates/ocx_cli/src/command/status.rs :: seam::status_outside_a_project_exits_64` | crates/ocx_cli/src/app/project_context.rs:121 — NoProject classified 78 instead of 64 | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:425 `assertion `left == right` failed: stderr: ERROR no ocx.toml found in /home/mherwig/.cache/` | no¹ |
| `test_status.py::test_status_names_the_selected_directory_when_it_holds_no_manifest` | `crates/ocx_cli/src/command/status.rs :: seam::status_names_the_selected_directory_when_it_holds_no_manifest (both params: flag, env)` | crates/ocx_cli/src/app/project_context.rs:238 — explicit dir reported as the cwd walk ([flag]); supplementary S8b crates/ocx_config/src/loader.rs:835 OCX_PROJECT ignored → `[env] the error must name the selected directory` | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:453 `[flag] the error must name the selected directory:` | no¹ |
| `test_status.py::test_status_rejects_selectors` | `crates/ocx_cli/src/command/status.rs :: seam::status_rejects_selectors` | crates/ocx_cli/src/command/status.rs:40 — `Status` gains `-g` and positional names | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:479 `assertion `left != right` failed` | no¹ |
| `test_status.py::test_status_makes_no_network_or_store_writes` | `crates/ocx_cli/src/command/status.rs :: seam::status_makes_no_network_or_store_writes` | crates/ocx_cli/src/command/status.rs:62 — status returns 1 under --offline; supplementary S10b creates $OCX_HOME/symlinks/<binding> → `status must not create install symlinks` | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/status.rs:496 `assertion `left == right` failed: ` | no¹ |
| `test_config_test.py::test_config_test_previews_merge_of_candidate_onto_machine_tiers` | `crates/ocx_cli/src/command/config_test.rs :: seam::previews_merge_of_candidate_onto_machine_tiers` | crates/ocx_package_manager/src/managed_config/preview.rs:77 — machine (base) tiers dropped from the preview | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:209 `assertion `left == right` failed: a machine value the candidate does not set must survive ` | yes |
| `test_config_test.py::test_config_test_candidate_overrides_machine_value` | `crates/ocx_cli/src/command/config_test.rs :: seam::candidate_overrides_machine_value` | crates/ocx_package_manager/src/managed_config/preview.rs:78 — machine tier merged over the candidate | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:240 `assertion `left == right` failed` | yes |
| `test_config_test.py::test_config_test_config_overlay_outranks_the_candidate` | `crates/ocx_cli/src/command/config_test.rs :: seam::config_overlay_outranks_the_candidate` | crates/ocx_package_manager/src/managed_config/preview.rs:79 — explicit overlay not merged | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:264 `assertion `left == right` failed: an explicit overlay outranks an adopted payload, so it m` | yes |
| `test_config_test.py::test_config_test_candidate_still_wins_where_the_overlay_is_silent` | `crates/ocx_cli/src/command/config_test.rs :: seam::candidate_still_wins_where_the_overlay_is_silent` | crates/ocx_package_manager/src/managed_config/preview.rs:78 — candidate ignored by the preview | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:285 `assertion `left == right` failed` | yes |
| `test_config_test.py::test_config_test_reports_the_machines_managed_posture` | `crates/ocx_cli/src/command/config_test.rs :: seam::reports_the_machines_managed_posture` | crates/ocx_cli/src/command/config_test.rs:54 — OCX_MANAGED_CONFIG not consulted for the posture | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:303 `assertion `left == right` failed` | yes |
| `test_config_test.py::test_config_test_reports_an_opted_out_managed_posture` | `crates/ocx_cli/src/command/config_test.rs :: seam::reports_an_opted_out_managed_posture` | crates/ocx_cli/src/api/data/config_test.rs:115 — managed.required hardcoded true | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:318 `assertion `left == right` failed` | yes |
| `test_config_test.py::test_config_test_reports_no_managed_tier_when_unconfigured` | `crates/ocx_cli/src/command/config_test.rs :: seam::reports_no_managed_tier_when_unconfigured` | crates/ocx_cli/src/command/config_test.rs:54 — a managed source fabricated when none is configured | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:329 `{"candidate":"/home/mherwig/.cache/wp13-opustmp/.tmpOxBuW2/candidate.toml","valid":true,"r` | yes |
| `test_config_test.py::test_config_test_plain_output_is_a_field_value_table` | `crates/ocx_cli/src/command/config_test.rs :: seam::plain_output_is_a_field_value_table` | crates/ocx_cli/src/api/data/config_test.rs:176 — plain header Field renamed Key | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:341 `Key               Value                                                      ` | yes |
| `test_config_test.py::test_config_test_plain_repeats_the_field_name_on_every_row` | `crates/ocx_cli/src/command/config_test.rs :: seam::plain_repeats_the_field_name_on_every_row` | crates/ocx_cli/src/api/data/config_test.rs:155 — only the first Registries row labelled | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:364 `both the candidate entry and the built-in ocx.sh entry must be labelled: Field       Value` | yes |
| `test_config_test.py::test_config_test_rejects_managed_section_exit_78` | `crates/ocx_cli/src/command/config_test.rs :: seam::rejects_managed_section_exit_78` | crates/ocx_package_manager/src/managed_config/publish.rs:325 — [managed] section no longer refused | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:381 `assertion `left == right` failed: ` | yes |
| `test_config_test.py::test_config_test_rejects_invalid_toml_exit_78` | `crates/ocx_cli/src/command/config_test.rs :: seam::rejects_invalid_toml_exit_78` | crates/ocx_cli/src/command/config_test.rs:29 — InvalidToml loses its typed classification (erased to anyhow) | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:393 `assertion `left == right` failed: ERROR managed config payload is not a valid config file` | yes |
| `test_config_test.py::test_config_test_rejects_oversize_payload_exit_78` | `crates/ocx_cli/src/command/config_test.rs :: seam::rejects_oversize_payload_exit_78` | crates/ocx_config/src/managed_config.rs:71 — payload cap raised 64 KiB -> 1 MiB | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:405 `assertion `left == right` failed: ` | yes |
| `test_config_test.py::test_config_test_missing_candidate_exits_79` | `crates/ocx_cli/src/command/config_test.rs :: seam::missing_candidate_exits_79` | crates/ocx_cli/src/exit/ocx_config.rs:174 — ManagedConfigPublishError::ReadFailed NotFound classified 74 | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:417 `assertion `left == right` failed: ERROR failed to read managed config payload '/home/mherw` | yes |
| `test_config_test.py::test_config_test_refuses_both_extra_ca_certs_keys_78` | `crates/ocx_cli/src/command/config_test.rs :: seam::refuses_both_extra_ca_certs_keys_78` | crates/ocx_package_manager/src/managed_config/publish.rs:338 — both extra-CA keys no longer refused | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:433 `assertion `left == right` failed: ` | yes |
| `test_config_test.py::test_config_test_accepts_pem_payload` | `crates/ocx_cli/src/command/config_test.rs :: seam::accepts_pem_payload` | crates/ocx_config/src/lib.rs:271 — extra_ca_certs_pem no longer a schema key | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:453 `assertion `left == right` failed: a key this ocx understands must not be reported as unkno` | yes |
| `test_config_test.py::test_config_test_rejects_plain_http_mirror_exit_78` | `crates/ocx_cli/src/command/config_test.rs :: seam::rejects_plain_http_mirror_exit_78` | crates/ocx_cli/src/command/config_test.rs:40 — mirror-map gate result discarded | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:474 `assertion `left == right` failed: ` | yes |
| `test_config_test.py::test_config_test_allowed_plain_http_mirror_passes` | `crates/ocx_cli/src/command/config_test.rs :: seam::allowed_plain_http_mirror_passes` | crates/ocx_cli/src/command/config_test.rs:39 — OCX_INSECURE_REGISTRIES half of the plain-HTTP set dropped | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:106 `assertion `left == right` failed: config test must exit 0: ERROR mirror for 'ghcr.io' uses` | yes |
| `test_config_test.py::test_config_test_plain_http_mirror_allowed_by_candidate_registries_entry` | `crates/ocx_cli/src/command/config_test.rs :: seam::plain_http_mirror_allowed_by_candidate_registries_entry` | crates/ocx_cli/src/command/config_test.rs:39 — candidate config half of the plain-HTTP set dropped | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:106 `assertion `left == right` failed: config test must exit 0: ERROR mirror for 'ghcr.io' uses` | yes |
| `test_config_test.py::test_config_test_rejects_empty_patch_registry_exit_78` | `crates/ocx_cli/src/command/config_test.rs :: seam::rejects_empty_patch_registry_exit_78` | crates/ocx_config/src/patch.rs:226 — empty [patches] registry no longer refused | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:522 `assertion `left == right` failed: a malformed [patches] tier must classify as a config err` | yes |
| `test_config_test.py::test_config_test_falls_back_to_the_env_patch_tier` | `crates/ocx_cli/src/command/config_test.rs :: seam::falls_back_to_the_env_patch_tier` | crates/ocx_cli/src/command/config_test.rs:49 — OCX_PATCHES env tier not consulted | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:541 `assertion `left == right` failed` | yes |
| `test_config_test.py::test_config_test_candidate_patches_outrank_the_env_tier` | `crates/ocx_cli/src/command/config_test.rs :: seam::candidate_patches_outrank_the_env_tier` | crates/ocx_cli/src/command/config_test.rs:47 — env patch tier outranks the config tier | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:553 `assertion `left == right` failed` | yes |
| `test_config_test.py::test_config_test_reports_unknown_keys_and_still_exits_zero` | `crates/ocx_cli/src/command/config_test.rs :: seam::reports_unknown_keys_and_still_exits_zero` | crates/ocx_package_manager/src/managed_config/preview.rs:68 — ignored-key reporter discards every key | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:573 `typo'd section must be listed: []` | yes |
| `test_config_test.py::test_config_test_does_not_check_keys_inside_a_mirrors_entry` | `crates/ocx_cli/src/command/config_test.rs :: seam::does_not_check_keys_inside_a_mirrors_entry` | crates/ocx_config/src/mirror.rs:354 — mirror table role `registry` no longer read | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:602 `the correctly spelled role must still take effect` | yes |
| `test_config_test.py::test_config_test_clean_payload_reports_no_unknown_keys` | `crates/ocx_cli/src/command/config_test.rs :: seam::clean_payload_reports_no_unknown_keys` | crates/ocx_package_manager/src/managed_config/preview.rs:70 — a phantom unknown key always reported | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:617 `assertion `left == right` failed` | yes |
| `test_config_test.py::test_config_test_finds_the_key_the_ordinary_command_ignores` | `crates/ocx_cli/src/command/config_test.rs :: seam::finds_the_key_the_ordinary_command_ignores` | crates/ocx_package_manager/src/managed_config/preview.rs:116 — ignored-key path joined with / instead of . | `cargo nextest run -p ocx --lib -E ...` — crates/ocx_cli/src/command/config_test.rs:629 `the same key the loader ignores must be surfaced here: {"candidate":"/home/mherwig/.cache/` | yes |

The status rows' `red` column names the command as `-E ...`; the exact filter is
`-E 'test(=command::status::seam::<test>)'`, and the same for `config_test::seam::`.

## Independent re-check (Opus audit, its own mutations)

An auditor that did not write the port picked its own one-line mutation for 12 sampled cases
(none repeats the porter's), one at a time, one test run each. The 8 rows below are the sample's
landed cases; all 8 killed. (The sample's other four: the status drift companion, killed; three
`config_setup` ports, two killed and one survived — those ports did not land.)

| case | auditor mutation | result |
|---|---|---|
| `test_platform_pairs.py::test_wasm_target_claims_no_binaries` | `bin_scan.rs` `is_wasm_target`: `Wasip1 \| Wasip2` → `Wasip2` | killed — `a wasm target must claim no binaries` |
| `test_platform_pairs.py::test_unsupported_platform_pair_is_refused` | `ocx_oci/src/platform.rs` FromStr: `validate_pair(os, arch)` → `Ok(())` | killed — `the refusal for wasip1/amd64 must list the supported pairs` |
| `test_platform_pairs.py::test_supported_pair_reaches_resolution` | `ocx_oci/src/platform.rs:43`: `(Windows, Arm64)` removed from `SUPPORTED_PAIRS` | killed — `windows/arm64 is a supported pair and must parse` |
| `test_status.py::test_status_reports_unreadable_lock` | `api/data/status.rs` `unreadable_lock`: default-group tools → empty map | killed — `the declaration is still readable and must still be reported` |
| `test_status.py::test_status_names_the_selected_directory_when_it_holds_no_manifest` | `app/project_context.rs:87` `NoProjectIn` message gains "or any parent" | killed — `[flag] no walk happened, so none may be claimed` |
| `test_config_test.py::test_config_test_reports_unknown_keys_and_still_exits_zero` | `managed_config/preview.rs`: `unknown_keys.truncate(1)` after the sort | killed — `a typo'd key must be listed by its full path` |
| `test_config_test.py::test_config_test_rejects_oversize_payload_exit_78` | `exit/ocx_config.rs:151`: `PayloadTooLarge` classified 65 | killed — `left: ExitCode(65)` |
| `test_config_test.py::test_config_test_plain_http_mirror_allowed_by_candidate_registries_entry` | `ocx_config/src/insecure.rs:100`: the config `[registries] insecure` half dropped | killed — `config test must exit 0: ERROR mirror for 'ghcr.io' uses http://` |

## Not deleted (the originals stay)

| case(s) | why |
|---|---|
| `test_status.py::test_status_reports_drift_instead_of_refusing` | Partial port: the sibling's 65 is checked through `is_stale` plus a hand-built error, not the gate `ocx pull` runs (`project_context.rs:336`). Kept as an unmarked companion test in `command/status.rs`. |
| `test_config_test.py::test_config_test_writes_nothing` | R2: pushes a managed-config payload to a live registry to arm the tier it then proves untouched. |
| `test_config_test.py::test_ordinary_command_stays_silent_about_an_unknown_config_key` | Partial port (not landed): it ran `ConfigLoader::load`, not `ocx about`; `log::` records do not reach the seam's `err`. |
| `test_config_setup.py` — all 13 | 8 ports re-implemented `ConfigSetupArgs::execute` in the test (`config setup` is not an admitted seam verb) and skipped its exit-code mapping — an auditor mutation making every clean outcome exit 82 survived. 5 need a registry-refresh warning read through `log::` (not bridged by the seam) or the 64/82 exit mapping of an unadmitted verb. |
| `test_platform_pairs.py::test_wasm_package_publishes_and_installs` | R2: create → push → index → install through a live registry is the asserted path. |
| `test_execution_record_standards.py` — all 16 | 9 need a real `exec` record against a published package (R2/R3); 7 are self-tests of the Python reference validators (in-toto protobuf bindings, the ECS/OTel vocabulary check) — R8. Porting the positive cases needs those validators in Rust, i.e. a new dependency. |

## Deletion guard (C-007(e), `--tiered-shapes`)

Run post-commit on the port commit `d7c6469a` (the same tree as `b0ec8e81` after the history split) (clean tree, tip == HEAD). The guard ran
`task bazel:test:unit` itself at HEAD and read the fresh per-case `target/bazel/junit.xml`
(8362 `<testcase>` across 35 targets).

| run | command | exit | key line |
|---|---|---|---|
| green: the WP's range | `ocx exec -- task scripts:test-diff-guard BASE=864b9b80 -- --tiered-shapes` | 0 | `test-diff guard: 864b9b80..HEAD changes nothing the acceptance suite asserts` |
| red: the flag off | `ocx exec -- task scripts:test-diff-guard BASE=864b9b80` | 201 | `test-diff guard: 461 violation(s) in 864b9b80..HEAD under test/` |
| red: S-020, one port `#[ignore]`d (throwaway worktree at `d7c6469a` plus a probe commit) | same as green | 201 | `command::status::seam::status_rejects_selectors under //crates/ocx_cli:*_test was skipped, and an #[ignore]d port executes nothing — remove the #[ignore]`; the floor rule reds with it: `lowered by 45 (3445 -> 3400), but the sanctioned moves and ports remove 44` |

The S-020 probe also lowered that target's `TEST_TARGET_MAP.toml` floors and raised
`crates/NEXTEST_SKIP_CEILING` to 10. Without that, the unit lane's own floor and ceiling refuse
first (`task bazel:test:unit exited 201`), which is also a refusal but not the one under test.

`SUITE_FLOOR` 3445 → 3400: the decrease equals the 45 collected cases deleted. Collected with
`OCX_TESTS_NO_REGISTRY=1 pytest --collect-only`: `test_status` 11 → 1, `test_config_test` 27 → 2,
`test_platform_pairs` 12 → 2.

The default merge-base range (`task scripts:test-diff-guard` with no `BASE`, here
`2a8c663d..HEAD`) reds on edits this WP did not make: WP-08b's `test_doc_scripts_publish`,
`test_execution_records`, `test_project_env`, `test_taplo_project_toolchain` and
`test/doc_scripts/BUILD.bazel` re-points. None of its findings names a file this WP touched
(0 hits for the three pilot modules or `ported-from`).
