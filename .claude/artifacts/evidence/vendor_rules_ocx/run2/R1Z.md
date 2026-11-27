# R1Z: wasm vendor tests red on a Windows host

Root cause: `test/src/helpers.py` `make_package` wrote the `bin/` leaf as `tool.bat` on any Windows host
(`sys.platform == "win32"`), even for `-p wasip1/wasm`, while the test claims `binaries=["tool"]`; `ocx vendor`
(`ensure_binary_targets_exist`, `crates/ocx_package_manager/src/tasks/vendor.rs`) correctly found no `bin/tool`.
Introduced by 172d391c (Windows fixture branch). Defect: test fixture, not product (target-platform naming is not
host-dependent in vendor.rs).

Fix: `_writes_windows_binary(plat)` gates the Windows leaf on a `windows/` target.
Regression test: `test/lint/test_make_package_windows_leaf.py`.
Red (predicate mutated back to host-only): 3 failed, 2 passed (wasip1/wasm, linux/amd64, darwin/arm64 cases).
Green: 5 passed; `test_make_package_scan_scope.py` still passes (25 passed together).
Not run: the two Windows acceptance tests (Windows host needed), Linux acceptance modules.
Other instances: none found (no other `host_bins`/`bins=` use with a foreign `platform=`).
