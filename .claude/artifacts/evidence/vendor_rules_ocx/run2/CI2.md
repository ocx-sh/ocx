# CI2 — launcher CI (run 2)

Branch `hex/bazel-rbe-rules-ocx--CI2`, base `333a5264`. Tests run locally from `scripts/tests` and `.claude/tests` with
scratch `HOME` and `OCX_HOME`. Every DP-39 `verify:scoped` was not run, because the branch routes a Bazel lane (the rc-file rule):
`scoped_gate.py --plan` returns decision `escalate`.

| Step | Commit subject | Red (TDD) | Green |
|---|---|---|---|
| 1 (C-139) | `ci(shim): rebuild the launcher blobs when the cargo configuration changes` | `test_the_shim_workflow_watches_every_hashed_file[push\|pull_request]`: `assert ['.cargo/config.toml'] == []` at base; its control case passed in the same run | `test_shim_inputs.py` 21 passed; actionlint clean |
| 2 (C-113, V-L58, V-L36) | `ci(shim): check each committed launcher with every record kind` | `test_a_launcher_that_accepts_any_record_fails_every_case[no-record]`: `assert None` on `^FAIL no-record: ` (old case names) | 20 of 21 passed; shellcheck `-S style` clean |
| 3 (DP-41, V-L31, V-L32) | `ci: prove the in-place junction retarget on Windows` | `test_workflows.py`: `test_non_linux_jobs_only_download_and_execute[basic-repo.yml]` ("non-Linux jobs are ['junction-proof', 'unit-native'], expected exactly ['unit-native']") and `test_basic_runs_on_linux_except_its_native_smoke_legs` | `test_workflows.py` 442 passed; actionlint clean |

Expected red until act B: `test_launcher_blob_check.py::test_the_committed_host_blob_passes_every_case`. The committed blobs are the
first run's launcher. Run by hand against the host blob, 9 of 9 cases ran and 8 failed: the blob reads `tool.direct` and refuses `kind`.

Controls, all green: an accept-everything stand-in fails all 9 host cases. A stand-in that prints the right argv passes the four
home kinds. A stand-in that drops `--` fails the four home kinds. A dropped case fails the floor (`8 of 9 cases ran`).

Cases owed per target:
- Linux and x86_64 macOS: 9.
- Windows: 8.
- aarch64 macOS: 13, the 9 plus `signature-verifies`, `signature-catches-a-changed-byte`, `signed-copies-start` and `quarantine-start`.

These rows are proven only in CI:
- **V-L58 and V-L36.** Proven by the six legs of `launcher-blobs.yml`, after act B.
  - The Windows legs use Git for Windows' `echo.exe` as the stub `ocx.exe`. They compare argv joined by spaces and do not check the
    depth.
  - Quarantine is reported, not asserted.
- **V-L31 and V-L32.** Proven by `basic-repo.yml › junction-proof`, which has two legs: `runner` and `standard`.
  - The job needs `fs::publish::tests::junction_proof_retarget_under_lookup` and `fs::publish::tests::junction_proof_retire_running_exe`.
    Both are `#[ignore]` and `cfg(windows)`, in `crates/ocx_util/src/fs/publish.rs`, and pipeline PB writes them.
  - Both tests run from the `//crates/ocx_util:ocx_util_test` entry of the `unit-tests-x86_64-pc-windows-gnullvm` bundle.
  - A leg is red unless each test ran exactly once and passed.
  - `whoami /groups` proves that Administrators is enabled on the runner leg and absent on the standard leg.

The run URLs are folded in at act M2.
