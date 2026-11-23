# P-PROOF-M2 mutation evidence

Green baseline: HEAD 85f862d6, each test run alone against `cargo build -p ocx --features __testing --locked`: 1 passed (V7, V9, V10, V16, V17, V18 acceptance; unit `lookup_hit_records_no_origin` 1 passed under nextest).
Red command shape: `cd test && OCX_COMMAND=<wt>/target/debug/ocx uv run pytest tests/<module>.py::<test> -q -p no:randomly`.
All mutations reverted (`git checkout -- crates`, clean status).

## V7 `test_import_then_project_exec_survives_clean` (`test_project_export_import.py`)
- Mutation: stop every project-ledger registration. Three sites were needed:
  `toolchain_exec.rs` (`if !context.global() && false`), `tasks/import.rs::import_project`
  (`request.project_dir.as_ref().filter(|_| false)`), `tasks/render_toolchain.rs` (`if !request.dry_run && false`).
- Landed: `git diff` showed all three; each rebuilt (`Finished`).
- Steps: exec-site alone green; exec + import green (second guard: import registration); exec + import + render
  green-to-red only once render_toolchain's `ProjectRegistry::register` was also off (third guard, the per-project render).
- Red: `assert second.returncode == EXIT_SUCCESS` -> "exec after clean must still run offline; rc=75" (503 from the denied registry).
- Right reason: the failure is the post-`ocx clean` exec in the scenario, i.e. the unregistered project lost its tools to GC; setup and the first exec passed.

## V9 `test_import_no_names_pins_every_tag` (`test_package_import.py`)
- Mutation: `tasks/import.rs::ExportDir::plan` unnamed branch enumerates one source.
- Step 1: `.take(1)` on `self.roots` -> green (the scenario exports one package, so first-root-only is equivalent here).
- Step 2: also `.take(1)` on `root.tags` (first tag only). Landed: `grep -n "take(1)"` lines 364, 371.
- Red: `assert set(rows) == {"1.0.0", "2.0.0"}` -> `{'1.0.0'} == {'1.0.0', '2.0.0'}`. The test pins per-tag, so the tag-level cut is its own assertion; a root-level cut would need a two-package scenario (design note, not a defect: the row's root-count mutation is unobservable by this test).

## V10 `test_import_platform_present_and_missing`
- Mutation: missing-platform `ExportNotFound` message omits the platform at both sites,
  `tasks/import.rs` line 659 (`import_leaf`, the live path) and `select_leaves` line 809.
- Step 1: `select_leaves` site alone -> green (not the path the scenario takes). Step 2: both -> red.
- Landed: `git diff` showed `platform of '{leaf}'` / `platform of '{identifier}'`.
- Red: `assert host in _text(missing), "the error names the platform the export lacks"` (`'linux/amd64' not in` the error text).
- Right reason: the exit code and `export_not_found` detail asserts before it passed; only the platform text is missing.

## V16 `test_import_conflict_and_named_replace`
The tag decision lives in `tasks/import.rs::plan_tags` (`if named`), not in `ocx_index::import_tags`, which only writes the plan.
- (a) `if named || true` (moves a pin without a name). Red: `assert rows["1.0.0"]["status"] == "conflict"` -> `'replaced' == 'conflict'`.
- (b) `if named && false` (name ignored, never replaces). Red: `assert rows["1.0.0"]["status"] == "replaced"` -> `'conflict' == 'replaced'`.
- First attempt `if true` / `if false` did not compile (unused `named`, warnings denied): the earlier "passed" lines were a stale binary, discarded.
- Right reason: each is the scenario's own status assertion on the unnamed (a) or named (b) import.

## V17 `test_root_import_patch_snapshot_round_trip`
- Mutation: `tasks/export.rs` `if request.packages.is_empty() && false` skips `export_snapshot` (companions and descriptors).
- Landed: `git diff` line `if request.packages.is_empty() && false {`.
- Red: `assert imported.returncode == EXIT_SUCCESS` -> "error: patch companion '...' is not in the export" (rc 79).
- Right reason: the scenario's offline import asserts, after a successful export (setup assert passed).

## V18 `test_planted_cache_blob_records_no_origin` and unit `lookup_hit_records_no_origin`
- Mutation: `tasks/pull.rs` `if from_registry && resolved.leaf_origin == BlobOrigin::Network {` -> `if from_registry {`.
- Landed: `grep -n "if from_registry {"` line 571.
- Red (acceptance): `assert not origins.exists() or list(origins.iterdir()) == []` (an origin record `refs/origins/<id>` appeared).
- Red (unit): `cargo nextest run -p ocx_package_manager lookup_hit_records_no_origin` -> FAIL, panicked at `tasks/pull.rs:3215`.
- Right reason: setup plants the blob and pulls; the assertion is the origin-record absence itself.
