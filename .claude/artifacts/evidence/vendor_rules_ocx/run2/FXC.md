# FXC evidence: comment ratchet and rendered text

## Item 1: code-docs ratchet
- Command: `python3 test/lint/test_code_docs_ratchet.py` (scratch HOME and OCX_HOME, never `--update`).
- Before: 14 risen keys, +41 over baseline (test_workflows.py, ocx_env_apply launch.rs, ocx_package_manager activation/error/generate/reachability_graph/render_toolchain/vendor, ocx_shim direct.rs and direct_launch.rs, ocx_store temp_store/launcher_generation).
- After: exit 0, no rise.
- Commits: `chore:` for ocx_env_apply + ocx_shim, `chore:` for package manager, store and test_workflows.py. IDs (`V-Lnn`, `C-nnn`, review labels) removed; the sentence kept.

## Item 2: rendered text
- `shells/default/bin` in four `long_about` strings of `crates/ocx_cli/src/command.rs` (lock, update, pull-like, remove) now names `shells/default-<hash>/bin` behind the `active` link.
- Package root `entrypoints/` now `launchers/<generation>/` in paths.rs, path_kind.rs, pull_dry_run.rs, which.rs, package_pull.rs, plus package.rs, select.rs, exec.rs, env.rs, toolchain_exec.rs, toolchain_env.rs.
- Metadata schema text: `crates/ocx_package/src/metadata/bundle.rs` `entrypoints` field.
- Not changed: composer.rs and other internal comments that still say `entrypoints/` (not rendered; reported).

## Item 3: launcher crates are comment-only
- Method: Python script strips `//` and `/* */` comments outside string, raw-string and char literals, drops blank lines, compares base 7d136b3c against HEAD for every changed file under `crates/ocx_shim` and `crates/ocx_env_apply`.
- Result: 3 files, 3 SAME, 0 DIFF. Mutation (one non-comment token edited) gave DIFF, so the check can go red.
- `cargo test -p ocx_shim --locked`: 162 + 18 + 1 passed. `cargo test -p ocx_env_apply --locked`: 78 + 1 passed. clippy `-D warnings` on both: clean.

## Item 4
- `cargo check -p ocx_package_manager -p ocx_store -p ocx -p ocx_package --all-targets --locked`: 0 errors.
- `cargo test --doc -p ocx_package -p ocx`: passed. `cargo fmt --check`: clean.
- `test/lint/test_testing_marker.py`: 33 passed, 1 failed (`test_the_rust_registry_and_its_readers_hold`: `__OCX_TESTING_SEAM_CHILD` is registered, no shipped Rust reads it). Not touched by these commits; outside this file set.
