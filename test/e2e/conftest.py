"""Fixtures for the dispatched public end-to-end run (`ocx-sh/e2e-index` `client-e2e.yml`).

Not collected by `task test` (`testpaths = ["tests"]`) nor by a Bazel target. The
dispatcher sets `OCX_E2E_BINARY` to the built `ocx`; nothing here starts a registry.
"""

from __future__ import annotations

import json
import os
import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

REGISTRY = "e2e.ocx.sh"
# Overridable so a case can be proven red and green against a local fake root.
INDEX_URL = os.environ.get("OCX_E2E_INDEX_URL", "https://ocx-sh.github.io/e2e-index")

Run = Callable[..., subprocess.CompletedProcess[str]]


@pytest.fixture(scope="session")
def e2e_binary() -> Path:
    raw = os.environ.get("OCX_E2E_BINARY")
    if not raw:
        if os.environ.get("CI") or os.environ.get("GITHUB_ACTIONS"):
            pytest.fail("OCX_E2E_BINARY unset on CI: a skipped e2e run reads as green")
        pytest.skip("OCX_E2E_BINARY unset: the public e2e run is dispatched by ocx-sh/e2e-index client-e2e.yml")
    return Path(raw)


@pytest.fixture(scope="session")
def new_client(e2e_binary: Path) -> None:
    """Skip unless the client enforces declared publishers (verify/sbom resolve the root's identities).

    The probe is the `contract` the client reports for its own `package verify`: released <= 0.6.6 reports 1,
    the declared-publisher client reports 2. A version compare cannot separate them: this branch's Cargo
    version is still 0.6.6, and a `__testing` Bazel build reports the same version on channel `test`.
    """
    out = subprocess.run([str(e2e_binary), "--format", "json", "version"], capture_output=True, encoding="utf-8", check=True).stdout
    # Every client back to v0.6.6 reports the key; absent means the `version` shape moved, and a default would skip silently.
    try:
        contract = json.loads(out)["contract"]["commands"]["package verify"]
    except (KeyError, TypeError):
        pytest.fail("`ocx version` no longer reports contract.commands['package verify']; update the new_client probe")
    if contract < 2:
        pytest.skip("needs a client with declared-publisher verification (`package verify` contract >= 2); this one reports 1")


@pytest.fixture()
def run_ocx(e2e_binary: Path, tmp_path: Path) -> Run:
    """Run `ocx --format json ...` against a fresh home whose config points the e2e registry at the public index."""

    def run(*args: str, signatures: str | None = None) -> subprocess.CompletedProcess[str]:
        home = tmp_path / "ocx-home"
        home.mkdir(exist_ok=True)
        lines = [f'[registries."{REGISTRY}"]', f'index = "{INDEX_URL}"']
        if signatures is not None:
            lines.append(f'signatures = "{signatures}"')
        if INDEX_URL.startswith("http://"):  # a local fake root: the client refuses plain HTTP unless the host is insecure
            lines += [f'[registries."{INDEX_URL.removeprefix("http://").rstrip("/")}"]', "insecure = true"]
        config = tmp_path / f"config-{signatures}.toml"
        config.write_text("\n".join(lines) + "\n", encoding="utf-8")
        env = {
            "OCX_HOME": str(home),
            "OCX_CONFIG": str(config),
            "XDG_CONFIG_HOME": str(home / "xdg"),
            "PATH": os.environ.get("PATH", ""),
            "HOME": os.environ.get("HOME", str(Path.home())),
        }
        for key in ("SYSTEMROOT", "TEMP", "TMP", "PATHEXT"):
            if key in os.environ:
                env[key] = os.environ[key]
        return subprocess.run(
            [str(e2e_binary), "--format", "json", *args],
            capture_output=True,
            encoding="utf-8",
            env=env,
            stdin=subprocess.DEVNULL,
            check=False,
        )

    return run
