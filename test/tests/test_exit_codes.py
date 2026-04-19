# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the public exit-code contract (sysexits alignment).

These codes are the scripting surface for tools that consume OCX — scripts
do `case $?` on them. Each test exercises one specific exit code via a minimal
real-world failure that reliably triggers it.

Contract demonstrations: the xfail-marked tests below document the intended
mapping but currently fail because the product doesn't yet route these error
paths through `classify_error`. They are `strict=True` so the marker gets
removed automatically the moment the product catches up:

- 64 (UsageError): clap exits with its own hardcoded `2` before `classify_error`
  runs. Routing clap errors through the typed taxonomy requires switching from
  `get_matches()` (which exits internally) to `try_get_matches()` + custom
  error handling in `app.rs`. Deferred.
- 65 (DataError): the identifier parser is permissive — `not:::valid:::identifier`
  successfully parses and the install fails later as `NotFound` (79). Triggering
  `IdentifierError` at parse time requires either a stricter parser or a test
  fixture that can inject a pre-parsed `IdentifierError` through the CLI.
- 69 (Unavailable): `ocx index update` logs errors per-package and exits 0;
  `ocx install` against an unroutable host surfaces as `AuthError` (80) because
  the transport interprets the connection failure as an auth failure. Neither
  reliably produces `ClientError::Registry → Unavailable` via the CLI.

Deferred codes (no reliable acceptance-test trigger available):
- 74 (IoError): disk-full or read-failure not injectable from user tests.
- 75 (TempFail): rate-limit or transient-network failure not reliably injectable.
- 77 (PermissionDenied): filesystem EPERM not reliably injectable without root.
"""
from __future__ import annotations

import subprocess

import pytest

from src.runner import OcxRunner


class TestExitCodes:
    """End-to-end tests for the public exit-code contract (sysexits alignment).

    These codes are the scripting surface for tools that consume OCX — scripts
    do `case $?` on them. Exercise each code via a minimal real-world failure
    that triggers it.
    """

    @pytest.mark.xfail(
        strict=True,
        reason="clap exits with hardcoded 2 before classify_error runs; "
        "routing requires try_get_matches() refactor in app.rs",
    )
    def test_exit_code_64_usage_error_on_bogus_flag(self, ocx: OcxRunner) -> None:
        """Unknown flag → clap rejects → exit 64 (EX_USAGE)."""
        result = subprocess.run(
            [str(ocx.binary), "install", "--not-a-real-flag", "cmake:3.28"],
            capture_output=True,
            text=True,
            env=ocx.env,
        )
        assert result.returncode == 64, (
            f"expected exit 64 (UsageError) for unknown flag, "
            f"got {result.returncode}\nstderr: {result.stderr.strip()}"
        )

    @pytest.mark.xfail(
        strict=True,
        reason="identifier parser is permissive — malformed shapes resolve to "
        "NotFound (79) at install time rather than IdentifierError (65) at parse time",
    )
    def test_exit_code_65_data_error_on_invalid_identifier(self, ocx: OcxRunner) -> None:
        """Malformed identifier → IdentifierError → exit 65 (EX_DATAERR)."""
        result = subprocess.run(
            [str(ocx.binary), "install", "not:::valid:::identifier"],
            capture_output=True,
            text=True,
            env=ocx.env,
        )
        assert result.returncode == 65, (
            f"expected exit 65 (DataError) for malformed identifier, "
            f"got {result.returncode}\nstderr: {result.stderr.strip()}"
        )

    @pytest.mark.xfail(
        strict=True,
        reason="`index update` swallows per-package errors and exits 0; "
        "`install` against unroutable host surfaces as AuthError (80), not "
        "ClientError::Registry → Unavailable (69)",
    )
    def test_exit_code_69_unavailable_on_unroutable_registry(
        self, ocx: OcxRunner
    ) -> None:
        """Unroutable registry → ClientError::Registry → exit 69 (EX_UNAVAILABLE)."""
        # Port 1 is reserved and unroutable on standard systems.
        env = {**ocx.env, "OCX_DEFAULT_REGISTRY": "127.0.0.1:1"}
        result = subprocess.run(
            [str(ocx.binary), "index", "update", "some/pkg"],
            capture_output=True,
            text=True,
            env=env,
        )
        assert result.returncode == 69, (
            f"expected exit 69 (Unavailable) for unroutable registry, "
            f"got {result.returncode}\nstderr: {result.stderr.strip()}"
        )
