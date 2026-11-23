# R1-ACC evidence

Binary: `cargo build -p ocx --features __testing --locked`, run with `OCX_COMMAND=<worktree>/target/debug/ocx`.
Hostile-export reds: one build with four guards neutered at once (`ensure_source_contained` in crates/ocx_index/src/store.rs, the slug check in crates/ocx_oci/src/blob_lookup/export_marker.rs, the non-regular-file refusal in `check_index_tree` and the post-filter referrer cap in crates/ocx_package_manager/src/tasks/import.rs), then reverted with `git checkout -- crates`.

test_package_import.py::test_import_refuses_symlinked_index_dir · red: :162 `assert 0 == 65` (via :776) · the import follows the linked index and succeeds
test_package_import.py::test_import_refuses_traversal_registry_name · red: :162 `assert 0 == 65` (via :795) · the `..` registry is read instead of refused
test_package_import.py::test_import_refuses_marker_key_that_is_not_the_slug · red: :814 `assert 'slug' in ...` · a second guard (index dir not named by marker) still refuses with 65, so only the slug wording reds; two guards defend one refusal
test_package_import.py::test_import_junk_referrer_markers_do_not_hide_a_real_one · red: :850 `assert blob_path(...).is_file()` · raw-name counting pushes the real referrer past 256
test_blob_cache.py::test_guard_counts_a_cold_lock_pinned_pull / test_guard_counts_an_install_an_import_cannot_serve · guard (red control): assert count > 0 on a run that needs the registry; green here is the proof the guard can count
test_package_import.py refusal tests (changed byte, truncated, missing layer, corrupt root) · also assert pins/packages/symlinks unchanged; no blob assert (verified metadata blobs left in the home are accepted)
test_install.py::test_dependency_pulled_into_fresh_home_records_origin · red (pre R1-PULL merge): `recorded origins set()` · green after merging 15db8725
test_install.py::test_install_from_an_import_records_no_origin · red: :264 `assert not origins.exists() or ...` with pull.rs recording an origin for every source (the Network-only condition dropped), reverted · bytes from an export are not network bytes
