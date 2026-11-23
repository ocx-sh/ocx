# P-PROOF-M1 — mutation evidence (V1, V2, V3, V4, V6, V8)

Green baseline for every row: HEAD 85f862d6, `cargo build -p ocx --features __testing --locked`, then
`cd test && OCX_COMMAND=<worktree>/target/debug/ocx uv run pytest <module>::<test> -q -p no:randomly` -> `1 passed` each.
Every mutation was a product-code edit proven landed by `git diff --stat` plus a grep, rebuilt, run once, then reverted (`git checkout -- crates`; `git status` clean).

## V1 `test_import_then_offline_install_by_tag`
- Mutation: `crates/ocx_index/src/local_index.rs` `import_tags`: the write guard `if !writes.is_empty()` -> `if !writes.is_empty() && false` (no pins written).
- Landed: `748:        if !writes.is_empty() && false {`
- Red: `pytest tests/test_package_import.py::test_import_then_offline_install_by_tag`
- Assertion: `second_home.json("--offline", "package", "install", pkg.short)` fails rc=81, "offline mode refused to resolve unpinned reference ...".
- Right reason: the import itself succeeded; the scenario's own install-by-tag step is what fails for lack of a pin.
- Green: 85f862d6, 1 passed.

## V2 `test_import_refuses_changed_byte`
- Mutation: `crates/ocx_oci/src/blob_lookup/stream.rs` `LookupWalk::reject`: dropped `self.bad_dir_copies += 1;` (a bad directory copy is not remembered, the walk ends as a plain miss).
- Landed: diff removes the line; `bad_dir_copies` remains only at the field, init and the `exhausted` check (lines 46, 58, 150).
- Red: `pytest tests/test_package_import.py::test_import_refuses_changed_byte`
- Assertion: `assert result.returncode == 65, _text(result)` fails; actual exit 79, detail `export_not_found` ("layer ... is not in the export").
- Right reason: the flipped-byte layer is reported absent rather than as a digest mismatch, which is exactly the assertion on 65 / `digest_mismatch`.
- Green: 85f862d6, 1 passed.

## V3 `test_import_one_name_pins_only_it`
- Mutation: `crates/ocx_index/src/export.rs` `write_export_index`: inserted `root = home_root.clone();` before the root write (the whole home root is copied, not just the exported tags).
- Landed: `85:    root = home_root.clone();`
- Red: `pytest tests/test_package_import.py::test_import_one_name_pins_only_it`
- Assertion: `assert _home_tags(second_home, repo_a) == {"1.0.0"}` -> `{'1','1.0','1.0.0','latest'} == {'1.0.0'}`.
- Right reason: the cascade tags in the source home leak into the importing home, which is the scenario's "pins only it" claim.
- Green: 85f862d6, 1 passed.

## V4 `test_import_verifies_from_carried_referrers` (`test_package_import_verify.py`)
- Step 1 (import-side, as the plan words it): `crates/ocx_package_manager/src/tasks/import.rs` `import_referrers`: marker write guard `if complete && ...` -> `if false && complete && ...` (import stores no referrer markers). Landed: `716:            if false && complete && ...`.
  Result: test stays green (`1 passed`). A second guard exists: verification during the pull reads the referrers straight from the export's `DIR/blobs`, so the home's markers do not gate it.
- Step 2 (the carried referrers unreachable): `crates/ocx_package_manager/src/tasks/export.rs` ~line 229: `referrers: request.referrers` -> `referrers: false` (the export writes none). Landed: `229:            referrers: false,`.
- Red: `pytest tests/test_package_import_verify.py::test_import_verifies_from_carried_referrers`
- Assertion: `assert ok.returncode == 0, "import must verify from carried referrers, got 79"`, stderr "no signatures found for target; referrers were not exported".
- Right reason: the with-referrers import is the scenario's first assertion; the carried referrers are gone so verification fails with 79.
- Green: 85f862d6, 1 passed.

## V6 `test_import_then_exec_without_export`
- Attempt 1: `crates/ocx_package_manager/src/tasks/pull.rs` `extract_layers`: spawn loop `.enumerate()` -> `.enumerate().take(0)` (no layer extracted). Landed: `861: ... .enumerate().take(0) {`. Red, but at the import setup (`_imported(_import(...))`, rc 74, "layer placement arity mismatch: 1 placements for 0 sources"), not at the scenario's assertion; rejected as wrong-reason.
- Final mutation: `crates/ocx_package_manager/src/tasks/import.rs` `import_leaf`: after the pull and referrers, replace the package `content` dir with an empty one (layers unextracted into the package, import still succeeds). Landed: `665:            std::fs::remove_dir_all(&content).unwrap();`.
- Red: `pytest tests/test_package_import.py::test_import_then_exec_without_export`
- Assertion: `run = second_home.plain("package", "exec", pkg.short, "--", "hello")` fails rc=79, "required path does not exist: .../content/bin".
- Right reason: the import succeeded; the exec after the export is removed is what fails.
- Green: 85f862d6, 1 passed.

## V8 `test_import_roots_nothing`
- Mutation: `crates/ocx_package_manager/src/tasks/import.rs` `import_leaf`: create a candidate symlink (`symlinks.candidate(leaf)` -> a temp path) for each imported leaf.
- Landed: `664:            let candidate = self.file_structure().symlinks.candidate(leaf);`
- Red: `pytest tests/test_package_import.py::test_import_roots_nothing`
- Assertion: `AssertionError: an import creates no candidate` (the `assert not (home / "symlinks").exists() or not any(... rglob("candidates"))` line).
- Right reason: the import ran and materialized packages; only the candidate check trips, before `clean`.
- Green: 85f862d6, 1 passed.
