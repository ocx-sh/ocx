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

WASIP1 = "wasip1/wasm"
WASIP2 = "wasip2/wasm"


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
