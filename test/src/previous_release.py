"""The previous-release `ocx` the nesting tests drive, or a failure that says why.

The binary is pinned in `[group.test.tools]` of the root `ocx.toml`, so CI and
a developer who ran `ocx pull -g test` have it without any setup. Resolution
order: `OCX_TEST_PREVIOUS_OCX`, then that toolchain entry. With neither, the
test fails: a skipped proof and a passing one are the same green, so a leg that
has no previous release must say so with `OCX_TEST_NO_PREVIOUS_OCX=1`, and the
skip then names that variable.
"""

from __future__ import annotations

import json
import os
import subprocess
from collections.abc import Callable, Mapping
from pathlib import Path

import pytest

PATH_ENV = "OCX_TEST_PREVIOUS_OCX"
OPT_OUT_ENV = "OCX_TEST_NO_PREVIOUS_OCX"
GROUP = "test"

_PROJECT = Path(__file__).resolve().parents[2]
_NAME = "ocx.exe" if os.name == "nt" else "ocx"
#: A cold `ocx env` may pull the locked package; a hung registry must not hang the suite.
_LOOKUP_TIMEOUT_SECONDS = 300
_STDOUT_HEAD_CHARS = 400
_STDERR_TAIL_LINES = 10


class LookupFailure(Exception):
    """The toolchain lookup ran and failed; the message carries why."""


def toolchain_binary() -> Path | None:
    """The `ocx` the project toolchain's `test` group composes onto `PATH`, or None.

    Asked of the ocx under test, as the cosign fixture asks for its pinned tool:
    `ocx env` materialises the locked package on a miss, and a failure to do so
    is a `LookupFailure`, which `resolve` reports with the instruction to pull.
    """
    ocx = os.environ.get("OCX_COMMAND") or str(_PROJECT / "test" / "bin" / "ocx")
    try:
        result = subprocess.run(
            [ocx, "--format", "json", "--project", str(_PROJECT / "ocx.toml"), "env", "-g", GROUP],
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
            timeout=_LOOKUP_TIMEOUT_SECONDS,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise LookupFailure(f"`ocx env -g {GROUP}` timed out after {_LOOKUP_TIMEOUT_SECONDS}s") from exc
    if result.returncode != 0:
        tail = "\n".join(result.stderr.strip().splitlines()[-_STDERR_TAIL_LINES:])
        raise LookupFailure(f"`ocx env -g {GROUP}` exited {result.returncode}:\n{tail}")
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        head = result.stdout.strip()[:_STDOUT_HEAD_CHARS]
        raise LookupFailure(f"`ocx env -g {GROUP}` printed output that is not JSON:\n{head}") from exc
    for entry in report.get("items") or []:
        if entry.get("kind") == "path":
            candidate = Path(entry["value"]) / _NAME
            if os.access(candidate, os.X_OK):
                return candidate
    return None


def resolve(
    environ: Mapping[str, str] | None = None,
    *,
    toolchain: Callable[[], Path | None] = toolchain_binary,
) -> Path:
    """The previous-release `ocx` path. Fails when there is none, unless the leg opted out."""
    env = os.environ if environ is None else environ
    configured = env.get(PATH_ENV)
    if configured:
        path = Path(configured)
        if not os.access(path, os.X_OK):
            pytest.fail(f"{PATH_ENV}={configured} is not an executable file")
        return path
    lookup_failure = ""
    try:
        found = toolchain()
    except LookupFailure as exc:
        found, lookup_failure = None, f"\nThe toolchain lookup failed: {exc}"
    if found is not None:
        return found
    if env.get(OPT_OUT_ENV) == "1":
        pytest.skip(f"no previous-release ocx, and {OPT_OUT_ENV}=1 says this leg has none")
    pytest.fail(
        f"no previous-release ocx: {PATH_ENV} is unset and the toolchain's `{GROUP}` group has none materialised. "
        f"Run `ocx pull -g {GROUP}` at the repository root, or set {OPT_OUT_ENV}=1 on a leg that carries no `{GROUP}` group."
        + lookup_failure
    )
