# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the `--format json` error document.

One invocation under `--format json` (or `--format=json`, or `--json`) that
fails prints exactly one error document on stdout, whatever stage it failed
in: a command line clap refuses (exit 64), an `OCX_*` variable or a config
file that cannot be read (exit 78), or the command itself. A `--json` that
sits after `--` belongs to the child command and asks for nothing. The
document never carries a credential, whichever variable or file held it.
"""

from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path
from typing import Any

import pytest

from src import conformance
from src.runner import OcxRunner, PackageInfo, current_platform
from tests.fixtures.sigstore_stack import SigstoreStack

JSON_SPELLINGS = [
    pytest.param(["--format", "json"], id="format-json"),
    pytest.param(["--format=json"], id="format-equals-json"),
    pytest.param(["--json"], id="json"),
]


def _run(
    ocx: OcxRunner, argv: list[str], env: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(ocx.binary), *argv],
        capture_output=True,
        encoding="utf-8",
        env={**ocx.env, **(env or {})},
        stdin=subprocess.DEVNULL,
        check=False,
    )


def _document(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    """The one JSON document on stdout, validated against the published errors schema."""
    assert result.stdout.strip(), (
        f"no error document on stdout (rc={result.returncode})\nstderr: {result.stderr}"
    )
    # `json.loads` refuses trailing data, so a second document on stdout reds here.
    document = json.loads(result.stdout)
    verdict = conformance.observe([], result.stdout, result.returncode)
    assert verdict is not None and verdict.finding is None, (
        f"the error document breaks the published schema: {verdict}\nstdout: {result.stdout}"
    )
    assert document["exit_code"] == result.returncode, (
        "the document and the process disagree"
    )
    assert "remediation" not in document["error"]
    assert result.stdout == json.dumps(document, indent=2, ensure_ascii=False) + "\n", (
        "the error document is not pretty-printed like a success report"
    )
    return document


def _bad_config(tmp_path: Path) -> Path:
    path = tmp_path / "broken.toml"
    path.write_text("this is = = not toml\n")
    return path


@pytest.mark.parametrize("spelling", JSON_SPELLINGS)
def test_a_usage_error_prints_one_error_document(
    ocx: OcxRunner, spelling: list[str]
) -> None:
    result = _run(
        ocx, [*spelling, "package", "install", "--not-a-real-flag", "cmake:3.28"]
    )

    assert result.returncode == 64, result.stderr
    document = _document(result)
    assert document["schema_version"] == 1
    assert document["command"] == "package install"
    assert document["error"]["kind"] == "usage_error"
    assert document["error"]["detail"] == "invalid_command_line"
    assert "--not-a-real-flag" in document["error"]["message"]
    assert "--not-a-real-flag" in result.stderr, (
        "clap's own diagnostic still reaches stderr"
    )


def test_a_usage_error_at_the_root_prints_one_error_document(ocx: OcxRunner) -> None:
    result = _run(ocx, ["--json", "--not-a-real-flag"])

    assert result.returncode == 64, result.stderr
    document = _document(result)
    assert document["error"]["kind"] == "usage_error"
    assert document["command"] == "", "no subcommand was named, so there are no words"


def test_a_usage_error_without_json_prints_nothing_on_stdout(ocx: OcxRunner) -> None:
    result = _run(ocx, ["package", "install", "--not-a-real-flag", "cmake:3.28"])

    assert result.returncode == 64, result.stderr
    assert result.stdout == ""


def test_a_json_flag_after_double_dash_asks_for_no_document(ocx: OcxRunner) -> None:
    """The `--format json` after `--` is the child's argument, so a usage error prints no document."""
    result = _run(ocx, ["exec", "--not-a-real-flag", "--", "tool", "--format", "json"])

    assert result.returncode == 64, result.stderr
    assert result.stdout == "", f"a document for a flag the child owns: {result.stdout}"

    # Control: the same refusal with the flag where ocx reads it does print one.
    control = _run(
        ocx, ["--format", "json", "exec", "--not-a-real-flag", "--", "tool", "--json"]
    )
    assert control.returncode == 64, control.stderr
    assert _document(control)["command"] == "exec"


def test_an_invalid_ocx_variable_prints_one_error_document(ocx: OcxRunner) -> None:
    value = "ocx-wp24-not-a-boolean-0d3f"
    result = _run(
        ocx,
        ["--format", "json", "package", "install", "cmake:3.28"],
        {"OCX_OFFLINE": value},
    )

    assert result.returncode == 78, result.stderr
    document = _document(result)
    assert document["command"] == "package install"
    assert document["error"]["kind"] == "config_error"
    assert document["error"]["detail"] == "invalid_env"
    assert "OCX_OFFLINE" in document["error"]["message"]
    assert value not in result.stdout + result.stderr, (
        "an invalid value is never echoed"
    )


