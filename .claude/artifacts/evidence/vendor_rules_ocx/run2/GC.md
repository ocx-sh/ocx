# GC evidence (run 2: `ocx clean` retires launcher directories)

Base `31cdcbd6`. Branch `hex/bazel-rbe-rules-ocx--GC`. `CARGO_TARGET_DIR=$SCRATCH/run2/target-GC`, 2 jobs, no debug info.
`task verify:scoped` was not run because it routes a Bazel lane (rc-file rule, DP-39); `scoped_gate.py --plan` returned
`escalate` (the branch diff includes `.github/workflows/**` and hub crates). Each step ran these by hand:
`cargo fmt --all -- --check`, `cargo clippy -p ocx_package_manager -p ocx --all-targets --locked -- -D warnings`,
`cargo test -p ocx_package_manager --lib tasks::clean` (and `garbage_collection` for step 1), `cargo test -p ocx --lib api::data::clean`.

| Step | Commit | Red (before the implementation) | Green |
|---|---|---|---|
| 1 package and deferred generations (C-137, V-L41, C-132 half) | `0cd0ad52` | 4 tests on assertions, e.g. `clean_retires_launcher_generations_other_than_the_hosts`: `got [] left: 0 right: 2`; the graph test `left: None right: Some(Shim)` | `tasks::clean` 28 passed; `garbage_collection` 52 passed; `api::data::clean` 2 passed |
| 2 toolchain homes (C-137, C-132, V-L29 clean half, V-L30) | `af21575c` | 5 tests against a no-op stub, e.g. `clean_dry_run_reports_superseded_render_directories_without_removing`: `got [] left: 0 right: 3` | `tasks::clean` 38 passed; `api::data::clean` 2 passed |

What the two steps do:

- `deferred/<G>/…` (G = `LauncherGeneration::host()`) is a GC tier with the `shims/` shape: entry plus `refs/blobs/` edges,
  rooted in lock pins, unrooted collected as `object`. Old `shims/` handling is unchanged.
- `launchers/<G'>` of every package and `deferred/<G'>` are retired through `retire_dir`, reported as `launchers`. A host with no
  embedded blob retires nothing.
- For the global home and every `state/projects/<key>/` with a v2 stamp whose scope matches its key: under the
  `toolchain-render` lock, after `refuse_symlinked_home`, every `shells/default-<16 hex>`, `.staging-*`, `.gc-*` other than
  `shell_dir` and `previous_shell_dir` is retired. A held lock or a refused guard skips the home with a warning.
- `--dry-run` reports candidates and retires nothing. `Retired::LeftForLater` is logged and not reported.
- Characterization (green at write): a held `fs.temp.scratch("deferred")` survives `clean` (V-L26 clean half; its red is EP.4's).

Files: `crates/ocx_package_manager/src/tasks/clean.rs`, `crates/ocx_cli/src/api/data/clean.rs` (`Clean::new` takes a fourth
list, allowed by the plan), `crates/ocx_cli/src/command/clean.rs` (help text), and, outside the plan's Expected files,
`crates/ocx_package_manager/src/tasks/garbage_collection.rs` and `.../garbage_collection/reachability_graph.rs`: the deferred
walk cannot be added without the reachability graph, and no other pipeline owns those files.

Not run or not covered:

- The V-L41 mutation (constant `G`) was not applied here; it is PF's mut row.
- A clean skipping a home whose render lock is held: no test, since clean's timeout is the 5 s constant.
  `clean_takes_the_lock_a_render_of_the_same_home_takes` pins clean's lock parameters to `render_lock_parameters`.
- Bazel lanes, acceptance (`test_clean.py` is HA2's), Windows and macOS.

For I2: `retire_dir` now has a consumer, so its `OCX_UTIL_WITHOUT_CONSUMER` entry in
`crates/ocx_test_support/tests/workspace_structure.rs` must be dropped (left untouched here). The `reports` and `cli` goldens
change with `CleanKind::Launchers` and the `ocx clean` help text.
