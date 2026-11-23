# Evidence: package export and import, red and green

This file proves that every Track 1 acceptance test of [the discussion's Verification section](../../.agents/discussions/bazel-hermetic-package-bundles.md) is shown red once and green. Two red sources are used: the A0 stub binary, where the command under test panics with `P-EXPORT` todo, and product-code mutations, each proven landed by `git diff` and a grep before the run.

Sources: per-pipeline runs in [P-ACC-EXP](evidence/package_export_import/P-ACC-EXP.md), [P-ACC-IMP](evidence/package_export_import/P-ACC-IMP.md), [P-ACC-VERIFY](evidence/package_export_import/P-ACC-VERIFY.md), [P-ACC-CACHE](evidence/package_export_import/P-ACC-CACHE.md), [P-ACC-PROJ](evidence/package_export_import/P-ACC-PROJ.md); mutation runs in [P-PROOF-M1](evidence/package_export_import/P-PROOF-M1.md), [P-PROOF-M2](evidence/package_export_import/P-PROOF-M2.md), [P-PROOF-M3](evidence/package_export_import/P-PROOF-M3.md). Spikes: [SPK-INDEX](evidence/package_export_import/SPK-INDEX.md), [SPK-OCI](evidence/package_export_import/SPK-OCI.md), [SPK-SIGN](evidence/package_export_import/SPK-SIGN.md). Harness: [P-HARNESS](evidence/package_export_import/P-HARNESS.md). Row list: [plan Track 1 section](plan_package_export_import.md).

Mutation red command shape, all rows: `cd test && OCX_COMMAND=<worktree>/target/debug/ocx uv run pytest <test id> -q -p no:randomly`. Green baseline for every row: HEAD `85f862d6`, `cargo build -p ocx --features __testing --locked`, `1 passed`.

## Track 1 acceptance tests

| V | Discussion test (short) | Test id | Red (mutation, as recorded) | Failing assertion (shortened) | Why it is the right reason | Green |
|---|---|---|---|---|---|---|
| V1 | Export, import into empty home, no registry, install by tag | `tests/test_package_import.py::test_import_then_offline_install_by_tag` | `import_tags` write guard `&& false` (no pins) | install rc=81 "offline mode refused to resolve unpinned reference" | Import succeeded; the install-by-tag step fails for lack of a pin | 85f862d6, 1 passed |
| V2 | One changed byte refused, exit 65 | `tests/test_package_import.py::test_import_refuses_changed_byte` | `LookupWalk::reject` drops `bad_dir_copies += 1` | `assert result.returncode == 65` got 79 (`export_not_found`) | Flipped layer is reported absent, not as digest mismatch | 85f862d6, 1 passed |
| V3 | Import for one package writes no pin for another | `tests/test_package_import.py::test_import_one_name_pins_only_it` | `write_export_index` copies whole home root | `assert _home_tags(...) == {"1.0.0"}` got `{'1','1.0','1.0.0','latest'}` | Cascade tags leak into the importing home | 85f862d6, 1 passed |
| V4 | Trust policy verifies offline from carried referrers; fails after `--no-referrers` | `tests/test_package_import_verify.py::test_import_verifies_from_carried_referrers` | export `referrers: request.referrers` set to `false` | `assert ok.returncode == 0` got 79, "referrers were not exported" | The carried referrers are gone, so verification fails with 79 | 85f862d6, 1 passed |
| V6 | Import, delete export, block network: `ocx exec` runs | `tests/test_package_import.py::test_import_then_exec_without_export` | `import_leaf` empties the package `content` dir | `package exec` rc=79, "required path does not exist: .../content/bin" | Import succeeded; exec after export removal fails | 85f862d6, 1 passed |
| V7 | Same with a project, `ocx clean`, `ocx exec` offline ([ocx-sh/ocx#759](https://github.com/ocx-sh/ocx/issues/759)) | `tests/test_project_export_import.py::test_import_then_project_exec_survives_clean` | project registration off at three sites | `assert second.returncode == EXIT_SUCCESS` rc=75 (denied registry, 503) | Post-clean exec of the unregistered project fails; setup and first exec passed | 85f862d6, 1 passed |
| V8 | Import, no install, no project, `ocx clean`: packages and metadata removed | `tests/test_package_import.py::test_import_roots_nothing` | `import_leaf` creates a candidate symlink | `AssertionError: an import creates no candidate` | Only the candidate check trips, before `clean` | 85f862d6, 1 passed |
| V9 | No names pins every tag; one name pins only it | `tests/test_package_import.py::test_import_no_names_pins_every_tag` | `ExportDir::plan` unnamed branch `take(1)` on roots and tags | `assert set(rows) == {"1.0.0", "2.0.0"}` got `{'1.0.0'}` | Tag-level cut is the test's own assertion | 85f862d6, 1 passed |
| V10 | `--platform` held assembles; missing one fails naming it | `tests/test_package_import.py::test_import_platform_present_and_missing` | missing-platform `ExportNotFound` message omits platform | `assert host in _text(missing)`: `'linux/amd64'` not in error | Exit code and detail asserts passed; only the platform text is missing | 85f862d6, 1 passed |
| V11 | Registries `a` and `b`, same repository path, stay apart | `tests/test_package_export.py::test_export_keeps_registries_apart` | blob writer registry slug replaced by constant `"shared-registry"` | `assert _blob(out, a, leaves[a]).is_file()` -> `AssertionError: assert False` | Setup passed; failing line is per-registry placement | 85f862d6, 1 passed |
| V12 | Concurrent pulls into one empty cache | `tests/test_blob_cache.py::test_concurrent_pulls_share_cache` | `persist_temp_file` plus unverified fast path; then `append` straight into the final path | `_assert_every_blob_verifies` -> `assert actual == hex_digest` (blob corrupt) | Pulls succeed; the verification assertion is the failing line | 85f862d6, 1 passed |
| V13 | Warm cache: zero registry requests, counted | `tests/test_blob_cache.py::test_warm_cache_zero_registry_requests` | `write_cache` guard `if false` (metadata not cached) | `assert len(blob_data_files(cache)) >= 3` -> `assert 1 >= 3` | Cache content claim fails; zero-request step not reached | 85f862d6, 1 passed |
| V14 | No `--platform` holds every platform; two hold exactly two | `tests/test_package_export.py::test_export_platform_filter` | `resolve_targets` keeps only the first platform (`take(1)`) | `assert {row["platform"] ...} == {"linux/amd64", "darwin/arm64"}` -> `{'linux/amd64'}` | Unfiltered export passed; two-platform filter fails | 85f862d6, 1 passed |
| V15 | Interface gate: `cli.json`, report goldens, contract rows, `SUITE_FLOOR` | `crates/ocx_schema/tests/golden_schemas.rs::reports_schema_matches_golden` (with the other 9 goldens, `cli_document_matches_golden` among them); `task website:reference:check` | `ImportReport` root deleted from `tests/golden/reports.json` (byte-identical JSON round trip otherwise), restored after | `reports schema differs from its golden at byte 1116 (398962 generated vs 398892 golden bytes)` | Additive against the v0.6.8 baseline (no `export`/`import` roots there), so no ledger entry; `SUITE_FLOOR` 4507 unchanged | fec09779: `cargo test -p ocx_schema --test golden_schemas` 10 passed; bazel `//crates/ocx_schema:golden_schemas` 10 passed in `task verify:scoped --force` (exit 0); `website:reference:check` exit 0 |
| V16 | (ADR) No-names conflict kept, named package moves | `tests/test_package_import.py::test_import_conflict_and_named_replace` | (a) `if named \|\| true`; (b) `if named && false` | (a) `status == "conflict"` got `'replaced'`; (b) `status == "replaced"` got `'conflict'` | Each is the scenario's own status assertion for the unnamed or named import | 85f862d6, 1 passed |
| V17 | Project export with patch snapshot: composed env offline equals connected | `tests/test_project_export_import.py::test_root_import_patch_snapshot_round_trip` | export skips snapshot when `packages.is_empty() && false` | `assert imported.returncode == EXIT_SUCCESS` rc=79, "patch companion ... is not in the export" | Setup export passed; offline import fails | 85f862d6, 1 passed |
| V18 | (ADR) Planted cache file writes no origin | `tests/test_blob_cache.py::test_planted_cache_blob_records_no_origin` | `pull.rs` origin gate `if from_registry` (drops `== Network`) | `assert not origins.exists() or list(origins.iterdir()) == []` (origin record appeared) | Setup plants and pulls; origin-record absence is the assertion. Unit `lookup_hit_records_no_origin` also red at `tasks/pull.rs:3215` | 85f862d6, 1 passed |

V5 is dropped by the owner's 2026-11-22 format reversal (`oras cp` reads the bundle). V15 is the interface gate, filled from the integration gate at fec09779.

**Extra guards and notes**

- **V4:** the import-side mutation (`if false && complete`, no referrer markers) stays green. Verification reads referrers from the export's `DIR/blobs`, so the home markers do not gate it. The red comes from the export-side `referrers: false`.
- **V6:** the first mutation (`take(0)` on the extract loop) reds at import setup (rc 74, arity mismatch), the wrong reason, and was rejected. The row uses the second mutation.
- **V7:** three registration sites were needed. Exec alone and exec plus import stayed green; red only once `render_toolchain` registration was also off.
- **V9:** the root-level `take(1)` is green (one-package scenario). The tag-level cut reds. A root-level cut needs a two-package scenario.
- **V10:** the `select_leaves` site alone is green; red needs both sites.
- **V12:** mutation 1 alone is 0/20 red in acceptance (the unit test `concurrent_writers_converge` is 10/10 red). Mutation 2 is 20/20 red in acceptance. The publish property is held by the unit test.
- **V16:** the decision lives in `tasks/import.rs::plan_tags`. Both directions red.

## Other scenario tests (A0 red run)

Command: `OCX_COMMAND=<a0-ref-build>/target/debug/ocx uv run pytest <module> -q -p no:randomly`. Every row reds on the A0 stub. "Export panic" is `not yet implemented: P-EXPORT: export` (rc 101).

| Test id | A0 red line | Reason |
|---|---|---|
| `tests/test_package_export.py::test_export_tagged_package_layout` | `_export` check=True, rc=101 | Export is the scenario's step |
| `tests/test_package_export.py::test_export_bare_identifier_exports_latest` | first `_export`, rc=101 | Same |
| `tests/test_package_export.py::test_export_slug_collision_refused` | `assert result.returncode == 65` got 101 | Scenario's own exit assertion |
| `tests/test_package_export.py::test_export_commits_first_sight_pin` | `_export` rc=101 | Export is the step |
| `tests/test_package_export.py::test_offline_export_of_unknown_tag_is_81` | `assert result.returncode == 81` got 101 | Scenario's own exit assertion |
| `tests/test_package_export.py::test_export_report_fields` | first `_export` rc=101 | Export is the step |
| `tests/test_package_import.py::test_first_sight_import_uses_identity_pointer` | `ocx package export` rc 101, export panic | Stub export is the setup step |
| `tests/test_package_import.py::test_reexport_adds_platform` | same | Same |
| `tests/test_package_import.py::test_import_digest_identifier_writes_no_pin` | same | Same |
| `tests/test_package_import.py::test_import_report_fields` | same | Same |
| `tests/test_package_import.py::test_import_truncated_layer_is_65` | same | Same |
| `tests/test_package_import.py::test_import_corrupt_root_is_invalid` | same | Same |
| `tests/test_package_import.py::test_import_missing_layer_is_export_not_found` | same | Same |
| `tests/test_package_import.py::test_import_without_marker_or_index_is_invalid` | `assert no_marker.returncode == 65` (line 618) got 101 | Import panics on stub; own assertion |
| `tests/test_package_import_verify.py::test_import_enforces_policy_identity_from_carried_referrers` | `assert exported.returncode == 0` (line 72) rc 101, export panic | Stubbed export is setup; import assertions not reached |
| `tests/test_blob_cache.py::test_missing_blob_dir_flag_is_79_env_is_skipped` | flag run exits 0, expected 79 | A0 ignores `--blob-dir`; own assertion |
| `tests/test_blob_cache.py::test_cache_flipped_byte_is_refetched` | `max()` over empty cache listing | Warm pull kept nothing; cache write is the subject |
| `tests/test_blob_cache.py::test_blob_dir_hit_is_not_copied_to_cache` | pull with `--blob-dir` exits 75, expected 0 | Blob directory not consulted; own assertion |
| `tests/test_blob_cache.py::test_project_tier_blobs_section_is_stripped` | control `--blob-cache-dir` wrote nothing | A0 writes no cache, so the strip proves nothing yet |
| `tests/test_blob_cache.py::test_blob_dir_flag_reaches_child_ocx` | child `ocx --offline package pull` exits 79, expected 0 | Directory never reaches child; own assertion |
| `tests/test_blob_cache.py::test_blob_dir_alone_never_resolves_a_tag` | `package export` panic | Export produces the blob directory; command under test |
| `tests/test_package_copy_from_export.py::test_copy_from_export_by_tag_with_source_down` | `package export` panic | Same |
| `tests/test_project_export_import.py::test_global_export_import_registers_nothing` | `--global export` exit 101 | Export is the command under test |
| `tests/test_project_export_import.py::test_root_export_smoke` (`smoke`) | `_export` exit 101 | Export is the command under test |
| `tests/test_project_export_import.py::test_root_import_smoke` (`smoke`) | `_export` exit 101 | Setup step; import not reached |

## Guards

A guard is green on A0 because clap or the existing load path already enforces the check. Its red comes from a product mutation (`P-PROOF-M2`, `P-PROOF-M3`).

- **`tests/test_package_export.py::test_export_requires_output_and_package`.** Dropping `required = true` on `-o` stays green, since `PathBuf` is still required by clap (second guard). Dropping both (`default_value = "."`) reds at `assert no_output.returncode == 64` (export wrote into the working directory). Dropping `required = true` on the `PACKAGE` positional reds at `assert no_package.returncode == 64` (got 0).
- **`tests/test_project_export_import.py::test_root_export_grammar_errors`.** Removing the outside-a-project check reds at `assert outside.returncode == EXIT_USAGE` (got 78). Removing the missing-lock check reds at `assert missing.returncode == EXIT_CONFIG` (got 64).

Row counts: 17 Track 1 rows (16 test-backed plus V15 integration gate; V5 dropped), 25 other scenario tests, 2 guards.
