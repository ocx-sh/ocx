"""Generic test that runs each shell script scenario as a recording.

For each .sh file in recordings/scripts/:
1. Provisions the setup environment (publishes packages)
2. Executes each command through a persistent bash shell in a PTY
3. Sanitizes output (tmp paths, registry, repo names)
4. Writes the .cast file to the website casts directory
"""
from __future__ import annotations

import shlex
from pathlib import Path

from src.runner import OcxRunner, PackageInfo, registry_dir

from recordings.cast_recorder import CastRecorder


def test_record(
    script: dict,
    ocx: OcxRunner,
    ocx_binary: Path,
    recorder: CastRecorder,
    setup_env: dict[str, list[PackageInfo]],
    cast_dir: Path,
    registry: str,
    ocx_home: Path,
) -> None:
    meta = script["meta"]
    commands = script["commands"]
    title = meta.get("title", script["path"].stem)

    # Build the sanitization map
    registry_slug = registry_dir(registry)
    sanitize_map = {
        str(ocx_home): "~/.ocx",
        registry + "/": "",
        registry_slug + "/": "",
    }
    # Map actual repo names to display names and sanitize markers
    for display_name, packages in setup_env.items():
        for pkg in packages:
            if pkg.repo != display_name:
                sanitize_map[pkg.repo] = display_name
            sanitize_map[pkg.marker] = f"Hello from {display_name}!"

    # Binary path for substitution into actual commands
    binary_quoted = shlex.quote(str(ocx_binary))

    # Execute each command through the persistent shell
    for cmd in commands:
        # Show the sanitised command as typed
        display_cmd = cmd
        for old, new in sanitize_map.items():
            display_cmd = display_cmd.replace(old, new)

        # Build actual command (replace "ocx" with real binary path)
        actual_cmd = cmd.replace("ocx", binary_quoted, 1)

        recorder.run_command(display_cmd, actual_cmd, timeout=120)
        recorder.pause(0.5)

    # Build, sanitize, stretch progress bars, truncate digests, and write
    cast_name = script["path"].stem
    (
        recorder.build(title=title)
        .sanitize(sanitize_map)
        .stretch_progress()
        .truncate_digests()
        .realign_tables()
        .auto_height()
        .write(cast_dir / f"{cast_name}.cast")
    )
