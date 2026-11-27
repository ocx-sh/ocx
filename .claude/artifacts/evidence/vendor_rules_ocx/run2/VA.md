# VA evidence (run 2, vendor acceptance, ocx)

Base `333a5264`. `ocx` built from the base with `cargo build -p ocx --locked` (target `run2/target-VA`, 2 jobs, no debug info).
Runs: `test/.venv` pytest per module, serial, `OCX_HOME`/`HOME` under `run2/VA/`, `OCX_NO_CONFIG=1`, `OCX_COMMAND` the built
binary, the suite's own zot (`OCX_TEST_ZOT` from the `ocx` checkout's toolchain, because the `uno` checkout's link is dangling).
No docker. `task verify:scoped` was not run: `scripts/scoped_gate.py --plan` routes a Bazel lane (rc-file rule, DP-39).
`ruff check` and `ruff format --check` are clean on the changed files.

The tests are red by design: product code lands in VF (and the launcher blob round, act B). PF shows them green.

## Step 1 — `f287538f` `test(vendor): vendored trees on the short layout with launch records`

Files: `test/tests/{test_vendor,test_package_vendor,test_vendor_launcher,test_vendor_parity}.py`, `test/src/vendor_fixtures.py`.
New helpers in `vendor_fixtures.py`: `hex16(digest)`, `assert_short_layout(out, manifest)` (layout, record files with `kind`
`direct` and `schema_version` 1, environment paths; shown red on five planted faults of a synthetic tree), `reserved_segment_package`,
`launch_named_package`.

| Module | Passed | Failed | Skipped |
|---|---|---|---|
| `test_vendor` | 27 | 11 | 0 |
| `test_package_vendor` | 12 | 4 | 0 |
| `test_vendor_launcher` | 2 | 8 | 5 |
| `test_vendor_parity` | 4 | 4 | 1 |

| Row | Red test(s) | Failing line |
|---|---|---|
| V-L50, V-L53 | `schema_version_2` pair, `closure_and_entrypoints_land_in_the_short_layout`, `wasm_tool_without_a_launcher`, the old-path tests (`test_vendor_output_carries_no_bazel_file_or_term`, `test_setuid_bit_is_cleared_and_modes_and_mtimes_are_normalised`, `test_package_vendor_writes_manifest_content_and_launchers`, `test_package_vendor_wasm_package_has_files_and_no_launcher`) | `test_vendor.py:296`, `test_package_vendor.py:144` (`schema_version == 2`, top level `{manifest.json, l, p}`); `vendor_fixtures.py:129` (`content_dir == p/<HEX16>/c`) |
| V-L56 | `package_path_segment_ocx_reserved[exact]`, `[ascii_case_folded]`; `package_vendor_refuses_reserved_segment[reserved_segment]` | `test_vendor.py:422`, `test_package_vendor.py:177`: `assert 0 == 65` |
| V-L27 (vendor half) | `tool_name_ending_in_launch[control_other_suffix]` | `test_vendor.py:456`: `KeyError: 'record_path'` |
| V-L50 parity | `test_selection_environment_equals_ocx_env_report`, `test_entrypoint_environment_equals_package_env_self`, `test_launcher_child_matches_ocx_exec_child` | old `.../content` against `p/<HEX16>/c` |
| golden | `test_windows_amd64_manifest_equals_golden` | golden is PF's; the test masks `p/<16 hex>` as `p/@HEX16@` on both sides |
| launcher start | eight `test_vendor_launcher.py` cases | `ocx-shim: sidecar not found … .direct` (embedded blob predates act B) |

Green against the base: `test_tool_name_ending_in_launch_is_refused_with_65[tool]`, `[tool_case_folded]` and the `launch_name` case of
`test_package_vendor.py`: today's binary already refuses `x.launch` with 65 `vendor_path_unrepresentable`. The entry-point half of
C-122 has no acceptance case: `package create` refuses an entry-point name with a dot, so no such package can be built.
Not run: `test_launcher_starts_on_image_without_a_shell` needs `gcr.io/distroless/static`, not present on this host.

## Step 2 — `795f5d58` `test(vendor): short layout, record paths and manifest v2 fields`

Files: `test/tests/test_vendor.py`, `test/tests/test_package_vendor.py` (`_pvendor` gained `root=` so `--index` precedes `package`).

| Module | Passed | Failed | Skipped |
|---|---|---|---|
| `test_vendor` | 31 | 13 | 0 |
| `test_package_vendor` | 15 | 5 | 0 |

| Row | Red test | Failing line |
|---|---|---|
| V-L52 | `test_declared_binary_missing_from_content_is_refused_with_65_naming_the_claim` | `test_vendor.py:1156`: `assert unique_repo in message` (exit 65 and the slug already hold) |
| V-L55 | `test_index_lookups_list_every_path_read_sorted_unique_and_relative` | `test_vendor.py:1228`: `KeyError: 'index_lookups'` |
| V-L54 | `test_index_digest_is_on_a_root_resolved_through_an_index_and_on_no_dependency` | `test_package_vendor.py:483`: `KeyError: 'index_digest'` |

C-121 fragments asserted on `error.message`, case-sensitive: the repository name, the tag `1.0.0`, `binaries`, `corepack`,
`does not hold`, `publisher`.

Green against the base (behaviour already present): `test_bare_platform_manifest_vendors_for_the_requested_platform` (C-120, V-L59),
`test_platform_manifest_identifier_carries_no_index_digest`, `test_declared_binary_missing_from_content_does_not_stop_a_manifest_only_run`
(S-108), `test_index_lookups_are_absent_without_index`.

### V-L60 — residue

`test_vendor_with_index_leaves_the_index_directory_byte_identical[filled]`, `[lacks_the_package]` and the digest-addressed
`package vendor` case are green and were never seen red. A retry ran 13 manual probes (fresh homes, tag, platform manifest digest and
image index digest requests, `--remote --index`, a package with a pinned dependency, `vendor` and `package vendor`): `DIR` was
unchanged in every one. Control: `ocx --index DIR package pull` does write `<registry>/p/<repo>.json` and the dispatch object.
Cause: `persist_dispatch` (`crates/ocx_index/src/local_index.rs:373`) writes only when an image index is fetched, and
`place_package` receives identifiers already pinned to platform manifests. `verify_selection` with a trust policy was not probed.
PF proves the row by mutation (`place_package` / `verify_selection`).
