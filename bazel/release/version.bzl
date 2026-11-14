"""`OCX_VERSION`: the crate version Bazel hands rustc as `CARGO_PKG_VERSION`.

Every `crates/*/BUILD.bazel` sets `version = OCX_VERSION`. It must equal the
`[workspace.package] version` in `Cargo.toml`: `bazel_pin_check.py` fails on a
difference and `task release:bump-files` rewrites both.
"""

visibility(["//crates/..."])

OCX_VERSION = "0.6.6"
