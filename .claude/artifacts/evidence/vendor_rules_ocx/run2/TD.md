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
