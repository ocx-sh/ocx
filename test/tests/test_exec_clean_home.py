"""Acceptance tests for `--clean` and `$OCX_HOME` (ocx-sh/ocx#488).

`ocx package exec --clean` starts the child environment empty and strips `HOME` with it. A child ocx
used to re-derive `~/.ocx` from the passwd database, so a generated launcher's `ocx launcher exec`
re-entry read another configuration chain and missed the package, surfacing as exit 78 under a
managed-config tier. The home now travels in the execution state's plaintext header (`h=`), with no
public `OCX_HOME` written, so every frame of a launch chain resolves the same home. `HOME` stays
stripped, and the OS-level fallback still resolves docker credentials. Sibling of
`test_exec_forwarding.py`.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from src import exec_state
from src.helpers import make_package_with_entrypoints, push_managed_config
from src.runner import OcxRunner, PackageInfo

pytestmark = pytest.mark.command("exec")

_POSIX_ONLY = "the `--clean` home contract is exercised through POSIX shell helpers; the Rust surface is covered by unit tests on Windows"


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_clean_child_finds_the_parent_ocx_home_through_the_state_header(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """A `--clean` child holds neither `OCX_HOME` nor `HOME`; the parent's absolute home
    rides in the state header."""
    ocx.plain("package", "install", published_package.short)
    result = ocx.plain(
        "package",
        "exec",
        "--clean",
        published_package.short,
        "--",
        # `--clean` drops the inherited PATH and exposes only the package's
        # own bin dir, so the interpreter is named absolutely -- the same
        # reason `test_exec_forwarding.py` spells `/bin/sh` out.
        "/bin/sh",
        "-c",
        'printf "%s\\n%s\\n%s" "${OCX_HOME-unset}" "${HOME-unset}" "$__OCX_ENV_STATE"',
        check=False,
    )
    assert result.returncode == 0, result.stderr
    home, outer_home, state_text = result.stdout.split("\n", 2)
    assert (home, outer_home) == ("unset", "unset"), (
        "a --clean child must hold no OCX_HOME and no HOME; "
        f"got OCX_HOME={home!r} HOME={outer_home!r}"
    )
    state = exec_state.decode(state_text)
    assert isinstance(state, dict), (
        f"the child must receive a readable state; got {state_text!r}"
    )
    assert Path(state["home"]).resolve() == Path(ocx.ocx_home).resolve(), (
        f"the parent's $OCX_HOME must travel in the header; got {state['home']!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_clean_child_ocx_reads_the_home_from_the_header(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """An ocx started inside a `--clean` child resolves the parent's home.

    The pin is the only way to name ocx in there, and `ocx shell state` reports the home
    the process resolved.
    """
    ocx.plain("package", "install", published_package.short)
    result = subprocess.run(
        [
            str(ocx.binary),
            "package",
            "exec",
            "--clean",
            published_package.short,
            "--",
            "/bin/sh",
            "-c",
            '"$OCX_BINARY_PIN" shell state',
        ],
        capture_output=True,
        text=True,
        env=dict(ocx.env),
        stdin=subprocess.DEVNULL,
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert f"ocx home: {Path(ocx.ocx_home).resolve()}" in result.stdout, (
        f"the inner ocx must resolve the parent's home from the header; got {result.stdout!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_clean_launcher_reentry_finds_the_managed_tier_in_the_same_home(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The exit-78 regression guard: a launcher re-entry under `--clean` resolves the managed
    snapshot the parent adopted.

    The managed tier fails closed when its snapshot is absent, and the snapshot lives under
    `$OCX_HOME`, so the inner `ocx launcher exec` exits 78 once it resolves a home the parent never
    wrote to. The parent's `OCX_MANAGED_CONFIG` reaches it as the state's `managed_config` policy.
    The payload names the registry the runner defaults to, so the tier is observable without
    changing what resolves.
    """
    managed_repo = f"{unique_repo}_managed"
    push_managed_config(
        ocx, managed_repo, "v1", f'[registry]\ndefault = "{ocx.registry}"\n', tmp_path
    )
    ref = f"{ocx.registry}/{managed_repo}:v1"
    ocx.run("config", "update", env_overrides={"OCX_MANAGED_CONFIG": ref})

    pkg = make_package_with_entrypoints(
        ocx, unique_repo, tmp_path, entrypoints=["hello"], bins=["hello"]
    )
    ocx.plain("package", "install", "--select", pkg.short)

    result = ocx.plain(
        "package",
        "exec",
        "--clean",
        pkg.short,
        "--",
        "hello",
        env_overrides={"OCX_MANAGED_CONFIG": ref},
        check=False,
    )
    assert result.returncode == 0, (
        "the launcher's `ocx launcher exec` re-entry must resolve the parent's "
        f"home under --clean; rc={result.returncode}, stderr={result.stderr!r}"
    )
    assert pkg.marker in result.stdout, (
        f"the launcher must reach the wrapped target; stdout={result.stdout!r}"
    )
