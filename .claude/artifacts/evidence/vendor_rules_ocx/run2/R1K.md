# R1K — CI failures at fc57dc2b (run 45985966210)

- Conventional Commits (job 140461840930): cocogitto `check` over base..head rejects commit 21878237 (`Revert "test: red proof, the launcher without the extended-length prefix"`): "Missing commit type separator `:`". Not fixed (no history rewrite); reword at finalize.
- Unit tests / test (jobs 140461841150, 140461840690): same three Bazel targets fail in both.
  - //crates/ocx_shim:direct_launch: red under `bazel test` (PermissionDenied, direct_launch.rs:692, EACCES on re-copy over read-only launcher); fixed in 0d0d26ca; green under `bazel test`.
  - //crates/ocx_schema:golden_schemas: `cli_document_matches_golden` differs at byte 483150 (529674 vs 529669), env doc text "runs the same" vs "runs the". Golden is a hub file: regenerate with `cargo run -p ocx_schema -- cli > crates/ocx_schema/tests/golden/cli.json`.
  - //crates/ocx_test_support:workspace_structure_isolated: `every_utility_error_reaching_the_cli_is_armed`, six `RecordError` reaches in ocx_env_apply/src/launch.rs (181,227,433,445,461,484). Cause: 4ab104fa added `impl std::error::Error for RecordError`, which puts it in scope. Needs a UNARMED_AT_THE_BOUNDARY row in the hub file workspace_structure.rs (not edited here).
