from __future__ import annotations

import dataclasses
import json
import os
import platform
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


# ---------------------------------------------------------------------------
# Platform helpers
# ---------------------------------------------------------------------------

_ARCH_MAP = {
    "x86_64": "amd64",
    "amd64": "amd64",
    "aarch64": "arm64",
    "arm64": "arm64",
}


def current_platform() -> str:
    """Return the current platform in OCI format (e.g. ``linux/amd64``)."""
    system = platform.system().lower()
    machine = platform.machine().lower()
    arch = _ARCH_MAP.get(machine, machine)
    return f"{system}/{arch}"


def registry_dir(registry: str) -> str:
    """Filesystem-safe registry name (mirrors ocx's relaxed-slug: keep ``[a-zA-Z0-9._-]``)."""
    return re.sub(r"[^a-zA-Z0-9._-]", "_", registry)


# ---------------------------------------------------------------------------
# PackageInfo
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class PackageInfo:
    """Metadata for a test package that has been pushed to the registry."""

    repo: str
    tag: str
    short: str
    fq: str
    content_dir: Path
    marker: str
    platform: str


# ---------------------------------------------------------------------------
# OcxRunner
# ---------------------------------------------------------------------------


class OcxRunner:
    """Wraps the ocx binary with per-test environment isolation.

    Each instance carries its own minimal environment so tests never
    leak host state into ocx.
    """

    def __init__(self, binary: Path, ocx_home: Path, registry: str):
        self.binary = binary
        self.registry = registry
        self.ocx_home = ocx_home
        self.env: dict[str, str] = {
            "OCX_HOME": str(ocx_home),
            "OCX_DEFAULT_REGISTRY": registry,
            "OCX_INSECURE_REGISTRIES": registry,
            "PATH": os.environ.get("PATH", ""),
            "HOME": os.environ.get("HOME", str(Path.home())),
        }
        # Windows needs these for subprocess spawning
        for key in ("SYSTEMROOT", "TEMP", "TMP"):
            if key in os.environ:
                self.env[key] = os.environ[key]

    def run(
        self,
        *args: str,
        format: str | None = "json",
        check: bool = True,
    ) -> subprocess.CompletedProcess[str]:
        """Run ocx with the given arguments."""
        cmd: list[str] = [str(self.binary)]
        if format:
            cmd += ["--format", format]
        cmd += list(args)
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            env=self.env,
        )
        if check and result.returncode != 0:
            raise AssertionError(
                f"ocx {' '.join(args)} failed (rc={result.returncode})\n"
                f"stderr: {result.stderr.strip()}"
            )
        return result

    def json(self, *args: str, **kwargs: Any) -> Any:
        """Run ocx and parse stdout as JSON."""
        result = self.run(*args, format="json", **kwargs)
        return json.loads(result.stdout)

    def plain(self, *args: str, **kwargs: Any) -> subprocess.CompletedProcess[str]:
        """Run ocx without ``--format`` (plain text output)."""
        return self.run(*args, format=None, **kwargs)