def test_an_unreadable_config_file_prints_one_error_document(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    result = _run(
        ocx,
        [
            "--format",
            "json",
            "--config",
            str(_bad_config(tmp_path)),
            "package",
            "install",
            "cmake:3.28",
        ],
    )

    assert result.returncode == 78, result.stderr
    document = _document(result)
    assert document["command"] == "package install"
    assert document["error"]["kind"] == "config_error"


def test_quiet_does_not_suppress_the_error_document(
    ocx: OcxRunner, tmp_path: Path
) -> None:
    usage = _run(
        ocx,
        [
            "--format",
            "json",
            "--quiet",
            "package",
            "install",
            "--not-a-real-flag",
            "x:1",
        ],
    )
    assert usage.returncode == 64, usage.stderr
    assert _document(usage)["error"]["kind"] == "usage_error"

    init = _run(
        ocx,
        [
            "--json",
            "-q",
            "--config",
            str(_bad_config(tmp_path)),
            "package",
            "install",
            "x:1",
        ],
    )
    assert init.returncode == 78, init.stderr
    assert _document(init)["error"]["kind"] == "config_error"


def test_a_failed_command_reports_the_slug_of_the_error_that_decided_its_exit_code(
    ocx: OcxRunner, unique_repo: str
) -> None:
    result = _run(
        ocx,
        ["--format", "json", "--offline", "package", "install", f"{unique_repo}:1.0.0"],
    )

    assert result.returncode == 81, result.stderr
    document = _document(result)
    detail = document["error"].get("detail")
    assert isinstance(detail, str) and detail, (
        f"no slug names the refusal: {result.stdout}"
    )


def _assert_no_secret(
    result: subprocess.CompletedProcess[str], secrets: dict[str, str], leg: str
) -> None:
    for name, secret in secrets.items():
        assert secret not in result.stdout, f"{leg}: the {name} secret reached stdout"
        assert secret not in result.stderr, f"{leg}: the {name} secret reached stderr"


def test_no_credential_reaches_the_error_document(
    ocx: OcxRunner,
    published_package: PackageInfo,
    sigstore_stack: SigstoreStack,
    tmp_path: Path,
) -> None:
    """Credentials in auth variables, the identity token, a proxy URL or a mirror URL never surface.

    Each leg's failure is shown to follow the read of its secret: a control without
    the secret fails differently, so a leg cannot pass by never reaching it.
    """
    pkg = published_package
    secrets = {
        "token": "wp24-registry-token-7c1e",
        "identity": "wp24-identity-token-91ab",
        "proxy": "wp24-proxy-password-5d20",
        "mirror": "wp24-mirror-password-e44f",
    }

    # Proxy: the install fails on the dead proxy; without it the same install succeeds.
    proxy = f"http://proxy-user:{secrets['proxy']}@127.0.0.1:1"
    install = ["--format", "json", "package", "install", pkg.short]
    proxied = _run(
        ocx, install, {"HTTP_PROXY": proxy, "HTTPS_PROXY": proxy, "ALL_PROXY": proxy}
    )
    assert proxied.returncode == 75, proxied.stderr
    assert _document(proxied)["error"].get("detail") == "registry_transient"
    _assert_no_secret(proxied, secrets, "proxy")
    direct = _run(ocx, install)
    assert direct.returncode == 0, (
        f"control: the install fails without the proxy too\n{direct.stderr}"
    )

    # Registry token: without it the auth lookup names the missing variable.
    slug = re.sub(r"[^a-zA-Z0-9]", "_", ocx.registry)
    token_env = f"OCX_AUTH_{slug}_TOKEN"
    auth = {f"OCX_AUTH_{slug}_TYPE": "basic", f"OCX_AUTH_{slug}_USER": "wp24-user"}
    absent_tag = ["--format", "json", "package", "install", f"{pkg.repo}:9.9.9"]
    untokened = _run(ocx, absent_tag, auth)
    assert token_env in untokened.stderr, "control: the auth lookup never ran"
    tokened = _run(ocx, absent_tag, {**auth, token_env: secrets["token"]})
    assert tokened.returncode == 79, tokened.stderr
    assert _document(tokened)["error"].get("detail") == "package_not_found"
    assert token_env not in tokened.stderr, "the token was not read"
    _assert_no_secret(tokened, secrets, "token")

    # Identity token: Fulcio rejects it; without it the OIDC pre-check refuses first.
    sign = [
        "--format", "json", "package", "sign", "--no-tty",
        "--platform", current_platform(),
        "--fulcio-url", sigstore_stack.fulcio_url,
        "--rekor-url", sigstore_stack.rekor_url,
        pkg.short,
    ]  # fmt: skip
    unsigned = _run(ocx, sign)
    assert _document(unsigned)["error"].get("detail") == "oidc_pre_check_failed"
    signed = _run(ocx, sign, {"OCX_IDENTITY_TOKEN": secrets["identity"]})
    assert signed.returncode == 80, signed.stderr
    assert _document(signed)["error"].get("detail") == "oidc_token_rejected"
    _assert_no_secret(signed, secrets, "identity")

    # Mirror: the config loader refuses the credential-bearing URL by name.
    config = tmp_path / "config.toml"
    config.write_text(
        f'[mirrors]\n"{ocx.registry}" = "http://mirror-user:{secrets["mirror"]}@127.0.0.1:1"\n'
    )
    mirrored = _run(ocx, ["--format", "json", "--config", str(config), *install[2:]])
    assert mirrored.returncode != 0, mirrored.stderr
    message = _document(mirrored)["error"]["message"]
    assert "mirror url must not carry credentials" in message, message
    _assert_no_secret(mirrored, secrets, "mirror")
