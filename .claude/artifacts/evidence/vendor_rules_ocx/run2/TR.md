# TR evidence (run 2: toolchain render, prompt gate, setup refresh, record validation)

Base `675dd37f`. Branch `hex/bazel-rbe-rules-ocx--TR`. `CARGO_TARGET_DIR=$SCRATCH/run2/target-TR`, 10 jobs, no debug info.
Windows switch: variant A (`retarget_link` in place, DP-41). `task verify:scoped` was not run in any step: `scripts/scoped_gate.py --plan`
escalates on this branch and routes a Bazel lane (rc-file rule, DP-39). Each step ran by hand `cargo fmt --all -- --check`,
`cargo clippy -p <crate> --all-targets --locked -- -D warnings` (exit 0) and the crate's lib tests.

| Step | Commit | Red (before the implementation) | Green |
|---|---|---|---|
| 1+2 render publish, tree, switch (C-131, C-132, C-138; V-L28, V-L29 render half, V-L35) | `fa7e095c` | V-L28 `render_toolchain.rs:8396` "one render, one directory: [] left 0 right 1"; re-switch `:8482`; changed content `:8450`; V-L29 `:8517` "shells/default snapshot differs"; V-L35 exclusive staging `:8541` | `ocx_package_manager --lib` 1620 passed. Mutations: stamp ignored → V-L28 and re-switch red; `replace_atomic` switch → the `cfg(windows)` switch unit red (run once with its gate removed; compiled and run in CI only) |
| 3 prompt gate (C-133; V-L33) | `286e2037` | `activation.rs:4167` v2 stamp naming `active` → left `None` right `Some(…/active/bin)` | `activation` 125 passed. V-L33 mutation (`default-` prefix check for the exact `shell_dir` compare) → `v2_stamp_refuses_an_active_that_names_another_complete_directory` red, restored by grep |
| 4 setup phase 1.6 (C-136; V-L42 unit half) | `db510b9f` | `refresh_global_toolchain_renders_when_the_lock_exists`: no `state/render_stamp.v2.json`; failed-render case: no warning (red by neutering the lock load) | `ocx_setup --lib` 306 passed. The phase-call mutation was not run: the call inside `run` needs a full bootstrap; V-L42's acceptance test is its owner |
| 5 drop `ShimBinStore`, `hardlink::update` (DP-42) | `f886511c` | compile check: callers listed by grep before deletion | `cargo check --workspace --all-targets --locked` exit 0; `ocx_store --lib` 398 passed |
| added: exit 77 on a permission refusal (V-L25, C-127) | `1be2c356` | `error::tests::internal_file_answers_by_io_kind` left `(IoError, "package_manager_internal_file")`; render staging `left: Some(IoError) right: Some(PermissionDenied)` | `ocx_package_manager --lib` 1628 passed |
| added: writers validate records | `a4619afa` | `a_package_root_holding_the_vendor_token_is_refused_before_anything_is_written` (`generate.rs:717`) | `launcher::` 65, `tasks::prepare_lazy` 11, `tasks::render_toolchain` 113 passed |
| review fix | `f433b778` | `clean_keeps_the_directory_active_names_when_the_stamp_lags` (C retired); `a_partial_render_publishes_a_new_generation_holding_the_stamped_names` and `a_partial_render_that_adds_a_name_publishes_the_union` "a new directory is published" | `render_toolchain` 115, `clean` 39, `activation` 125, `ocx_setup` 306 passed |
| review fix 2 (orchestrator, inline) | `9daed449` | `a_stamped_case_twin_does_not_join_a_partial_render_on_a_folding_home` red with the case skip removed (`render_toolchain.rs:3254`) | `render_toolchain` 116 passed |

Final lib suites at `9daed449`: `ocx_package_manager` 1634 passed, 1 ignored; `ocx_store` 398; `ocx_setup` 306; `ocx` 1818 passed, 2 failed:
`exit::tests::detail_rows_snapshot_matches_the_registry` (I2: `crates/ocx_cli/src/exit/detail_rows.txt` needs a `permission_denied`
row for `PackageManagerError::InternalFile`) and `app::seam::tests::seam_installs_no_global_subscriber_and_logs_to_err` (known
parallel-run flake, CW2).

## Decisions

- Steps 1 and 2 ran as one worker step: the plan gives them one commit. The trampoline script and `.exec` bodies left `body.rs` in
  that commit (they became dead code); step 3 then only changed the gate.
- Exit 77: `InternalFile` classifies at run time (`#[exit(with = internal_file_row, rows(...))]`): `io::ErrorKind::PermissionDenied`
  → 77 with the existing slug `permission_denied`; every other kind, `ReadOnlyFilesystem` included, → 74
  `package_manager_internal_file`. No new slug, no new rung; `errors.json` is unchanged (it dedupes by slug). This moves every
  internal-file permission refusal (pull, export, first use), not only first use, from 74 to 77.
- Writers: `launcher::validate_for_write(record, windows, context)` maps `RecordError`'s `Display` into an `InvalidInput` file
  error; package, deferred and toolchain writers call it before any write. A deferred record cannot be invalid by type
  (`PinnedPackageRef`); its call is defence in depth.
- Setup phase 1.6 renders through the new `PackageManager::render_global_home()` (offline surface, all lock groups, `pinned`
  from the global `ocx.toml`), because `ocx_setup` has no edge to `ocx_project`. A failure is a `log::warn!`, never an error.
