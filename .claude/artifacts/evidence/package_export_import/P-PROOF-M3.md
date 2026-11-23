# P-PROOF-M3 mutation evidence

Base: green at `85f862d6` (unmutated build, `cargo build -p ocx --features __testing --locked`), all six tests below: `6 passed`.
Red command shape: `cd test && OCX_COMMAND=<worktree>/target/debug/ocx uv run pytest tests/<module>.py::<test> -q -p no:randomly`.
Every mutation was reverted with `git checkout -- crates`; `git status` shows no product change.

## V11 `test_export_keeps_registries_apart` (`test_package_export.py`)
- Mutation: `crates/ocx_package_manager/src/tasks/export.rs` (both `BlobWriter::root_for` calls in the blob writer): the registry slug is replaced by the constant `"shared-registry"`, so blobs no longer key by registry.
- Landed: `634:  let writer = BlobWriter::root_for(&self.out_blobs, "shared-registry", digest)` and the same at line 654.
- Red assertion: `assert _blob(out, a, leaves[a]).is_file()` -> `AssertionError: assert False` (no `blobs/<a slug>/...` data file).
- Right reason: setup (two registries, two leaf digests, report with both identifiers) passed; the failing line is the scenario's own per-registry placement assertion.
- Green: 85f862d6, 1 passed.

## V12 `test_concurrent_pulls_share_cache` (`test_blob_cache.py`)
- Mutation 1 (`crates/ocx_oci/src/blob_lookup/writer.rs`): `BlobWriter::finish` publishes with `persist_temp_file` (replace) instead of `persist_temp_file_noclobber`, and `inspect_occupant` returns `Occupant::Verified` without hashing (`if true { return Ok(Occupant::Verified); }`).
  Landed: `git diff` shows both lines. Acceptance, 20 runs: 0/20 red (an atomic rename-over of identical, hashed bytes leaves a correct blob; no reader exists in this test that could observe a difference). Unit `writer::tests::concurrent_writers_converge`, 10 runs: 10/10 red, `assertion left == right failed, left: 16` at the `Written` count (`writer.rs:589`, expected 1).
- Mutation 2 (escalation, on top of mutation 1): `BlobWriter::write` also appends every chunk straight to `<blob dir>/data` (create+append), and `finish` no longer publishes a temp file (`drop(temp)`), i.e. the final path is written in place with no temp file.
  Landed: `git diff` shows `OpenOptions::new().create(true).append(true).open(dir.join("data"))` in `write` and `drop(temp); Ok::<PersistOutcome, _>(PersistOutcome::Published)` in `finish`.
  Acceptance, 20 runs: 20/20 red. Red assertion: `tests/test_blob_cache.py:139  _assert_every_blob_verifies(cache)` -> `test_blob_cache.py:50  assert actual == hex_digest, "<data> does not hash to its recorded digest"` (eight writers append to one file, the blob is corrupt). Unit `concurrent_writers_converge`, 10 runs: 10/10 red, `left: 15, right: 1`.
- Right reason: the pulls succeeded (failure list empty); the failing line is the blob-verification assertion.
- Green: 85f862d6, 1 passed (unit green on the same SHA).
- Finding: mutation 1 alone is not caught by the acceptance test; the property it defends (no unverified or non-atomic publish) is held by the unit test and, for corruption, by the acceptance verification.

## V13 `test_warm_cache_zero_registry_requests` (`test_blob_cache.py`)
- Mutation: `crates/ocx_index/src/chained_index.rs::write_cache`: guard made `if false && let Some(cache) = ...`, so manifests/config/index blobs are never written to the cache; layer tee (client.rs `open_tee`) is untouched.
- Landed: `419:        if false`.
- Red assertion: `assert len(blob_data_files(cache)) >= 3` -> `assert 1 >= 3` (only the layer is in the cache).
- Right reason: the warm pull returned 0; the cache content check is the scenario's "holds metadata as well as layers" claim. (The zero-request assertion after it was not reached.)
- Green: 85f862d6, 1 passed.

## V14 `test_export_platform_filter` (`test_package_export.py`)
- Mutation: `crates/ocx_package_manager/src/tasks/export.rs::resolve_targets`: `platforms.to_vec()` -> `platforms.iter().take(1).cloned().collect()` (second `-p` ignored).
- Landed: `823:        platforms.iter().take(1).cloned().collect()`.
- Red assertion: `assert {row["platform"] for row in rows} == {"linux/amd64", "darwin/arm64"}` -> `{'linux/amd64'} == {...}`, missing `darwin/arm64`.
- Right reason: the unfiltered export before it passed (setup); the failing line is the two-platform filter result.
- Green: 85f862d6, 1 passed.

## Guard `test_export_requires_output_and_package` (`test_package_export.py`)
- Mutation 1: `crates/ocx_cli/src/command/package_export.rs`: `required = true` dropped from `-o`. Landed (`17: #[arg(short = 'o', long = "output", value_name = "DIR")]`). Test stayed green: the field is a non-`Option` `PathBuf`, clap still requires it (second guard).
- Mutation 2: `-o` given `default_value = "."` (both guards gone). Red: `assert no_output.returncode == 64` -> the export succeeded and wrote into the working directory. Right reason: the first assertion of the scenario. (The stray `test/blobs`, `test/index`, `test/ocx-export.json` it wrote were deleted.)
- Mutation 3: `required = true` dropped from the `PACKAGE` positional (a `Vec`, so no second guard). Red: `assert no_package.returncode == 64` -> `assert 0 == 64`, report with `"packages": []`.
- Green: 85f862d6, 1 passed.

## Guard `test_root_export_grammar_errors` (`test_project_export_import.py`)
- Run A, outside-a-project check removed: `crates/ocx_cli/src/app/project_context.rs`: `None => return Err(no_project(context, cwd))` replaced by `LockCurrency::Missing` (the sed also hit the same text in a second loader in that file). Landed: `git diff` shows both. Red: `assert outside.returncode == EXIT_USAGE` -> `got 78`.
- Run B, no-lock check removed: `project_context.rs:197` `LockCurrency::Missing` replaced by `no_project(context, cwd)`. Landed: `git diff`. Red: `assert missing.returncode == EXIT_CONFIG` -> `got 64`.
  Variant tried first: `Missing` replaced by `LockCurrency::Stale`: red too (`got 65`, not 78), so the test tells the missing-lock verdict apart from the stale one.
- Green: 85f862d6, 1 passed.
