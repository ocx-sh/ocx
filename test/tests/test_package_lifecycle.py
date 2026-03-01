"""End-to-end lifecycle: create, push, index update, install, find."""

import json
import stat
import sys
from pathlib import Path
from uuid import uuid4

from src import OcxRunner, current_platform


def test_create_push_install_find(ocx: OcxRunner, unique_repo: str, tmp_path: Path):
    """ocx package create; ocx package push; ocx index update; ocx install; ocx find"""
    tag = "0.1.0"
    plat = current_platform()
    marker = f"lifecycle-{uuid4().hex[:8]}"

    # --- Create package content ---
    pkg_dir = tmp_path / "pkg"
    bin_dir = pkg_dir / "bin"
    bin_dir.mkdir(parents=True)

    hello = bin_dir / "hello"
    if sys.platform == "win32":
        hello = hello.with_suffix(".bat")
        hello.write_text(f"@echo {marker}\n")
    else:
        hello.write_text(f"#!/bin/sh\necho {marker}\n")
        hello.chmod(hello.stat().st_mode | stat.S_IEXEC)

    metadata = tmp_path / "metadata.json"
    metadata.write_text(
        json.dumps(
            {
                "type": "bundle",
                "version": 1,
                "env": [
                    {
                        "key": "PATH",
                        "type": "path",
                        "required": True,
                        "value": "${installPath}/bin",
                    },
                ],
            }
        )
    )

    # --- Create bundle ---
    bundle = tmp_path / "bundle.tar.xz"
    ocx.plain("package", "create", "-m", str(metadata), "-o", str(bundle), str(pkg_dir))
    assert bundle.exists()

    # --- Push ---
    fq = f"{ocx.registry}/{unique_repo}:{tag}"
    ocx.plain("package", "push", "-n", "-p", plat, "-m", str(metadata), fq, str(bundle))

    # --- Index update ---
    short = f"{unique_repo}:{tag}"
    ocx.plain("index", "update", short)

    # --- Install ---
    result = ocx.json("install", short)
    content = result[short]["content"]
    assert Path(content).is_dir()

    # --- Find ---
    find_result = ocx.json("find", short)
    assert find_result[short] == content
