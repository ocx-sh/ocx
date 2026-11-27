# R1J evidence

## Task 1 — OCX_BINARY_PIN doc cap
- Commit: see `git log` subject "docs: the OCX_BINARY_PIN reference states the pin and the PATH walk in ten lines".
- Red: code-docs ratchet, doc block of `OCX_BINARY_PIN` 12 lines against cap 10 (as reported).
- Fix: doc block now 8 lines (incl. one blank); facts kept: absolute pin, empty = unset on every OS, PATH walk absolute-only, skips own dir and other launchers.
- Green: `ocx_schema docs --out website/src/content/docs --check` exit 0 after regeneration; lint tier (`pytest lint/ -m "not deep"`, scratch HOME/OCX_HOME) 808 passed; `cargo test -p ocx_env` 93+4 passed, 0 failed; `rustfmt --check` on vars.rs clean.

## Task 2 — gamma hostile-argument test on Windows
- Commit: "test: the hostile-argument launcher test on Windows runs the probe it asserts on".
- Defect (by reading): `bins=["gamma"]` writes `bin/gamma.bat` on Windows; PATHEXT takes `.bat` before `.cmd`, so `gamma.cmd` never ran.
- Fix: the test builds its package with `extra_files={"bin/gamma.bat": probe}` (same shape as the case-fold test), asserts a `gamma-cmd` line for every exit-0 run and that at least one run happened, before the injection assertion; control runs the `.bat`.
- Linux module counts before and after: 10 passed, 5 skipped both. Skip count unchanged on every platform (the test stays `windows_only`).
- Windows path reasoned, not run.
