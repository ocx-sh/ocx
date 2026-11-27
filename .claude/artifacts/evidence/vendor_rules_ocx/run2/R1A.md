# R1A: older and other-generation deferred launchers

Commit aadf24c3. Finding confirmed by reading (claim read only deferred/<host G>/l) and by the red runs below.

Red (mutation: declared_names returns an empty set, the old refusal; mutation proven present by grep before the run):
- unit claims_come_from_the_metadata_when_no_deferred_tree_exists: left [] right ["cmake","ctest"]
- unit launcher_trees_with_other_names_do_not_change_the_claim_set: left [] right ["cmake","ctest"]
- acceptance test_an_older_releases_deferred_launcher_still_runs_through_the_current_ocx: rc=65, "'hello' is not an interface name declared by ..."
- acceptance test_a_launcher_of_another_generation_still_runs_through_the_current_ocx: rc=65 (same refusal)
- the undeclared-name test stays green under the mutation (65 is also the correct answer there).

Green: ocx_package_manager materialize_lazy 4 passed; ocx lib the_launcher_dirs_cover_older_and_other_generation_trees_that_exist 1 passed; tests/test_lazy_loading.py 36 passed (3 new). clippy -D warnings on ocx and ocx_package_manager clean.

Not run: Windows, macOS (CI only). Windows skips: 2 new tests (old POSIX script form; the undeclared-name case needs it).
Residual refusal: after ocx clean the leaf and dependency manifests are collected (a deferred tree retains config blobs only), so the offline metadata read fails with offline_manifest_missing (81) naming the digest and the ocx install remedy.
