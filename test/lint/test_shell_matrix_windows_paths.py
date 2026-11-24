# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The Windows branches of the `shell_matrix` path helpers, driven with Windows-shaped literals on any host.

`test_toolchain_activate.py` runs its bash sessions under Git Bash on Windows and compares
directories through these helpers; the Linux run only exercises their POSIX branch, so the
Windows one is proven here by passing the host explicitly.
"""

from __future__ import annotations

import pytest

from src import shell_matrix as matrix


@pytest.mark.parametrize(
    ("left", "right"),
    [
        (r"C:\Users\Runner\toolchain\bin", r"c:\users\runner\toolchain\bin"),
        (r"C:\Users\Runner\toolchain\bin", "C:/Users/Runner/toolchain/bin"),
        (r"C:\Users\Runner\toolchain\bin", "C:\\Users\\Runner\\toolchain\\bin\\"),
        (r"C:\Users\Runner\toolchain\bin", r"\\?\C:\Users\Runner\toolchain\bin"),
    ],
)
def test_windows_identity_folds_case_separators_and_the_verbatim_prefix(
    left: str, right: str
) -> None:
    assert matrix.path_identity(left, "win32") == matrix.path_identity(right, "win32")


def test_windows_identity_keeps_distinct_directories_apart() -> None:
    assert matrix.path_identity(r"C:\a\bin", "win32") != matrix.path_identity(
        r"C:\a\bin2", "win32"
    )
    assert matrix.path_identity("C:\\", "win32") == "c:\\"


def test_posix_identity_is_the_string() -> None:
    assert matrix.path_identity("/Users/Runner/bin/", "linux") == "/Users/Runner/bin/"


def test_native_path_list_splits_on_the_hosts_separator() -> None:
    assert matrix.native_path_list(r"C:\a;;C:\Program Files\Git\usr\bin;", "win32") == [
        r"C:\a",
        r"C:\Program Files\Git\usr\bin",
    ]
    assert matrix.native_path_list("/a::/usr/bin", "linux") == ["/a", "/usr/bin"]


@pytest.mark.parametrize(
    ("value", "named"),
    [
        (r"/c/a;C:\b", "';'"),
        (r"/c/a:C\b", "backslash"),
        ("/c/a:C:/b", "drive-letter"),
        ("C:/b:/usr/bin", "drive-letter"),
    ],
)
def test_an_unconverted_bash_path_is_named_on_windows(value: str, named: str) -> None:
    fault = matrix.posix_path_list_fault(value, "win32")
    assert fault is not None and named in fault, fault


def test_a_converted_bash_path_passes_and_posix_never_faults() -> None:
    assert (
        matrix.posix_path_list_fault(
            "/c/users/runner/bin:/usr/bin:/c/Windows/System32", "win32"
        )
        is None
    )
    assert matrix.posix_path_list_fault(r"C:\a;b", "linux") is None


def test_a_lookup_without_exe_resolves_to_its_exe_twin_on_windows_only() -> None:
    present = {r"C:\t\bin\ocx.exe", r"C:\t\bin\ocx.exec"}
    assert (
        matrix.executable_identity(r"C:\t\bin\ocx", "win32", present.__contains__)
        == r"C:\t\bin\ocx.exe"
    )
    assert (
        matrix.executable_identity(r"C:\t\bin\ocx.exe", "win32", present.__contains__)
        == r"C:\t\bin\ocx.exe"
    )
    assert (
        matrix.executable_identity(r"C:\t\bin\gone", "win32", present.__contains__)
        == r"C:\t\bin\gone"
    )
    assert (
        matrix.executable_identity(
            "/t/bin/ocx", "linux", {"/t/bin/ocx.exe"}.__contains__
        )
        == "/t/bin/ocx"
    )
