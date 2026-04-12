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


def _rewrite_command(cmd: str, repo_map: dict[str, str]) -> str:
    """Replace display-name package refs with actual repo names in OCX args.

    Skips the first word (the ``ocx`` command itself) and only rewrites the
    part before `` -- `` so binary names passed to ``ocx exec … -- <bin>``
    are left untouched.
    """
    if not repo_map:
        return cmd
    parts = cmd.split(" -- ", 1)
    ocx_part = parts[0]

    # Skip the command name (first word, always "ocx")
    first_space = ocx_part.find(" ")
    if first_space == -1:
        return cmd
    command_name = ocx_part[:first_space]
    args = ocx_part[first_space:]

    # Longest-first to avoid partial matches (e.g., "nodejs" before "node")
    for display, actual in sorted(repo_map.items(), key=lambda x: -len(x[0])):
        args = args.replace(display, actual)

    result = command_name + args
    if len(parts) > 1:
        return result + " -- " + parts[1]
    return result


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
    # Map actual repo names to display names and build reverse map
    repo_map: dict[str, str] = {}
    for display_name, packages in setup_env.items():
        for pkg in packages:
            if pkg.repo != display_name:
                sanitize_map[pkg.repo] = display_name
                if display_name not in repo_map:
                    repo_map[display_name] = pkg.repo

    # Binary path for substitution into actual commands
    binary_quoted = shlex.quote(str(ocx_binary))

    # Execute each command through the persistent shell
    for cmd in commands:
        # Show the sanitised command as typed
        display_cmd = cmd
        for old, new in sanitize_map.items():
            display_cmd = display_cmd.replace(old, new)

        # Rewrite package refs to UUID'd names, then substitute binary path
        actual_cmd = _rewrite_command(cmd, repo_map)
        actual_cmd = actual_cmd.replace("ocx", binary_quoted, 1)

        recorder.run_command(display_cmd, actual_cmd, timeout=120)
        recorder.pause(0.5)

    # Build, sanitize, truncate digests, and write
    cast_name = script["path"].stem
    (
        recorder.build(title=title)
        .strip_progress()
        .sanitize(sanitize_map)
        .truncate_digests()
        .realign_tables()
        .auto_height()
        .write(cast_dir / f"{cast_name}.cast")
    )
