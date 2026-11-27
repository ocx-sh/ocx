# R1G — manifest `executable` on a Windows host

Root cause: `list_tree` read `executable` from the Unix mode bit (`ocx_package::bin_scan::unix_is_executable`), which is always `false`
on a non-Unix build, so a Windows host wrote `executable: false` for `bin/tool.exe` where a Linux host wrote `true`.

Fix commit: see branch log (`fix: vendor ... executable in manifest.json`). `mark_declared_binaries_executable` sets the flag for every
file named by a `Binary` tool's `target_path`, after the trees are listed, on every host.

Red: `a_declared_binary_is_executable_whatever_the_host_mode_bits_say` with the stub body empty (mutation present, line `fn mark_declared_binaries_executable(_tools..., _packages...) {}`):
`left: [false, false] right: [true, false]`.
Green: `cargo test -p ocx_package_manager --lib vendor`: 63 passed. clippy `-D warnings` clean. `test/tests/test_vendor_parity.py` on Linux: 8 passed, 1 skipped; committed goldens unchanged.
Windows result is reasoned, not run (CI only).
