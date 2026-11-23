# P-ACC-CACHE red proof

Command: `cd test && OCX_COMMAND=<a0-ref-build>/target/debug/ocx uv run pytest tests/test_blob_cache.py tests/test_package_copy_from_export.py -q -p no:randomly` -> 10 failed, 0 passed (A0 stubs).

- test_concurrent_pulls_share_cache · red: `_assert_every_blob_verifies`, "cache holds no blob at all" · the eight pulls exit 0 but nothing is kept; the scenario's own cache assertion.
- test_warm_cache_zero_registry_requests · red: `len(blob_data_files(cache)) >= 3` is 0 · warm pull kept no metadata or layer; the scenario's warm-cache precondition (the zero-request step is never reached).
- test_missing_blob_dir_flag_is_79_env_is_skipped · red: flag run exits 0, expected 79 · A0 ignores `--blob-dir`; the scenario's own exit-code assertion.
- test_cache_flipped_byte_is_refetched · red: `max()` over an empty cache listing · the warm pull kept nothing to damage; the cache write is the scenario's subject.
- test_blob_dir_hit_is_not_copied_to_cache · red: pull with `--blob-dir` exits 75, expected 0 · the blob directory is not consulted and the guarded registry is dialled; the scenario's own exit assertion.
- test_planted_cache_blob_records_no_origin · red: pull from a populated cache with the registry refused exits 75, expected 0 · the cache is not read; the scenario's own exit assertion.
- test_project_tier_blobs_section_is_stripped · red: control `--blob-cache-dir` wrote nothing · the legitimate cache path writes nothing at A0, so the strip proves nothing yet.
- test_blob_dir_flag_reaches_child_ocx · red: child `ocx --offline package pull` exits 79, expected 0 · the directory never reaches the child; the scenario's own exit assertion.
- test_blob_dir_alone_never_resolves_a_tag · red: `package export` panics (`P-EXPORT: export`) · the export is the producer of the blob directory the scenario drives; its panic is a command under test, not setup of a different scenario.
- test_copy_from_export_by_tag_with_source_down · red: `package export` panics (`P-EXPORT: export`) · same as above.

Against the current branch binary (A0 plus the merged lookup/chain/pin/layer/copy pipelines): 6 failed, 4 passed. Green there: concurrent pulls, missing-dir flag/env, flipped-byte refetch, project `[blobs]` strip. Still red there: warm-cache metadata (only the layer is kept), blob-dir hit and planted cache with registry refused (exit 75; pull path does not yet consult the lookup), child inheritance, both export-backed tests.

## Gaps found
- Project-tier `[blobs]`: an `ocx.toml` with `[blobs]` is refused as unknown field (`ProjectConfig` denies unknown keys), and `fold_project_tier` has no caller outside its unit tests, so "pull warns once" is not reachable through the CLI. The test asserts the invariant that holds either way (nothing written at the redirected path, `blobs` named on stderr) plus a control proving the cache would otherwise be written. The orchestrator should decide whether refusal (78) or warn-and-strip is the contract.
- `OcxRunner.run` raises a conformance finding for any argv that carries `--blob-dir`/`--blob-cache-dir` (`cli.json` does not yet name the flags as value-taking root options) and for `error.detail = blob_dir_not_found` (missing from the error enum). The modules call `subprocess` directly for these; regenerate the contract files and these can move back to `ocx.run`.
- test_copy_from_export_by_tag_with_source_down reaches the target as `127.0.0.1` so `NO_PROXY=127.0.0.1` lets it bypass the refusing proxy that `localhost` (the source) goes through.
