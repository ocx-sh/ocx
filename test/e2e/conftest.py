"""Fixtures for the dispatched public end-to-end run (`ocx-sh/e2e-index` `client-e2e.yml`).

Not collected by `task test` (`testpaths = ["tests"]`) nor by a Bazel target. The
dispatcher sets `OCX_E2E_BINARY` to the built `ocx`; nothing here starts a registry.
"""

from __future__ import annotations

import os
import subprocess
from collections.abc import Callable
from pathlib import Path

import pytest

REGISTRY = "e2e.ocx.sh"
INDEX_URL = "https://ocx-sh.github.io/e2e-index"

Run = Callable[..., subprocess.CompletedProcess[str]]


@pytest.fixture(scope="session")
def e2e_binary() -> Path:
    raw = os.environ.get("OCX_E2E_BINARY")
    if not raw:
        if os.environ.get("CI") or os.environ.get("GITHUB_ACTIONS"):
            pytest.fail("OCX_E2E_BINARY unset on CI: a skipped e2e run reads as green")
        pytest.skip("OCX_E2E_BINARY unset: the public e2e run is dispatched by ocx-sh/e2e-index client-e2e.yml")
    return Path(raw)


@pytest.fixture()
def run_ocx(e2e_binary: Path, tmp_path: Path) -> Run:
    """Run `ocx --format json ...` against a fresh home whose config points the e2e registry at the public index."""

    def run(*args: str, signatures: str | None = None) -> subprocess.CompletedProcess[str]:
        home = tmp_path / "ocx-home"
        home.mkdir(exist_ok=True)
        lines = [f'[registries."{REGISTRY}"]', f'index = "{INDEX_URL}"']
        if signatures is not None:
            lines.append(f'signatures = "{signatures}"')
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
