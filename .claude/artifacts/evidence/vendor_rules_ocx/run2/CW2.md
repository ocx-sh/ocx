# CW2 evidence (run 2, contract wave, ocx)

Base `6afc82b3`. Built with `cargo` in a CW2-only target dir, 10 jobs. `task verify:scoped` was not run:
`scripts/scoped_gate.py --plan` escalates on this branch's whole diff against `origin/main`, so each step ran
`cargo fmt --all --check`, `cargo clippy -p <crate> --all-targets --locked -- -D warnings` and `cargo test -p <crate>`
per touched crate by hand (DP-39).

## Step 1 — `5e5343d9` `feat(vendor)!: the vendored launcher record is NAME.launch with a kind key`

`git diff --exit-code Cargo.lock`: clean (the three `windows-sys` features change no lock entry).

Red (14, `ocx_env_apply`, owner LA step 1), all `unimplemented!()` panics in `crates/ocx_env_apply/src/launch.rs`:

| Test (`launch::tests::`) | Failing line |
|---|---|
| `contract_decode_accepts_one_valid_record_per_kind` | `launch.rs:180` (`LaunchRecord::decode`) |
| `contract_decode_refuses_extra_key` | `launch.rs:180` |
| `contract_decode_refuses_a_key_of_a_sibling_kind` | `launch.rs:180` |
| `contract_decode_refuses_an_unknown_key_in_an_env_entry` | `launch.rs:180` |
| `contract_decode_refuses_a_missing_unknown_or_duplicate_kind` | `launch.rs:180` |
| `contract_decode_refuses_schema_version_other_than_one` | `launch.rs:180` |
| `contract_decode_refuses_more_than_one_mebibyte` | `launch.rs:180` |
| `contract_decode_refuses_a_nul_in_any_string` | `launch.rs:180` |
| `contract_peek_kind_ignores_every_other_key` | `launch.rs:228` (`peek_kind`) |
| `contract_peek_kind_is_none_without_a_valid_kind` | `launch.rs:228` |
| `contract_package_root_is_absolute_and_identifier_is_pinned` | `launch.rs:255` (`PackageRecord` validate) |
| `contract_token_outside_direct_is_refused` | `launch.rs:255` |
| `contract_toolchain_tier_mismatch_is_refused` | `launch.rs:278` (`ToolchainRecord` validate) |
| `contract_toolchain_ocx_is_absolute_and_not_a_batch_file` | `launch.rs:278` |

Green against real code: `contract_encode_is_byte_equal_to_the_four_examples` (V-L02), the two `record_path_for`
cases (V-L05), `contract_validate_refuses_a_struct_kind_other_than_the_peek`.
Suites: `ocx_env_apply` 61 pass / 14 red; `ocx_shim` 156 pass; `ocx_env` 93 pass; `ocx_package_manager` launcher,
vendor and render filters 274 pass.

## Step 2 — `1b98d4f6` `feat(vendor)!: vendor manifest schema version 2 types`

Includes the orchestrator's one-line follow-up in `crates/ocx_schema/src/lib.rs`: `vendor_manifest_version!()` 1 → 2
(the schema `$id` is now `vendor-manifest/v2.json`, const 2) and its pin test, because
`id_versions_equal_the_versions_their_owners_declare` compares it with `ocx_package::vendor::SCHEMA_VERSION`.

| Test | Failing line | Owner |
|---|---|---|
| `ocx_store` `file_structure::vendor_layout::tests::contract_package_keys_refuses_a_hex16_clash_naming_both` | `vendor_layout.rs:120` (`package_keys`) | VF step 1 |
| `ocx_store` `file_structure::vendor_layout::tests::contract_package_keys_keys_by_first_16_hex` | `vendor_layout.rs:120` | VF step 1 |
| `ocx_store` `file_structure::vendor_layout::tests::contract_check_relative_path_refuses_reserved_segment` | `vendor_layout.rs:369` | VF step 1 |
| `ocx_index` `store::wire_grammar_tests::contract_recorder_records_a_found_and_an_absent_path` | `store.rs:2790`, `left: []` | IX step 1 |

