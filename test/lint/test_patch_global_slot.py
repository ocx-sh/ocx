# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The one registry-wide singleton this suite writes, and who may write it.

`ocx patch publish --global` targets `<patch-registry>/global:__ocx.patch` — a
single reserved repository per patch registry (`ocx_config::patch`). No
`unique_repo` prefix reaches that name, so the SP7 per-worker isolation every
other registry write relies on does not apply: two xdist workers publishing
there overwrite each other, and each then installs the other's companion.

The suite keeps that slot to one writer at a time in two ways. Every patch
tier it spells — a `[patches]` config block, an `OCX_PATCHES` wire, a
`--registry` argument — names a registry PATH of its own, so the `global`
repository under it belongs to that test alone. The tests that need the bare
registry itself carry `xdist_group("patch_global_slot")` on the function (or
the module) that spells it. `test_every_patch_tier_names_a_path_of_its_own`
reads both off the source with `ast`, per function; the older
`test_no_unsynchronised_global_patch_publisher` is its coarse per-FILE
predecessor, kept verbatim from its move out of `tests/`. A source sweep, not
a behaviour test, and it takes no fixture — nothing here starts a registry.
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


#: A `[patches]` block's registry, as `Module.spelled` renders it.
_PATCHES_TIER = re.compile(r'\[patches\]\s*registry\s*=\s*"(?P<value>[^"]*)"')

#: The reader floor of the per-function sweep, observed like the one above.
_KNOWN_SITES = {
    "tests/test_patches.py": (14, 6),
    "tests/test_managed_config.py": (1, 2),
    "tests/test_frozen.py": (0, 2),
    "recordings/setups.py": (0, 2),
}

_SCOPES = (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)


def _is_slot_mark(node: ast.AST) -> bool:
    """`xdist_group("patch_global_slot")`, reached as an attribute or a bare name."""
    if not (isinstance(node, ast.Call) and node.args and isinstance(node.args[0], ast.Constant)):
        return False
    func = node.func
    name = func.attr if isinstance(func, ast.Attribute) else func.id if isinstance(func, ast.Name) else None
    return name == "xdist_group" and node.args[0].value == SLOT_GROUP


def _marks_slot(nodes: list[ast.expr]) -> bool:
    return any(_is_slot_mark(inner) for node in nodes for inner in ast.walk(node))


def _is_fixture(node: ast.AST) -> bool:
    return isinstance(node, _SCOPES[:2]) and any("fixture" in ast.unparse(d) for d in node.decorator_list)


def _params(node: ast.FunctionDef | ast.AsyncFunctionDef) -> set[str]:
    return {arg.arg for arg in node.args.args}


