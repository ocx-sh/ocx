# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for secret variables that hold a reference (``file://``, ``env://``, ``plain://``).

The registries here are anonymous, so a pull that ignores a broken token reference
still succeeds. That is the failure these tests catch: an unresolvable reference
must fail the command, never fall back to anonymous access.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from uuid import uuid4

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.command("package_pull")

EXIT_IO_ERROR = 74
EXIT_CONFIG = 78


def slug(registry: str) -> str:
    """``localhost:5003`` -> ``localhost_5003`` (``StringExt::to_slug``)."""
    return re.sub(r"[^a-zA-Z0-9]", "_", registry)


@pytest.fixture
def pull_with(ocx: OcxRunner, target_registry: str, tmp_path: Path):
    """Publish a package to the target registry once; return a puller taking env overrides."""
    package = make_package(
        ocx, f"t_{uuid4().hex[:8]}_secret", "1.0.0", tmp_path, cascade=False
    )
    insecure = f"{ocx.registry},{target_registry}"
    ocx.run(
        "package",
        "copy",
        "--to",
        target_registry,
        package.short,
        env_overlay={"OCX_INSECURE_REGISTRIES": insecure},
    )
    reference = f"{target_registry}/{package.repo}:{package.tag}"
    counter = 0

    def pull(**extra: str):
        nonlocal counter
        counter += 1
        home = tmp_path / f"home-{counter}"
        home.mkdir()
        return ocx.run(
            "package",
            "pull",
            reference,
            check=False,
            env_overlay={
                "OCX_HOME": str(home),
                "OCX_INSECURE_REGISTRIES": insecure,
                **extra,
            },
        )

    return pull


def test_a_missing_token_file_fails_the_pull_instead_of_going_anonymous(
    pull_with, target_registry: str, tmp_path: Path
) -> None:
    """``OCX_AUTH_<registry>_TOKEN=file://<missing>`` exits 74 and names the variable and the path.

    The control pull, with no variable set, succeeds: otherwise a failure
    could come from the setup rather than from the reference.
    """
    control = pull_with()
    assert control.returncode == 0, f"control pull failed: {control.stderr}"

    variable = f"OCX_AUTH_{slug(target_registry)}_TOKEN"
    missing = tmp_path / "no-such-dir" / "token"
    assert not missing.exists()

    failed = pull_with(**{variable: f"file://{missing}"})

    assert failed.returncode == EXIT_IO_ERROR, (
        f"stdout: {failed.stdout}\nstderr: {failed.stderr}"
    )
    assert variable in failed.stderr, failed.stderr
    assert str(missing) in failed.stderr, failed.stderr
    assert (
        json.loads(failed.stdout)["error"]["detail"] == "secret_reference_unreadable"
    ), failed.stdout


def test_a_readable_token_file_does_not_fail_the_pull(
    pull_with, target_registry: str, tmp_path: Path
) -> None:
    """A readable token file with a trailing newline raises no reference error.

    The registry is anonymous, so it accepts any credential and this cannot show
    the file content became the token; the unit tests in
    ``crates/ocx_oci/src/auth.rs`` prove that.
    """
    token = tmp_path / "token"
    token.write_text("s3cret-token\n", encoding="utf-8")
    variable = f"OCX_AUTH_{slug(target_registry)}_TOKEN"

    result = pull_with(**{variable: f"file://{token}"})

    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr}"
    assert variable not in result.stderr, result.stderr


def test_readable_user_and_token_files_let_the_pull_succeed_without_printing_either(
    pull_with, target_registry: str, tmp_path: Path
) -> None:
    """The registry is anonymous: this proves both files resolve and stay out of the output."""
    user_text = f"user-{uuid4().hex}"
    token_text = f"token-{uuid4().hex}"
    user = tmp_path / "user"
    token = tmp_path / "token"
    user.write_text(user_text, encoding="utf-8")
    token.write_text(token_text, encoding="utf-8")
    prefix = f"OCX_AUTH_{slug(target_registry)}"

    result = pull_with(
        **{f"{prefix}_USER": f"file://{user}", f"{prefix}_TOKEN": f"file://{token}"}
    )

    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr}"
    for text in (user_text, token_text):
        assert text not in result.stdout
        assert text not in result.stderr


@pytest.mark.parametrize(
    ("reference", "expected_text"),
    [
        pytest.param("file://token", "absolute", id="relative_file_path"),
        pytest.param("env://A=B", "no environment can hold", id="env_name_with_equals"),
    ],
)
def test_an_invalid_reference_fails_the_pull_with_a_config_error(
    pull_with, target_registry: str, reference: str, expected_text: str
) -> None:
    variable = f"OCX_AUTH_{slug(target_registry)}_TOKEN"

    failed = pull_with(**{variable: reference})

    assert failed.returncode == EXIT_CONFIG, (
        f"stdout: {failed.stdout}\nstderr: {failed.stderr}"
    )
    assert variable in failed.stderr, failed.stderr
    assert expected_text in failed.stderr, failed.stderr
    assert json.loads(failed.stdout)["error"]["detail"] == "secret_reference_invalid", (
        failed.stdout
    )


def test_a_missing_user_file_fails_the_pull_without_leaking_the_token(
    pull_with, target_registry: str, tmp_path: Path
) -> None:
    sentinel = f"sentinel-{uuid4().hex}"
    token = tmp_path / "token"
    token.write_text(sentinel, encoding="utf-8")
    missing = tmp_path / "no-such-dir" / "user"
    user_variable = f"OCX_AUTH_{slug(target_registry)}_USER"

    failed = pull_with(
        **{
            user_variable: f"file://{missing}",
            f"OCX_AUTH_{slug(target_registry)}_TOKEN": f"file://{token}",
        }
    )

    assert failed.returncode == EXIT_IO_ERROR, (
        f"stdout: {failed.stdout}\nstderr: {failed.stderr}"
    )
    assert user_variable in failed.stderr, failed.stderr
    assert sentinel not in failed.stdout
    assert sentinel not in failed.stderr


def test_a_reference_is_resolved_one_step_only(pull_with, target_registry: str) -> None:
    """``env://`` yields the target's text verbatim, even when that text looks like a reference."""
    chained = "file:///definitely/missing"

    result = pull_with(
        **{
            f"OCX_AUTH_{slug(target_registry)}_TOKEN": "env://OTHER_VAR",
            "OTHER_VAR": chained,
        }
    )

    assert result.returncode == 0, f"stdout: {result.stdout}\nstderr: {result.stderr}"
    assert "OCX_AUTH" not in result.stderr, result.stderr
    assert "OTHER_VAR" not in result.stderr, result.stderr
    assert chained not in result.stderr, result.stderr