Green: the `example.json` round-trip (v2, short layout), the recorder survives `Clone`. `LookupRecorder::{new,
record(&self, relative_path, found), lookups() -> Vec<(String, bool)>}`: sorted by path, one entry per path.

## Step 3 — this commit `chore: launcher generation and publish contract stubs`

| Test | Failing line | Owner |
|---|---|---|
| `ocx_util` `fs::publish::tests::contract_publish_onto_absent_target_is_new` | `publish.rs:39` (`publish_dir`) | PB step 2 |
| `ocx_util` `fs::publish::tests::contract_publish_onto_empty_dir_is_new` | `publish.rs:39` | PB step 2 |
| `ocx_util` `fs::publish::tests::contract_publish_onto_non_empty_dir_is_already_there_and_keeps_staged` | `publish.rs:39` | PB step 2 |
| `ocx_util` `fs::publish::tests::contract_retire_removes_a_real_directory` | `publish.rs:61` (`retire_dir`) | PB step 2 |
| `ocx_util` `fs::publish::tests::contract_retire_leaves_a_symlink_alone` | `publish.rs:61` | PB step 2 |
| `ocx_util` `fs::publish::tests::contract_retire_sweeps_sibling_debris` | `publish.rs:61` | PB step 2 |
| `ocx_util` `fs::publish::tests::contract_retarget_creates_an_absent_link` | `publish.rs:51` (`retarget_link`) | PB step 3 |
| `ocx_util` `fs::publish::tests::contract_retarget_replaces_an_existing_link` | `publish.rs:51` | PB step 3 |
| `ocx_util` `fs::publish::tests::contract_retarget_refuses_a_real_directory` | `publish.rs:51` | PB step 3 |
| `ocx_util` `fs::publish::tests::contract_retarget_refuses_a_regular_file` | `publish.rs:51` | PB step 3 |
| `ocx_store` `launcher_generation::tests::contract_generation_of_the_empty_blob_matches_the_golden` | `launcher_generation.rs:21` (`of`) | PB step 1 |
| `ocx_store` `launcher_generation::tests::contract_host_follows_the_host_blob` | `launcher_generation.rs:21` | PB step 1 |
| `ocx_store` `launcher_generation::tests::contract_render_fingerprint_matches_the_golden` | `launcher_generation.rs:42` | PB step 1 |
| `ocx_store` `file_structure::toolchain_store::tests::contract_active_target_name_reads_the_link_and_is_none_when_absent` | `toolchain_store.rs:205` | PB step 3 |

Green: the C-126 characterization (`TempStore::scratch` moved from `export_archive.rs`), the new path accessors, the v2
stamp round-trip and unknown-`v`-reads-absent, `is_record_name`, `CleanKind::Launchers` display.
`crates/ocx_test_support/tests/workspace_structure.rs`: `publish_dir`, `retarget_link`, `retire_dir` added to
`OCX_UTIL_WITHOUT_CONSUMER` (the `with_level` precedent); the first caller of each must drop its entry or
`every_public_item_of_ocx_util_has_a_consumer` reds on the departure.

## Wave gate (after step 3)

- `cargo check --workspace --all-targets --locked`: exit 0.
- `cargo test -p ocx_test_support --test workspace_structure`: 49 passed, 0 failed.
- Unit suites, `cargo test -p <crate> --locked --no-fail-fast`: `ocx_shim`, `ocx_env`, `ocx_package`,
  `ocx_package_manager` green; `ocx_env_apply` 61/14, `ocx_util` 293/10, `ocx_index` 536/1, `ocx_store` 403/7 — every
  failure is a contract test of the tables above; `ocx` lib 1818/1, the one being
  `app::seam::tests::seam_installs_no_global_subscriber_and_logs_to_err`, green when run alone (parallel-run flake).
- `ocx_schema`: `vendor_manifest_schema_matches_golden`, `reports_schema_matches_golden`, `cli_document_matches_golden`
  red on golden drift (`vendor-manifest` v2, `CleanKind::Launchers`, `__OCX_LAUNCHER_DEPTH`): hub files, regenerated by I2.

Not run: Bazel lanes, acceptance (`test/tests/**` still names `.direct`; red until act B and the pipelines that own
those modules), Windows and macOS legs.
