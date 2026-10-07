# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Two-way coverage between the environment reference page and the env registry.

The registry's published projection is the `env` section of
`crates/ocx_schema/tests/golden/cli.json`: every Public, Foreign and Plumbing
declaration. `environment.md` needs one heading per name and none for an
undeclared one. A `{SLOT}` name is matched literally, the page spelling it
`<SLOT>`. A heading may carry several names. A section opening with a
`> **REMOVED**` quote tombstones a name no longer read and is exempt from the
"extra" direction. Both floors keep an empty reader from passing in silence.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[2]
ENV_REF = PROJECT_ROOT / "website" / "src" / "docs" / "reference" / "environment.md"
CLI_GOLDEN = PROJECT_ROOT / "crates" / "ocx_schema" / "tests" / "golden" / "cli.json"

MIN_HEADING_NAMES = 50
MIN_REGISTRY_ENTRIES = 50

_HEADING = re.compile(r"^#{3,4}\s+(.*)$", re.MULTILINE)
_BACKTICKED = re.compile(r"`([^`]+)`")
_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_()<>{}]*$")


def registry_names(golden_text: str) -> set[str]:
    return {entry["name"] for entry in json.loads(golden_text)["env"]}


def heading_names(page: str) -> tuple[set[str], set[str]]:
    """The variable names the page's headings carry: (live, tombstoned)."""
    live: set[str] = set()
    tombstoned: set[str] = set()
    headings = list(_HEADING.finditer(page))
    for index, heading in enumerate(headings):
        end = headings[index + 1].start() if index + 1 < len(headings) else len(page)
        body = page[heading.end() : end].lstrip()
        names = {
            token.replace("<", "{").replace(">", "}")
            for token in _BACKTICKED.findall(heading.group(1))
            if _NAME.match(token)
        }
        (tombstoned if body.startswith("> **REMOVED**") else live).update(names)
    return live, tombstoned


def coverage_problems(page: str, registry: set[str]) -> list[str]:
    live, tombstoned = heading_names(page)
    problems = [f"no heading for `{name}`" for name in sorted(registry - live)]
    problems += [
        f"heading `{name}` is not in the env registry"
        for name in sorted(live - registry - tombstoned)
    ]
    if len(live | tombstoned) < MIN_HEADING_NAMES:
        problems.append(f"only {len(live | tombstoned)} heading names parsed (floor {MIN_HEADING_NAMES})")
    if len(registry) < MIN_REGISTRY_ENTRIES:
        problems.append(f"only {len(registry)} registry entries read (floor {MIN_REGISTRY_ENTRIES})")
    return problems


def _real() -> tuple[str, set[str]]:
    return ENV_REF.read_text(encoding="utf-8"), registry_names(CLI_GOLDEN.read_text(encoding="utf-8"))


def test_environment_reference_covers_the_registry_both_ways() -> None:
    page, registry = _real()
    assert coverage_problems(page, registry) == []


def test_a_removed_heading_is_red() -> None:
    page, registry = _real()
    mutated = page.replace("### `OCX_OFFLINE` {#ocx-offline}", "### Offline {#ocx-offline}")
    assert mutated != page, "mutation did not land"
    assert coverage_problems(mutated, registry) == ["no heading for `OCX_OFFLINE`"]


def test_an_undeclared_heading_is_red() -> None:
    page, registry = _real()
    mutated = page + "\n### `OCX_BOGUS` {#ocx-bogus}\n\nNot declared.\n"
    assert mutated != page, "mutation did not land"
    assert coverage_problems(mutated, registry) == ["heading `OCX_BOGUS` is not in the env registry"]


def test_a_tombstoned_heading_is_not_extra() -> None:
    page, registry = _real()
    mutated = page + "\n### `OCX_GONE` {#ocx-gone}\n\n> **REMOVED** — no longer read.\n"
    assert mutated != page, "mutation did not land"
    assert coverage_problems(mutated, registry) == []


def test_a_short_parse_is_red() -> None:
    page, registry = _real()
    mutated = "\n".join(page.splitlines()[:300])
    assert mutated != page, "mutation did not land"
    problems = coverage_problems(mutated, registry)
    assert any("heading names parsed" in problem for problem in problems), problems


def test_the_registry_floor_is_red_when_the_golden_is_empty() -> None:
    page, _ = _real()
    problems = coverage_problems(page, {"OCX_HOME"})
    assert any("registry entries read" in problem for problem in problems), problems
