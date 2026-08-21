# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Acceptance tests for the wasm platforms and the supported-pair check.

Two things ship together and are tested together here:

* `wasip1/wasm` (WASI 0.1 modules) and `wasip2/wasm` (WASI 0.2 components)
  parse, publish, resolve and install like any other platform.
* The cross product of `OperatingSystem` x `Architecture` is no longer the
  set of legal platforms. `SUPPORTED_PAIRS` is, and it is enforced at the two
  places a `Platform::Specific` is built out of separate parts — so
  `wasip1/amd64` and `linux/wasm` name nothing and are refused at parse.

Wasm targets get no automatic binary scan: a wasm module is a data artifact
OCX never exec's, so the `binaries` claim comes back empty even when the
content tree carries executable files. `test_wasm_target_claims_no_binaries`
pins that against a `linux/amd64` control running on the identical tree.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from src.helpers import make_package
from src.runner import OcxRunner

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="the binaries-claim control leg drives raw Unix exec-bit fixtures; the "
    "wasm claim_name branch itself is covered by bin_scan.rs unit tests",
)

EXIT_SUCCESS = 0
EXIT_USAGE = 64  # UsageError — clap value_parser rejects the --platform value

WASIP1 = "wasip1/wasm"
WASIP2 = "wasip2/wasm"
NATIVE = "linux/amd64"


def _platform_arg(ocx: OcxRunner, platform: str) -> subprocess.CompletedProcess[str]:
    """Run a platform-taking command with `platform`, tolerating failure.

    The identifier is deliberately never published: every case here is
    refused while parsing `--platform`, before resolution is reached.
    """
    return subprocess.run(
        [str(ocx.binary), "package", "install", f"--platform={platform}", f"{ocx.registry}/absent:1.0.0"],
        capture_output=True,
        text=True,
        env=ocx.env,
    )


@pytest.mark.parametrize("platform", [WASIP1, WASIP2])
def test_wasm_package_publishes_and_installs(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, platform: str
) -> None:
    """A wasm package survives the whole create -> push -> index -> install path.

    `make_package` asserts on each step internally, so a platform the CLI
    refuses fails here at `create` rather than reaching the install.
    """
    pkg = make_package(ocx, unique_repo, "1.0.0", tmp_path, platform=platform)

    result = ocx.plain("package", "install", f"--platform={platform}", pkg.short)
    assert result.returncode == EXIT_SUCCESS, f"installing a {platform} package failed: {result.stderr}"


def test_wasm_target_claims_no_binaries(ocx: OcxRunner, unique_repo: str, tmp_path: Path) -> None:
    """A wasm target claims no binaries; the same tree under `linux/amd64` does.

    The control leg is what makes the empty claim evidence: it proves the
    fixture really does carry an interface-visible executable that the scan
    would otherwise find, so the wasm result is the no-scan rule and not an
    unreadable or empty content tree.
    """
    wasm_pkg = make_package(ocx, f"{unique_repo}-wasm", "1.0.0", tmp_path / "wasm", platform=WASIP1)
    ocx.plain("package", "install", f"--platform={WASIP1}", wasm_pkg.short)
    wasm_env = ocx.json("package", "env", f"--platform={WASIP1}", wasm_pkg.short)

    assert wasm_env["binaries"] == [], (
        f"a wasm target must claim no binaries (no automatic scan); got {wasm_env['binaries']}"
    )

    native_pkg = make_package(ocx, f"{unique_repo}-native", "1.0.0", tmp_path / "native", platform=NATIVE)
    ocx.plain("package", "install", f"--platform={NATIVE}", native_pkg.short)
    native_env = ocx.json("package", "env", f"--platform={NATIVE}", native_pkg.short)

    assert [claim["name"] for claim in native_env["binaries"]] == ["hello"], (
        "control: the identical tree under linux/amd64 must claim its executable, "
        f"otherwise the wasm assertion above proves nothing; got {native_env['binaries']}"
    )


@pytest.mark.parametrize("platform", ["wasip1/amd64", "wasip2/arm64", "linux/wasm", "darwin/wasm"])
def test_unsupported_platform_pair_is_refused(ocx: OcxRunner, platform: str) -> None:
    """An os and arch that are each valid, paired into something nobody ships.

    Asserting on the message, not only the code: every value here also exits
    64 before wasm existed, because `wasip1` was not an OS and `wasm` was not
    an architecture. Naming a supported pair in the refusal is what
    distinguishes "the pair check ran" from "the component was unknown".
    """
    result = _platform_arg(ocx, platform)

    assert result.returncode == EXIT_USAGE, (
        f"an unsupported pair must exit {EXIT_USAGE}; got {result.returncode}: {result.stderr}"
    )
    assert WASIP1 in result.stderr, (
        f"the refusal must list the supported pairs, which is what proves the pair "
        f"check refused it rather than an unknown os or arch; got: {result.stderr}"
    )


@pytest.mark.parametrize("platform", [WASIP1, WASIP2, NATIVE, "linux/arm64", "windows/arm64"])
def test_supported_pair_reaches_resolution(ocx: OcxRunner, platform: str) -> None:
    """Every supported pair parses — the failure is resolution, never usage.

    `windows/arm64` is in the list deliberately: it parsed before this change
    (there was no pair check to pass) and must keep parsing after it.
    """
    result = _platform_arg(ocx, platform)

    assert result.returncode != EXIT_USAGE, (
        f"{platform} is a supported pair and must parse; got a usage error: {result.stderr}"
    )
