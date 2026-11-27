# TD evidence

## TD.1 recognise native trampolines by their record

Step: `is_ocx_trampoline` answers rule 2 on every OS (sibling or own `.launch` record peeks `toolchain`, bounded
regular-file read, `O_NONBLOCK`, `MAX_PEEK_BYTES + 1`); rule 1 (Windows `.exec`) and rule 3 (POSIX marker) kept in order.

- Red: with the record rule forced to false, `cargo test -p ocx_config --lib env::` failed 3:
  `a_launch_path_is_peeked_directly`, `a_schema_two_toolchain_record_is_a_trampoline`,
  `a_sibling_toolchain_record_is_a_trampoline` (env.rs assert at the `is_ocx_trampoline` call).
  Red was produced by a forcing edit, not against the pre-change source (tests were written with the code).
- Green: `env::` 60 passed; `cargo test -p ocx_config` 871 passed, 1 ignored.
- Mutation (not committed): `peek_kind` replaced by `LaunchRecord::decode`; mutated text confirmed present by grep;
  `a_schema_two_toolchain_record_is_a_trampoline` FAILED (panic at its `assert!(is_ocx_trampoline(&path))`). Restored.
- Not run: Windows unit `a_windows_exe_with_a_toolchain_record_is_a_trampoline` and the Windows `.exec` tests (CI only);
  `task verify:scoped` (hand run instead: `cargo fmt`, `cargo clippy -p ocx_config --all-targets --locked -- -D warnings`
  showed only warnings from `external/sigstore-rs`, none in ocx_config; `cargo test -p ocx_config`).

## TD.2 launcher depth survives a clean environment

- Bug: `compose` starts `ExecMode::Clean` from `Env::clean()` and carried only `SystemRoot`, so `__OCX_LAUNCHER_DEPTH` was
  dropped and a launcher -> `ocx exec --clean` -> launcher chain restarted at depth 0 (the 32 limit never fired).
- Red: `exec_env::compose_tests::the_launcher_depth_counter_reaches_the_child_unchanged_in_every_mode` failed at
  `compose_tests.rs:222`: "the depth counter changed under Clean", left `None`, right `Some("7")`.
- Fix: `compose` copies the caller's depth into the clean env beside `carry_system_root`, never increments it. A package
  value is applied after, so the carry is a floor, not a lock. `registry_tests` table now lists the depth as carried under Clean.
- Green: `cargo test -p ocx_package_manager --lib exec_env::` 99 passed; `clippy -p ocx_package_manager --all-targets -D warnings` clean
  (only external/sigstore-rs warnings); full `-p ocx_package_manager`: 1657 passed, 1 failed (`launch::firewall_tests::every_child_spawn_is_pinned_to_the_projection`,
  `ocx_util/src/fs/publish.rs: 1 spawn expectation(s) against 0 projection call(s)`; file untouched here, not caused by this change).
- Mutation: replaced the `env.set(...)` carry line by `let _ = depth;` (grep count 1); new test and the registry table test both failed; restored.
- Audit (depth survives?): `ocx exec`, `package exec`/`toolchain_exec`, `launcher exec`, `launcher shim`, plugin dispatch, `package test`
  all go through `apply_child_env` -> `compose`: Layer/Isolate yes (ambient), Clean now yes. `launch/child_process.rs` spawns only the
  composed env: same. Out of scope: self-update version query (`update_check.rs`, hermetic by design, `ocx version` only), git
  (`announce/git_command.rs`), sdkgen template, `ocx_script` ocx module, `ocx_setup` live-shell test, shim direct tests.
- Not run: `task verify:scoped` (plan lists whole-branch paths and a Bazel lane; hand run above); Windows.
