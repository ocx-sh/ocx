# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Contract-level checks of the usage telemetry, read on the wire.

Each test reads what a capture OTLP collector received, so it proves what
leaves the machine rather than what ocx meant to build. Every "nothing is
present" assertion carries a positive control in the same test.
``OcxRunner`` builds its environment from scratch, so no ambient ``OTEL_*``,
``OCX_TELEMETRY*``, ``DO_NOT_TRACK`` or ``CI`` reaches the child. Setup
commands run unarmed.
"""

from __future__ import annotations

import getpass
import json
import re
import subprocess
import sys
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from src.helpers import make_package
from src.otlp_fixture import OtlpCollector
from src.runner import OcxRunner
from src.telemetry_helpers import armed, package_with_dependency

pytestmark = pytest.mark.command("install", "uninstall", "about", "update")

REPO_ROOT = Path(__file__).resolve().parents[2]
TELEMETRY_DOC = REPO_ROOT / "website/src/content/docs/reference/telemetry.md"
PACKAGE = "sh.ocx.package"
COMMAND = "sh.ocx.command"
EVENT_GROUP = {"command": COMMAND, "package": PACKAGE}

GITHUB_ENV = {"GITHUB_ACTIONS": "true", "GITHUB_RUN_ID": "11", "GITHUB_JOB": "build"}
GITLAB_ENV = {
    "GITLAB_CI": "true",
    "CI_PIPELINE_ID": "21",
    "CI_JOB_ID": "22",
    "CI_JOB_NAME": "test",
}


def documented_fields() -> dict[str, dict[str, str]]:
    """``{group: {key: presence}}`` from the ``{#fields}`` section of telemetry.md."""
    text = TELEMETRY_DOC.read_text(encoding="utf-8")
    start = text.index("{#fields}")
    end = text.find("\n## ", start)
    section = text[start : end if end != -1 else len(text)]
    groups: dict[str, dict[str, str]] = {}
    group: str | None = None
    presence_col = 0
    for line in section.splitlines():
        if heading := re.match(r"###\s.*\{#fields-(\w+)\}", line):
            group = heading.group(1)
            groups[group] = {}
        elif group and line.startswith("|"):
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            if cells[0] == "Key":
                presence_col = cells.index("Presence")
            elif key := re.fullmatch(r"`([^`]+)`", cells[0]):
                groups[group][key.group(1)] = cells[presence_col]
    return groups


def strings_of(value: Any) -> Iterator[str]:
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from strings_of(item)
    elif isinstance(value, dict):
        for key, item in value.items():
            yield from strings_of(key)
            yield from strings_of(item)


def wire_strings(export: dict[str, Any]) -> list[str]:
    """Every string a capture carries: resource, scope, event names, record attributes."""
    found = list(strings_of(export["resource"])) + list(strings_of(export["scope"]))
    for record in export["records"]:
        found.append(record["event_name"])
        found.extend(strings_of(record["attributes"]))
    return found


# --------------------------------------------------------------------------- #
# The documented field tables are the wire
# --------------------------------------------------------------------------- #


def test_wire_keys_match_documented_field_table(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    table = documented_fields()
    assert set(table) == {"resource", "command", "package"}, sorted(table)
    always = {
        g: {k for k, p in rows.items() if p.startswith("always")}
        for g, rows in table.items()
    }
    assert all(always.values()), always

    root, _dep = package_with_dependency(ocx, unique_repo, tmp_path)
    missing = f"{ocx.registry}/{root.repo}:9.9.9"

    def run(*args: str, env: dict[str, str] | None = None, check: bool = True) -> int:
        proc = ocx.run(
            *args, env_overrides=armed(otlp_collector, **(env or {})), check=check
        )
        return proc.returncode

    run(
        "package", "install", root.short, env=GITHUB_ENV
    )  # fetched root + dependency, GitHub ids
    run("package", "install", root.short)  # cached: store_hit true
    assert run("package", "install", missing, check=False) == 79  # error.type
    run("package", "uninstall", root.short)  # removed
    run("about", env=GITLAB_ENV)  # GitLab ids, including the task run id

    exports = otlp_collector.exports
    assert len(exports) == 5
    commands = [
        r["attributes"]
        for e in exports
        for r in e["records"]
        if r["event_name"] == COMMAND
    ]
    packages = [
        r["attributes"]
        for e in exports
        for r in e["records"]
        if r["event_name"] == PACKAGE
    ]
    assert len(commands) == 5 and len(packages) == 4, (len(commands), len(packages))
    assert all(
        r["event_name"] for e in exports for r in e["records"]
    )  # event_name set on every record

    captures = {
        "resource": [e["resource"] for e in exports],
        "command": commands,
        "package": packages,
    }
    for group, records in captures.items():
        observed: set[str] = set()
        for keys in records:
            assert set(keys) <= set(table[group]), (
                f"undocumented {group} keys: {set(keys) - set(table[group])}"
            )
            assert always[group] <= set(keys), (
                f"missing always-present {group} keys: {always[group] - set(keys)}"
            )
            observed |= set(keys)
        expected = set(table[group])
        if group == "resource" and sys.platform == "win32":
            expected.discard("process.parent.pid")
        assert observed == expected, (
            f"documented {group} keys never sent: {expected - observed}"
        )


# --------------------------------------------------------------------------- #
# Never collected
# --------------------------------------------------------------------------- #


def test_never_collected(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    pkg = make_package(ocx, f"{unique_repo}_nc", "1.0.0", tmp_path)
    workdir = tmp_path / "SENTINELCWD"
    workdir.mkdir()
    env = {
        **ocx.env,
        **armed(
            otlp_collector,
            OTEL_EXPORTER_OTLP_HEADERS="authorization=Bearer SENTINELTOKEN",
        ),
        "SENTINEL_ENV_VAR": "sentinelenvvalue",
    }

    def run(*args: str) -> None:
        subprocess.run(
            [str(ocx.binary), "--format", "json", *args],
            cwd=workdir,
            env=env,
            capture_output=True,
            encoding="utf-8",
            stdin=subprocess.DEVNULL,
            check=False,
        )

    run("package", "install", pkg.short)  # rich capture: a package record
    run(
        "package", "install", f"{ocx.registry}/{pkg.repo}:sentinelargvtag"
    )  # argv value, failing command

    exports = otlp_collector.exports
    assert len(exports) == 2
    assert any(
        r["event_name"] == PACKAGE for r in exports[0]["records"]
    )  # control: the rich one
    found = [s for e in exports for s in wire_strings(e)]
    assert len(found) > 20

    for export in exports:
        keys = set(export["resource"]) | {
            k for r in export["records"] for k in r["attributes"]
        }
        assert not {k for k in keys if k.startswith("telemetry.sdk")}, keys
    banned = [
        "SENTINELCWD",
        "sentinelargvtag",
        "sentinelenvvalue",
        "SENTINEL_ENV_VAR",
        "SENTINELTOKEN",
        str(tmp_path),
        str(tmp_path.resolve()),
        str(ocx.ocx_home),
    ]
    for value in found:
        for needle in banned:
            assert needle not in value, f"{needle!r} leaked in {value!r}"
        assert value != getpass.getuser(), f"user name leaked in {value!r}"
        assert not value.startswith("/") or value == "/", (
            f"a file path leaked: {value!r}"
        )


# --------------------------------------------------------------------------- #
# The debug line is the wire
# --------------------------------------------------------------------------- #


def test_debug_line_matches_wire(
    ocx: OcxRunner, unique_repo: str, tmp_path: Path, otlp_collector: OtlpCollector
) -> None:
    root, _dep = package_with_dependency(ocx, unique_repo, tmp_path)

    proc = ocx.run(
        "package",
        "install",
        root.short,
        log_level="debug",
        env_overrides=armed(otlp_collector, **GITHUB_ENV),
    )

    lines = [ln for ln in proc.stderr.splitlines() if "telemetry payload " in ln]
    assert len(lines) == 1, proc.stderr
    debug = json.loads(lines[0].split("telemetry payload ", 1)[1])
    (export,) = otlp_collector.exports
    assert len(export["records"]) >= 3  # control: command record plus two packages
    assert debug["resource"] == export["resource"]
    assert debug["scope"] == export["scope"]
    assert [(r["event_name"], r["attributes"]) for r in debug["records"]] == [
        (r["event_name"], r["attributes"]) for r in export["records"]
    ]