- Partial render (cold store, `unrendered` non-empty): the desired names are the computed set ∪ the previous v2 stamp's names
  (records are name-independent), compared by fingerprint like a full render, so a new generation still publishes.

## Review

One adversarial reviewer (sonnet, read-only) over `675dd37f..a4619afa`, against the atomic-publish and mixed-version rules.

- Fixed: `ocx clean` could retire the directory `active` names while the stamp lags (Block); a partial render never published
  a new generation (Warn); three join errors swallowed without a log, and a stamp with an empty bin fingerprint (Warn); the
  case-twin edge of the partial union (from the fix worker's own risk note).
- Traced with no finding: mixed versions both ways (older ocx heals `active` to `shells/default` → v1 gates, the new render
  re-switches without publishing; an older prompt after a new render omits the project entry), locking, the v2 gate, setup 1.6.
- Not fixed:
  - `heal_active` still removes a non-link `active` (a dereferencing copy) before `retarget_link`; that is the pre-existing
    defensive-layout behaviour of `adr_toolchain_activation.md`, reached only for a foreign real directory, never for anything
    an older ocx writes. For the owner: it is a delete in the render and a window without `active`.
  - A failed stamp write only warns; the next render publishes again (bounded by `ocx clean`).

## Files outside the Expected files

`launcher.rs` (re-export), `launch.rs` (one spawn-firewall exempt row of the deleted `/bin/sh` body harness),
`tasks/garbage_collection/reachability_graph.rs` (a test built its fixture with `shim_bin.ensure()`), `tasks/export.rs` (a test
pinned 74 for a permission refusal), `crates/ocx_cli/src/exit/ocx_package_manager.rs` (one test row's expected code and slug),
`launcher/generate.rs` and `tasks/prepare_lazy.rs` (allowed for the validation step).

## For I2 and later pipelines

- `OCX_UTIL_WITHOUT_CONSUMER`: `retarget_link` now has its first caller (render). `publish_dir` too (render, EP). Left untouched.
- `crates/ocx_cli/src/exit/detail_rows.txt`: one row (above). Goldens untouched.
- Stale references to the deleted store: `crates/ocx_env/src/vars.rs:583` registry row `__OCX_TESTING_SHIM_LOST_PUBLISH_RACE`
  (nothing reads it; a `cli.json`/env doc row if it is published), `crates/ocx_store/Cargo.toml:10-14` and
  `crates/ocx_store/CLAUDE.md:44` (`__testing` feature comment), `ocx_store::shim::published_blob_is_intact` now has no
  non-test caller (launcher-blob file, left alone).
- `crates/ocx_cli` still probes the physical `shells/default/bin`: `command/toolchain_exec.rs:461-475`
  (`trampoline_lookup_exclusions`; should also exclude the directory `active_target_name()` names) and
  `command/self_group/activate.rs:1024-1029` (`wrapper_binary_path`; the pinned global `ocx` trampoline now lives in
  `shells/<shell_dir>/bin`, so the wrapper falls back to the install bin). Fixtures at `toolchain_exec.rs:782-798`,
  `activate.rs:3521-3533`. Not a TR file.

## Not run

Bazel lanes; Windows and macOS (the `cfg(windows)` switch unit is CI-only); `workspace_structure`; `ocx_schema` goldens (red since
CW2, I2's). Acceptance: see below.

## Job 1: the hung unit suite (run 3 worker)

- Cause: a test-isolation self-deadlock in an uncommitted edit, not a product deadlock. The edit routed every test render through
  `render_unfaulted` (takes `ocx_env::overrides::lock()`, a non-reentrant process lock) to stop the fault-seam test
  (`__OCX_TESTING_RENDER_FAULT`, process-global) from aborting renders in parallel tests. Two tests already held that lock
  (`two_renders_against_one_home_are_serialized_by_the_render_lock`, `a_render_lock_timeout_is_a_skip_and_never_an_error`), so their
  `render_unfaulted` blocked on the lock the same thread held; the other 10 render tests queued behind it (12 threads at under 1 % CPU).
- Reproduced: with the diff, `cargo test ... tasks::render -- --test-threads=12` hit `timeout 120` (exit 124, 12 tests "running for
  over 60 seconds"). Without it (committed HEAD), `tasks::render` 117 passed in 0.79 s and the full lib suite passed 3 of 3 runs.
- Fix: the two lock-holding tests call `render_toolchain` directly; every other render goes through `render_unfaulted` (committed
  `206f75e7`).
- Regression test: `a_render_in_a_parallel_test_never_reads_a_fault_another_test_holds` (worker thread plus bounded
  `recv_timeout`). Red with `render_unfaulted` unlocked: `FAILED ... render_toolchain.rs:7952` "the render must not see a fault set by
  another test". Green with the lock; the helper-only fix cannot be reverted to the self-deadlock without a test hanging, which the
  existing suite shows (exit 124).
- Green: `ocx_package_manager --lib` 1636 passed, 1 ignored, 3 consecutive runs (20 to 32 s); `ocx_store` 398; `ocx_setup` 306;
  `ocx` 1818 passed, 2 failed (the two known reds above). `cargo fmt --all -- --check` clean; clippy `-D warnings` clean for
  `ocx_package_manager`, `ocx_setup`, `ocx`, `ocx_store`.
