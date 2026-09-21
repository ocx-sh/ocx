# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The one registry-wide singleton this suite writes, and who may write it.

`ocx patch publish --global` targets `<patch-registry>/global:__ocx.patch` — a
single reserved repository per patch registry (`ocx_config::patch`). No
`unique_repo` prefix reaches that name, so the SP7 per-worker isolation every
other registry write relies on does not apply: two xdist workers publishing
there overwrite each other, and each then installs the other's companion.

`tests/test_patches.py` pins its whole module to `xdist_group("patch_global_slot")`
for that reason, and `tests/test_frozen.py` and one `tests/test_doc_scripts.py`
row join it by hand. Membership being hand-added is the defect this module
closes: it is a source sweep, not a behaviour test, and it takes no fixture —
nothing here starts a registry.
"""
from __future__ import annotations

import ast
import re
from pathlib import Path

SLOT_GROUP = "patch_global_slot"

SUITE = Path(__file__).parents[1]

#: A `--registry` whose value carries a `/` names a path-scoped patch tier, so
#: the `global` repository under it belongs to that caller alone. This is the
#: isolation `tests/test_patch_smoke.py` documents and relies on, and it is why
#: that module is parallel-safe without joining the group.
_REGISTRY_ARG = re.compile(r"'--registry',\s*(?P<value>[^,)\]]+)")

#: A reader floor, not a subject floor: the sweep must keep finding the
#: publishers we know are there. A needle that stopped matching — a helper
#: interposed, the argv spelled differently — reports the same clean tree as a
#: suite with no unsynchronised writer in it.
_KNOWN_PUBLISHERS = {"test_patches.py": 3, "test_managed_config.py": 1}


def global_publish_sites(path: Path) -> list[str]:
    """Every `patch publish … --global` argv spelled in `path`, unparsed.

    The argv reaches ocx either as a call (`ocx.run("patch", "publish", …)`) or
    as a list a helper hands back (`tests/test_exit_codes.py`), so both node
    kinds are read. Needles are single-quoted because that is what
    `ast.unparse` emits, whatever the source spells.
    """
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    return [
        text
        for node in ast.walk(tree)
        if isinstance(node, (ast.Call, ast.List))
        for text in [ast.unparse(node)]
        if "'patch', 'publish'" in text and "'--global'" in text
    ]


def suite_sources() -> list[Path]:
    """Every Python file under `test/`, dot-directories excluded (`.venv`)."""
    return [
        path
        for path in sorted(SUITE.rglob("*.py"))
        if not any(part.startswith(".") for part in path.relative_to(SUITE).parts)
    ]


def test_the_sweep_reads_the_publishers_it_is_meant_to_judge() -> None:
    """The reader floor. Both numbers below are observed, not assumed."""
    sources = suite_sources()
    assert len(sources) >= 100, (
        f"the sweep walked only {len(sources)} files under {SUITE} — that is a broken "
        f"walk reporting a clean tree, not a small suite"
    )
    found = {
        name: len(global_publish_sites(SUITE / "tests" / name))
        for name in _KNOWN_PUBLISHERS
    }
    assert all(found[name] >= floor for name, floor in _KNOWN_PUBLISHERS.items()), (
        f"the `patch publish --global` needle no longer matches what it used to: "
        f"expected at least {_KNOWN_PUBLISHERS}, found {found}. Every verdict in "
        f"test_no_unsynchronised_global_patch_publisher is vacuous until this is fixed."
    )


def test_no_unsynchronised_global_patch_publisher() -> None:
    """A test publishing a GLOBAL descriptor to the shared registry is grouped.

    Not hypothetical. `tests/test_managed_config.py::
    test_setup_refresh_syncs_patch_descriptors` published its own `match: "*"`
    rule into the slot while `tests/test_patches.py::
    test_patch_companion_integrations_appear_once_across_several_bases` was
    between its publish and its install, two xdist workers apart. The patches
    test composed the managed-config companion — which carries no
    `integrations` — and reported `integrations: []` in two consecutive full
    runs, green whenever its module ran alone.

    A call naming a path-scoped `--registry` is exempt: its `global` repository
    is its own.

    Ceiling: membership is read per FILE, so a module already carrying the mark
    on one test satisfies it for every publisher in that file. Resolving it per
    test function would mean following helpers and parametrization, which is
    more machinery than a two-writer population earns.
    """
    offenders = [
        f"{path.relative_to(SUITE)}: {site}"
        for path in suite_sources()
        for site in global_publish_sites(path)
        if not ((match := _REGISTRY_ARG.search(site)) and "/" in match.group("value"))
        and f'xdist_group("{SLOT_GROUP}")' not in path.read_text(encoding="utf-8")
    ]
    assert not offenders, (
        f"these publish a global patch descriptor to the shared registry without "
        f'joining xdist_group("{SLOT_GROUP}"), so they race every other writer of '
        f"that one reserved repository: {offenders}. Add the mark, or point "
        f"`--registry` at a path-scoped tier of your own (tests/test_patch_smoke.py)."
    )