class Module:
    """One parsed source file, and which of its nodes run inside the slot group."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        self.parent = {child: node for node in ast.walk(self.tree) for child in ast.iter_child_nodes(node)}
        self.grouped_module = any(
            isinstance(stmt, ast.Assign)
            and any(isinstance(t, ast.Name) and t.id == "pytestmark" for t in stmt.targets)
            and _marks_slot([stmt.value])
            for stmt in self.tree.body
        )
        functions = [n for n in ast.walk(self.tree) if isinstance(n, _SCOPES[:2])]
        self.tests = [f for f in functions if f.name.startswith("test_")]
        self.fixtures = [f for f in functions if _is_fixture(f)]

    def scopes(self, node: ast.AST) -> list[ast.AST]:
        """`node` itself if it is a function or class, then those around it, innermost first."""
        chain = [node]
        while chain[-1] in self.parent:
            chain = [*chain, self.parent[chain[-1]]]
        return [n for n in chain if isinstance(n, _SCOPES)]

    def grouped(self, node: ast.AST) -> bool:
        """Whether `node` only ever runs inside the slot group.

        By decorator on `node`'s own function, an enclosing one or its class,
        or by `pytestmark`. A fixture is judged by the tests that request it,
        directly or through another fixture of this module: all of them
        grouped, or none at all (a fixture nothing requests never runs). Only a
        `test_*.py` module can know that — nothing outside it requests its
        fixtures — so a fixture anywhere else is ungrouped.
        """
        scopes = self.scopes(node)
        if self.grouped_module or any(_marks_slot(s.decorator_list) for s in scopes):
            return True
        fixture = next((s for s in scopes if _is_fixture(s)), None)
        if fixture is None or not self.path.name.startswith("test_"):
            return False
        autouse = any(k.arg == "autouse" for d in fixture.decorator_list if isinstance(d, ast.Call) for k in d.keywords)
        names = {fixture.name}
        for _ in self.fixtures:  # the request closure, one fixture level per pass
            names = names | {f.name for f in self.fixtures if names & _params(f)}
        requesters = self.tests if autouse else [t for t in self.tests if names & _params(t)]
        return all(self.grouped(t) for t in requesters)

    def spelled(self, node: ast.AST) -> str:
        """The text `node` spells, each interpolation kept as `{expr}`.

        A bare name is resolved through its one assignment in the enclosing
        function (`patch_registry = f"{ocx.registry}/…"`); a parameter, or a
        name assigned twice, stays `{name}`.
        """
        match node:
            case ast.Constant(value=str(text)):
                return text
            case ast.JoinedStr(values=values):
                return "".join(self.spelled(v) for v in values)
            case ast.FormattedValue(value=ast.Name() as name):
                return self.spelled(name)
            case ast.FormattedValue(value=value):
                return "{" + ast.unparse(value) + "}"
            case ast.Name(id=name):
                scope = next(iter(self.scopes(node)), self.tree)
                assigned = [
                    a.value
                    for a in ast.walk(scope)
                    if isinstance(a, ast.Assign) and [ast.unparse(t) for t in a.targets] == [name]
                ]
                return self.spelled(assigned[0]) if len(assigned) == 1 else "{" + name + "}"
        return "{" + ast.unparse(node) + "}"

    def publish_sites(self) -> list[ast.AST]:
        """Every `patch publish … --global` argv, as a call or as a list a helper returns."""
        return [
            node
            for node in ast.walk(self.tree)
            if isinstance(node, (ast.Call, ast.List))
            and "'patch', 'publish'" in ast.unparse(node)
            and "'--global'" in ast.unparse(node)
        ]

    def patch_tiers(self) -> list[tuple[ast.AST, str]]:
        """Every patch tier spelled here, with the registry it names.

        A `[patches]` block in a string, an `OCX_PATCHES` wire dict (the one
        dict carrying a `path_template`), and the value after `--registry` in a
        `patch publish --global` argv.
        """
        tiers = [
            (node, found["value"])
            for node in ast.walk(self.tree)
            if isinstance(node, (ast.Constant, ast.JoinedStr))
            and not isinstance(self.parent.get(node), ast.JoinedStr)
            for found in _PATCHES_TIER.finditer(self.spelled(node))
        ]
        tiers += [
            (node, self.spelled(value))
            for node in ast.walk(self.tree)
            if isinstance(node, ast.Dict)
            for key, value in zip(node.keys, node.values, strict=True)
            if isinstance(key, ast.Constant)
            and key.value == "registry"
            and any(isinstance(k, ast.Constant) and k.value == "path_template" for k in node.keys)
        ]
        for site in self.publish_sites():
            argv = site.args if isinstance(site, ast.Call) else site.elts
            tiers += [
                (site, self.spelled(argv[i + 1]))
                for i, flag in enumerate(argv[:-1])
                if isinstance(flag, ast.Constant) and flag.value == "--registry"
            ]
        return tiers


def names_bare_registry(registry: str) -> bool:
    """A registry spelled from an interpolation with no path after its host.

    `{registry}` and `{ocx.registry}` are the shared test registry itself;
    `{registry}/p…` is a path of the caller's own. A value with no
    interpolation at all (`localhost:1`) is not the shared registry.
    """
    return "{" in registry and "/" not in re.sub(r"\{[^{}]*\}", "", registry)


def test_the_per_function_sweep_reads_the_sites_it_is_meant_to_judge() -> None:
    """The reader floor of the per-function sweep: publishers and patch tiers."""
    found = {}
    for name in _KNOWN_SITES:
        module = Module(SUITE / name)
        found[name] = (len(module.publish_sites()), len(module.patch_tiers()))
    assert all(
        found[name][0] >= publishers and found[name][1] >= tiers
        for name, (publishers, tiers) in _KNOWN_SITES.items()
    ), (
        f"the per-function sweep no longer finds the (publishers, patch tiers) it used to: "
        f"expected at least {_KNOWN_SITES}, found {found}. Every verdict in "
        f"test_every_patch_tier_names_a_path_of_its_own is vacuous until this is fixed."
    )


def test_every_patch_tier_names_a_path_of_its_own() -> None:
    """Every patch tier names a registry path of its own, or runs in the group.

    A tier is a WRITER when a `patch publish --global` goes through it and a
    READER when an install probes it, and a reader of the bare slot composes
    whatever `match: "*"` rule a grouped writer left there. So every tier is
    judged, not only the publishers': a `[patches]` block, an `OCX_PATCHES`
    wire and a `--registry` argument must name `<registry>/<path>`, unless the
    function spelling it carries the group. A publish with no `--registry`
    goes through a `[patches]` tier, which this judges where it is spelled.

    Ceiling: a registry passed through a helper parameter is judged where the
    helper spells it, not where it is called — `_write_config(ocx, registry)`
    reads as the path `_write_config` appends.
    """
    offenders = []
    for path in suite_sources():
        module = Module(path)
        offenders += [
            f"{path.relative_to(SUITE)}:{node.lineno}: {registry}"
            for node, registry in module.patch_tiers()
            if names_bare_registry(registry) and not module.grouped(node)
        ]
    assert not offenders, (
        f"these patch tiers name the shared registry with no path of their own, outside "
        f'xdist_group("{SLOT_GROUP}"), so they race every other writer and reader of its '
        f"one reserved `global` repository: {offenders}. Point the tier at "
        f"`<registry>/<a path of this test's own>` (tests/test_patch_smoke.py), or add "
        f"the mark to the function."
    )
