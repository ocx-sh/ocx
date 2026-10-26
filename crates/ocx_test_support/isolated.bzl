# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Testcases a `rust_test` target `--skip`s, run each in a process of its own under Bazel.

`isolated_cases()` is an `sh_test` over the target's own test binary (no second compile), with
every crate's `:tree` as `data`. The cases it runs walk `crates/*/src` of crates their target does
not depend on, or need a process of their own; libtest runs threads in one process, so they cannot
run inside the target. Declaring the trees on the `rust_test` itself would re-run its whole suite on
any edit anywhere; here only these cases re-run.
"""

load("@rules_shell//shell:sh_test.bzl", "sh_test")

visibility(["//crates/..."])

# The workspace members. `scripts/tests/test_isolated_cases.py` holds this list to `crates/*/BUILD.bazel`.
CRATES = [
    "ocx_announce",
    "ocx_cli",
    "ocx_config",
    "ocx_console",
    "ocx_env",
    "ocx_exit",
    "ocx_exit_derive",
    "ocx_index",
    "ocx_oci",
    "ocx_package",
    "ocx_package_manager",
    "ocx_project",
    "ocx_python",
    "ocx_schema",
    "ocx_script",
    "ocx_sdkgen",
    "ocx_setup",
    "ocx_shell",
    "ocx_shim",
    "ocx_sign",
    "ocx_store",
    "ocx_test_support",
    "ocx_trust",
    "ocx_util",
]

def crate_tree(name = "tree"):
    """Every file of this package, for a test that walks the workspace."""
    native.filegroup(
        name = name,
        srcs = native.glob(["**"], allow_empty = True),
    )

def isolated_cases(name, binary, cases):
    """One `sh_test` running each of `cases` against `binary`, a `rust_test` label of this package."""
    sh_test(
        name = name,
        srcs = ["//crates/ocx_test_support:run_each_case.sh"],
        args = ["$(rootpath %s)" % binary] + cases,
        data = [
            binary,
            "//:workspace_files",
        ] + ["//crates/%s:tree" % crate for crate in CRATES],
    )
