# R1I evidence (hub files, limits, red-proof log)

Environment: scratch `HOME` and `OCX_HOME`, scratch `CARGO_TARGET_DIR`; Linux only; no Bazel call (the generators ran through cargo).

1. Slug row, f92cec88. `exit::tests::detail_rows_snapshot_matches_the_registry` was red after R1D (R1D.md); after adding
   `launcher_record_refused` to `detail_rows.txt`, `cargo test -p ocx --lib exit::` gives 294 passed. Regenerated with the
   `ocx_schema` and `ocx-sdkgen` binaries: `errors.json` golden, SDK `exit.rs` and `types.rs`, `reference/exit-codes.md`,
   `commands.md` (clean help from R1B), `environment.md` (`OCX_BINARY_PIN` from R1E). `cli.json` and the other goldens did not move.
   `cargo test -p ocx_schema -p ocx_sdkgen`: all pass except four `real_binary` cases in `sdk_rust.rs` that need the Bazel-built binary.
   `ocx_schema docs --check` exits 0.
2. Ledger and version: none. A new slug is an addition to an open enum (IC-15), the error document keeps its exit codes, and the
   baseline under `crates/ocx_schema/contract/` records the last release and is rotated only by `contract:rotate`. The compat-gate
   tests (29) and `test/lint/test_contract_baseline.py` pass. The 74 to 65 change for a refused record moves no row of any gated document.
3. Limits, 12125c1a: `test/SUITE_FLOOR` 4600 to 4603; per-OS `SUITE_FLOOR` 3953 to 3956 (macos, windows, macos-release,
   windows-release) and linux-release 3946 to 3949 (three R1A cases, each a counted case on every OS); Windows `SKIP_CEILING`
   1925 measured + 2 = 1927; macOS 542; windows-release 2168 to 2170 (the two R1A skips only); census `PINNED` tests 3803, skipped 157
   (live census read 157 before the edit, `suite_census.py --check` green after).
4. Checks: `task test:lint:structure` 807 passed, 1 failed: `test_code_docs_ratchets_hold`, `crates/ocx_env/src/vars.rs:9` LEN-02/LEN-03,
   the R1E `OCX_BINARY_PIN` doc block is 12 lines against a cap of 10. Not fixed here (outside the file set). `task test:rows:check` OK.
   `cargo fmt --check` clean.
