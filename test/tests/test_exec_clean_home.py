"""Acceptance tests for `--clean` and `$OCX_HOME` (ocx-sh/ocx#488).

`ocx package exec --clean` starts the child environment empty, and it strips
`HOME` along with everything else. A child ocx therefore used to re-derive
`~/.ocx` from the passwd database: a generated entrypoint launcher's
`ocx launcher exec` re-entry read a different configuration chain and looked
for the package in a store that never held it, which surfaced as exit 78 when
a managed-config tier was in force.

`Env::apply_ocx_config` now writes `OCX_HOME` onto every child it composes, so
each frame of one launch chain resolves against the same home. `HOME` stays
stripped — that is the point of `--clean`, and the OS-level fallback still
resolves docker credentials for any account with a passwd home.

Sibling of `test_exec_forwarding.py`, which covers the other forwarded keys.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

from src.helpers import make_package_with_entrypoints, push_managed_config
from src.runner import OcxRunner, PackageInfo

_POSIX_ONLY = "the `--clean` home contract is exercised through POSIX shell helpers; the Rust surface is covered by unit tests on Windows"


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_clean_child_carries_the_parent_ocx_home(
    ocx: OcxRunner, published_package: PackageInfo
) -> None:
    """A `--clean` child sees the parent's absolute `$OCX_HOME`, and no `HOME`."""
    ocx.plain("package", "install", published_package.short)
    result = ocx.plain(
        "package",
        "exec",
        "--clean",
        published_package.short,
        "--",
        # `--clean` drops the inherited PATH and exposes only the package's
        # own bin dir, so the interpreter is named absolutely — the same
        # reason `test_exec_forwarding.py` spells `/bin/sh` out.
        "/bin/sh",
        "-c",
        'printf "home=%s outer=%s" "$OCX_HOME" "${HOME-unset}"',
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == f"home={ocx.ocx_home} outer=unset", (
        "a --clean child must land in the parent's $OCX_HOME with HOME still "
        f"stripped; got {result.stdout!r}"
    )


@pytest.mark.skipif(sys.platform == "win32", reason=_POSIX_ONLY)
def test_clean_launcher_reentry_finds_the_managed_tier_in_the_same_home(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path
) -> None:
    """The exit-78 regression guard: a launcher re-entry under `--clean`
    resolves the managed snapshot the parent adopted.

    The managed tier fails closed when its snapshot is absent, and the
    snapshot lives under `$OCX_HOME`. The inner `ocx launcher exec` therefore
    exits 78 the moment it resolves a home the parent never wrote to — which
    is exactly what a `--clean` child did before `OCX_HOME` was forwarded.
    The payload names the registry the runner already defaults to, so the tier
    is observable without changing what anything resolves.
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
