# R2B

## Item 1: zero file identity (state_store.rs)
`file_id_of` now returns `None` for 0 on the unix and windows branches via `no_zero_identity`; the non-unix/non-windows branch already returns `None`.
A stamp holding `Some(0)` never equals a fresh stamp (`None` or non-zero), so the gate's `file_id.is_some() && identity == stamped` falls through to size plus hash; no error path. Stamp format unchanged. `render_toolchain.rs` untouched.
Test `a_zero_file_identity_is_no_identity`.
- red (helper neutralised to `Some(id)`, mutation confirmed present): `left: Some(0) right: None`, `1 failed`
- green: `1 passed`; `file_structure::state_store`: 39 passed.
Not run: Windows (CI only). The stamp-vs-gate comparison itself lives in render_toolchain.rs and is covered only by reasoning.

## Item 2: call-site test (vendor.rs tests)
`write_content_marks_the_declared_binary_executable_though_its_mode_bits_are_clear` drives `write_content` over a fake registry (new test helper `register_package_typed` spells `artifactType` so the pull succeeds), with a 0644 declared binary and a 0644 undeclared file.
- red (call line commented out; grep count of the call = 0): `[("p/.../c/bin/cmake", false), ("p/.../c/share/readme", false)]`, `1 failed`
- green (call restored; count = 1): `1 passed`; `tasks::vendor`: 56 passed.
Unix only (`cfg(unix)`): one test skipped on Windows.
