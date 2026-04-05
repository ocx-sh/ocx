from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from uuid import uuid4

from src.runner import OcxRunner, PackageInfo, current_platform

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

PROJECT_ROOT = Path(__file__).resolve().parent.parent.parent
COMPOSE_FILE = Path(__file__).resolve().parent.parent / "docker-compose.yml"

# ---------------------------------------------------------------------------
# Docker-compose helpers
# ---------------------------------------------------------------------------


def registry_is_reachable(registry: str) -> bool:
    """Return True if the registry responds to ``GET /v2/``."""
    try:
        urllib.request.urlopen(f"http://{registry}/v2/", timeout=2)
        return True
    except (urllib.error.URLError, OSError):
        return False


def start_registry(registry: str) -> None:
    """Start the registry via docker-compose if it is not already running."""
    if registry_is_reachable(registry):
        return

    subprocess.run(
        ["docker", "compose", "-f", str(COMPOSE_FILE), "up", "-d"],
        check=True,
        capture_output=True,
    )

    # Wait for the registry to become reachable (up to 15 s).
    for _ in range(30):
        if registry_is_reachable(registry):
            return
        time.sleep(0.5)

    raise RuntimeError(f"Registry at {registry} did not become reachable")


# ---------------------------------------------------------------------------
# Package publishing
# ---------------------------------------------------------------------------


def _build_trap_script(outputs: dict[str, str], marker: str) -> str:
    """Build an argument-aware trap shell script.

    ``outputs`` maps argument strings to the exact output the binary should
    produce (e.g. ``{"--version": "uv 0.10.10"}``).  Multi-line values are
    emitted via heredocs.
    """
    lines = ["#!/bin/sh", 'case "$*" in']
    for args, output in outputs.items():
        lines.append(f'  "{args}")')
        if "\n" in output:
            lines.append("    cat <<'TRAP_EOF'")
            lines.append(output)
            lines.append("TRAP_EOF")
        else:
            lines.append(f'    echo "{output}"')
        lines.append("    ;;")
    # Fallback: echo marker for acceptance tests
    lines.append("  *)")
    lines.append(f'    echo "{marker}"')
    lines.append("    ;;")
    lines.append("esac")
    return "\n".join(lines) + "\n"


def make_package(
    ocx: OcxRunner,
    repo: str,
    tag: str,
    tmp_path: Path,
    *,
    new: bool = True,
    cascade: bool = True,
    size_mb: int = 0,
    platform: str | None = None,
    bins: list[str] | None = None,
    env: list[dict] | None = None,
    outputs: dict[str, dict[str, str]] | None = None,
    dependencies: list[dict] | None = None,
) -> PackageInfo:
    """Create, bundle, push, and index a test package.

    Parameters
    ----------
    size_mb:
        Approximate size in MB of random padding data.  Useful for making
        downloads large enough to show progress bars.
    platform:
        OCI platform string (e.g. ``linux/amd64``).  Defaults to the
        current host platform.
    bins:
        List of binary names to create.  Each gets a shell script that
        echoes a unique marker.  Defaults to ``["hello"]``.
    env:
        Custom metadata env entries.  Defaults to PATH + ``{REPO}_HOME``
        (derived from the repo name, e.g. ``cmake`` → ``CMAKE_HOME``).
    outputs:
        Maps binary name to ``{args: output}`` pairs.  When provided, the
        trap binary uses a ``case`` block to reproduce exact command output
        for specific argument patterns.  Multi-line output uses heredocs.
    """
    plat = platform or current_platform()
    marker = f"marker-{uuid4().hex[:12]}"
    bin_names = bins or ["hello"]

    # Build content
    pkg_dir = tmp_path / f"pkg-{repo}-{tag}"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)

    for name in bin_names:
        script = bin_dir / name
        bin_outputs = (outputs or {}).get(name)
        if sys.platform == "win32":
            script = script.with_suffix(".bat")
            script.write_text(f"@echo {marker}\n")
        elif bin_outputs:
            script.write_text(_build_trap_script(bin_outputs, marker))
            script.chmod(script.stat().st_mode | stat.S_IEXEC)
        else:
            script.write_text(f"#!/bin/sh\necho {marker}\n")
            script.chmod(script.stat().st_mode | stat.S_IEXEC)

    # Add random padding for realistic download sizes
    if size_mb > 0:
        lib_dir = pkg_dir / "lib"
        lib_dir.mkdir(parents=True)
        data_file = lib_dir / "data.bin"
        data_file.write_bytes(os.urandom(size_mb * 1024 * 1024))

    # Write metadata
    metadata_path = tmp_path / f"metadata-{repo}-{tag}.json"
    home_key = repo.upper().replace("-", "_") + "_HOME"
    metadata_env = env or [
        {
            "key": "PATH",
            "type": "path",
            "required": True,
            "value": "${installPath}/bin",
        },
        {
            "key": home_key,
            "type": "constant",
            "value": "${installPath}",
        },
    ]
    metadata_obj: dict = {
        "type": "bundle",
        "version": 1,
        "env": metadata_env,
    }
    if dependencies:
        metadata_obj["dependencies"] = dependencies
    metadata_path.write_text(json.dumps(metadata_obj))

    # Create bundle
    bundle = tmp_path / f"bundle-{repo}-{tag}.tar.xz"
    ocx.plain(
        "package",
        "create",
        "-m",
        str(metadata_path),
        "-o",
        str(bundle),
        str(pkg_dir),
    )

    # Push
    fq = f"{ocx.registry}/{repo}:{tag}"
    push_args = ["package", "push", "-p", plat, "-m", str(metadata_path)]
    if new:
        push_args.append("-n")
    if cascade:
        push_args.append("--cascade")
    push_args += [fq, str(bundle)]
    ocx.plain(*push_args)

    # Update local index so install/find can discover the package.
    # When cascade is enabled, use bare repo name to index all cascaded tags;
    # when disabled, use tagged identifier for minimal indexing.
    short = f"{repo}:{tag}"
    index_target = repo if cascade else short
    ocx.plain("index", "update", index_target)

    return PackageInfo(
        repo=repo,
        tag=tag,
        short=short,
        fq=fq,
        content_dir=pkg_dir,
        marker=marker,
        platform=plat,
    )
