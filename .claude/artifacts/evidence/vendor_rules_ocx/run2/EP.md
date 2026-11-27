# EP evidence (run 2: package entry points, deferred tools and pull)

Base `9ca69670`. Branch `hex/bazel-rbe-rules-ocx--EP`. The feature branch (LA, GC, the firewall exemption `62143a20`)
was merged in at `9a7acb6e`, between steps 3 and 4. `CARGO_TARGET_DIR=$SCRATCH/run2/target-EP`, 3 jobs, no debug info.
`task verify:scoped` was not run in any step, because `scripts/scoped_gate.py --plan` routes a Bazel lane (rc-file rule, DP-39).
Each step ran `cargo fmt --all --check` and `cargo clippy -p <crate> --all-targets --locked -- -D warnings` by hand, plus the
crate's lib tests.

| Step | Commit | Red | Green |
|---|---|---|---|
| 1 writer (C-128, C-130) | `0b6ae36a` | V-L38 writer-half listing `left: ["cmake","ctest"]` against the `.exe`/`.launch` pairs; V-L37 `cmake: bytes equal the host blob`; 6 new units red against the script writer | `ocx_package_manager --lib` 1621 passed, 1 failed (`launch::firewall_tests::every_child_spawn_is_pinned_to_the_projection`, PB's `publish.rs` test spawn, fixed on the feature branch by `62143a20`) |
| 2 pull, idempotence (C-140, C-142) | `f7fa1749` | `generate_is_idempotent`, `publish_does_not_recreate_a_removed_package_directory`, `completeness_needs_a_real_directory_with_every_file`: `not implemented` at `generate.rs:85`, `:67` | `launcher::` 86, `tasks::pull` 54 |
| 3 first use (C-127) | `4b497439` | see note 1. V-L21 `generate.rs:794`; V-L23 `:849` (missing record), `:870` (symlink at `D`); V-L25 `:927` (`left: Launchers right: Legacy`), `:951`; composer `composer.rs:2220` (`entrypoints` against `launchers/<G>`) | `ocx_package_manager --lib` 1632 passed, 2 failed: the vendor swap test (fixed in step 4) and the firewall test (fixed by the merge) |
| 4 deferred tools (C-129) | `88852e1e` | V-L26 `prepare_lazy.rs:577` "ocx clean must not delete a tree that is being staged" (old unlocked `TempDir` staging, swept by `fs.temp.stale_entries()`, the sweep `clean_temp` uses) | `ocx_package_manager --lib` 1652 passed, 0 failed, 1 ignored; `launch::firewall_tests` 5 passed; `ocx --lib` (`command::{which,launcher,exec,toolchain_exec}`) 67 passed |

Note 1. Step 3 wrote its tests after the implementation. It proved red by stubbing the code back to HEAD behaviour: a
`first_use` early return, the composer fixed to `entrypoints`, and the fault seam off. V-L22 (concurrent first use converges)
and V-L24 (a removed package directory is not recreated) stayed green under that stub, because they exercise `publish_dir`
behaviour that existed before. Neither has a red proof here, so their mutation proof is PF's.

## V rows at unit level

V-L21, V-L22, V-L23, V-L24, V-L25 (with the exit-code residue below), V-L26, V-L27 (home half: a `.launch` name is skipped
with a warning), V-L37, V-L38 (writer half). V-L20 and V-L38's start half are acceptance and CI rows, not run here.

Case twins (V-L27 home half). The package and deferred writers do not fold names. Their record holds no name (only
`package_root` or `identifier`), and every launcher is the same blob. So two names that are equal after case folding
write byte-identical files to one path on a case-insensitive file system. No unit asserts this.

## File set

These are outside the plan's EP Expected files. The driver widened the set, following the VF precedent: no concurrent
pipeline edits these files, and each edit is minimal.

- `tasks/pull_local.rs`: test only. `launcher_baked_with_override_root` reads `launchers/<G>/*.launch`.
- `tasks/resolve.rs`: test module only. Two assertions name `launchers(&G)`.
- `tasks/vendor.rs`: test module only. The install-root swap line uses `launchers/<G>`.
- `launcher/env_tests.rs` (TR's file; TR has not started): the baked-path-marker test of the deleted script body was removed.
- `ocx_cli/src/command/{launcher/shim.rs, launcher/exec.rs, which.rs, toolchain_exec.rs, exec.rs}`: the deferred tree is
  `deferred/<G>/…/l`, and the execution record's `shim_root` is `deferred_root()`. Without this, `ocx launcher shim` prunes the
  old `shims/…/bin` from `PATH`, and a materialized name resolves back into its own launcher.

CW2-listed files edited as EP's own work: `composer.rs`, `launcher/direct.rs`, `launcher/generate.rs`,
`tasks/prepare_lazy.rs`, `tasks/vendor.rs` (test only). No contract-wave signature, error variant or exit rung changed.

## Residue and open items

- **Exit 77 (V-L25, C-127) is not reachable.** A permission refusal on first use surfaces as `crate::Error::InternalFile`,
  which is `#[exit(IoError)]` with the cause interpolated rather than a source. It therefore exits 74, and
  `ocx_cli/src/exit/ocx_package_manager.rs:292` pins that. Exit 77 needs `InternalFile` re-attributed in `error.rs` (a
  contract-wave surface), so EP did not change it. The unit test asserts `InternalFile(<package>/launchers, PermissionDenied)`.
- **No record validation.** `RecordError` has no `std::error::Error` impl, so the writers do not call `LaunchRecord::validate`.
  Making it a source needs LA's file or a new error variant. Instead, the package writer refuses a root that fails
  `is_fully_qualified(root, windows)` (`InvalidInput`, nothing written). A deferred identifier is pinned by type.
- **Composer callers that bypass `compose_roots`** reach `composer::compose` at `tasks/resolve.rs:706`: `patch_why`,
  `package_test`, `launcher exec`, `launcher shim`, `update_check`, `patch_sync`, `resolve_env_from_package_root`. They run
  no first use. The composer reads the disk without writing: a complete `launchers/<G>`, else a complete older
  `entrypoints/`, else `launchers/<G>`. Only a package that an older release installed, with neither directory complete,
  emits a missing path there.
- A deferred root emits its package's `launchers/<G>` path without I/O, as it emitted `entrypoints` before.
- `tasks/vendor.rs::closure_root_of` (non-test code) still names `fs.shims.shim_dir`. Nothing reads that path.
- `setup_vendored` (`tasks/pull.rs`) builds `PackageDir::with_root(package_dir)`, whose `.content()` names `content`, not the
  vendored `c`. Nothing reads it; its doc comment says so (C-142).
- `publish_dir` and `retire_dir` now have their first callers. `OCX_UTIL_WITHOUT_CONSUMER` in
  `ocx_test_support/tests/workspace_structure.rs` is untouched (I2), and that test was not run.
- `launcher/body.rs` held no `launcher exec` or `launcher shim` token constants. They were literals in the deleted bodies, so
  only `ocx_shim`'s constants remain as the wire canary.
- Flake: `pull_all_opens_one_pulling_span_carrying_the_batch_size` failed once in a parallel run and passed alone.

Not run: acceptance tests (`test_entrypoints.py`, `test_launcher_exec.py`, `test_lazy_loading.py`), Bazel lanes, Windows
and macOS, `workspace_structure`.
