from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any


class MirrorRunner:
    """Wraps the ocx-mirror binary with per-test environment isolation."""

    def __init__(self, binary: Path, registry: str, temp_dir: Path):
        self.binary = binary
        self.registry = registry
        self.temp_dir = temp_dir
        self.env: dict[str, str] = {
            "OCX_INSECURE_REGISTRIES": registry,
            "PATH": __import__("os").environ.get("PATH", ""),
            "HOME": __import__("os").environ.get("HOME", str(Path.home())),
        }

    def run(
        self,
        *args: str,
        check: bool = True,
    ) -> subprocess.CompletedProcess[str]:
        """Run ocx-mirror with the given arguments."""
        cmd = [str(self.binary)] + list(args)
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            env=self.env,
        )
        if check and result.returncode != 0:
            raise AssertionError(
                f"ocx-mirror {' '.join(args)} failed (rc={result.returncode})\n"
                f"stdout: {result.stdout.strip()}\n"
                f"stderr: {result.stderr.strip()}"
            )
        return result
