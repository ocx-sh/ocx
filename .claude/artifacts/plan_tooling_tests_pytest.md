# Plan: tooling tests on pytest, in parallel

**Status:** done (2026-09-25), branch `refactor/bazel-test-binary`.

## Why

The gate scripts under `scripts/` proved themselves through a homemade harness
(`--self-test` flag, a `self_test()` orchestrator per script), and these proofs
were listed by hand in `taskfiles/scripts.taskfile.yml`. That list was guarded by
`self-test:complete` and run in sequence. Three parsers had red/green proofs
written as shell inside taskfile YAML. Two pytest suites (`test/lint`,
`.claude/tests`) ran serially. The loop was slow and the harness reimplemented
pytest: discovery, a completeness check, parallelism, reporting.

## Target

1. `scripts/tests/test_<script>.py`, one per gate script. Each is a pytest
   module whose tests call the script's existing proof functions, with shared
   setup such as the live Bazel capture as a module fixture. The script loses
   `self_test()` and `--self-test`, and pytest becomes the only entry point.
   A test in `scripts/tests/` asserts that every gate script has a module, which
   replaces `self-test:complete`.
2. The shell-in-YAML parser proofs (`rust:test:ceiling:self-test`,
   `rust:test:duration:self-test`, `test:ceilings:self-test`) move into Python
   with pytest tests.
3. `scripts/tests`, `test/lint` and `.claude/tests` run with `-n auto`.
4. A wall-clock budget on the tooling suite (20 s).

Out of scope: the acceptance suite, the Docker shell matrix, the Rust unit tests.

## Execution

- The infrastructure (`scripts/tests/pytest.ini`, `conftest.py`) and the
  `bazel_tag_guard` pattern are done first, in the main loop.
- The other gate scripts are converted by file-disjoint workers, each allowed
  only `scripts/<x>.py` and `scripts/tests/test_<x>.py`. Workers do not commit.
- The main loop owns every shared file (taskfiles, `.claude/tests`, rules) and
  the commits. Each step is measured before and after and committed via
  `task verify:mark`.
