# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""The smoke tier's coverage contract (plan_crate_split_workspace.md C-013, C-014).

`task test:smoke` runs `-m smoke`. That selection is only a gate if two things
hold, and both are structural facts about the suite rather than about any one
test, so they are asserted here over the suite's own source:

(a) every visible top-level `ocx` verb has at least one `smoke`-marked test
    that invokes it — a verb whose only smoke test loses its marker reds this
    test naming the verb;
(b) the selection is disjoint from the signing surface — no marked test is a
    `sign|attest|verify|cosign` test by name or file, and none takes the
    `sigstore_stack` / `identity_token` fixtures, so the tier can never grow
    a dependency on the seven-container Sigstore stack.

Both read the test files with `ast`; nothing here imports a test module, so a
module whose import needs the registry cannot make this file's verdict depend
on it.

(c) the tier is the *whole* automatic pull-request gate, because the deep one
    is opt-in: `verify-deep.yml` has no `pull_request` trigger, is reached by
    `workflow_dispatch`, and still runs in the merge queue. That is a trigger
    shape and so is checked here too — this tier's coverage obligation follows
    directly from it.
"""
from __future__ import annotations

import ast
import re
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

# The acceptance modules this sweep reads; it lives in the lint tier beside them.
TESTS_DIR = Path(__file__).resolve().parents[1] / "tests"
REPO_ROOT = TESTS_DIR.parents[1]
COMMAND_RS = REPO_ROOT / "crates" / "ocx_cli" / "src" / "command.rs"
COMMAND_MODULES = COMMAND_RS.parent / "command"

SIGNING_SURFACE = re.compile(r"sign|attest|verify|cosign", re.IGNORECASE)
SIGNING_FIXTURES = frozenset({"sigstore_stack", "identity_token"})


# ---------------------------------------------------------------------------
# The visible verbs, read from `pub enum Command`
# ---------------------------------------------------------------------------


def _attrs_hide(attrs: list[str]) -> bool:
    return any("hide = true" in a for a in attrs)


def _subcommand_type_is_hidden(module: str, type_name: str) -> bool:
    """`#[command(hide = true)]` on the group's own enum hides the variant too.

    `Launcher` is the case: `command.rs` carries only `#[command(subcommand)]`,
    the hiding attribute sits on `pub enum Launcher` in `command/launcher.rs`.
    """
    source = (COMMAND_MODULES / f"{module}.rs").read_text()
    attrs: list[str] = []
    for line in source.splitlines():
        stripped = line.strip()
        if stripped.startswith("#["):
            attrs.append(stripped)
        elif re.match(rf"pub (enum|struct) {re.escape(type_name)}\b", stripped):
            return _attrs_hide(attrs)
        elif not stripped.startswith("///") and not stripped.startswith("//"):
            attrs = []
    raise AssertionError(f"pub enum/struct {type_name} not found in {module}.rs")


def visible_verbs() -> set[str]:
    """Every `pub enum Command` variant clap would list under `ocx --help`.

    Excluded: `hide = true` (on the variant or on the group type it wraps),
    `Deprecated*` variants and the `external_subcommand` catch-all.
    """
    source = COMMAND_RS.read_text()
    body = re.search(r"^pub enum Command \{\n(.*?)^\}", source, re.DOTALL | re.MULTILINE)
    assert body, f"pub enum Command not found in {COMMAND_RS}"
    verbs: set[str] = set()
    attrs: list[str] = []
    for line in body.group(1).splitlines():
        stripped = line.strip()
        if stripped.startswith("#["):
            attrs.append(stripped)
            continue
        variant = re.match(r"(\w+)\((\w+)::(\w+)\)", stripped)
        if not variant:
            if not stripped.startswith("///"):
                attrs = []
            continue
        name, module, type_name = variant.groups()
        hidden = (
            _attrs_hide(attrs)
            or name.startswith("Deprecated")
            or any("external_subcommand" in a for a in attrs)
            or (
                any("subcommand" in a for a in attrs)
                and _subcommand_type_is_hidden(module, type_name)
            )
        )
        if not hidden:
            explicit = next(
                (m.group(1) for a in attrs if (m := re.search(r'name = "([^"]+)"', a))),
                None,
            )
            verbs.add(explicit or re.sub(r"(?<!^)(?=[A-Z])", "-", name).lower())
        attrs = []
    # The parser's own red states: a regex drift that finds nothing, or a hide
    # detection that stops working, must not read as "all verbs covered".
    assert {"env", "package", "version", "self"} <= verbs, verbs
    assert "launcher" not in verbs and "run" not in verbs, verbs
    return verbs


# ---------------------------------------------------------------------------
# The smoke-marked tests, read with `ast`
# ---------------------------------------------------------------------------


@dataclass(slots=True)
class SmokeTest:
    path: Path
    name: str
    fixtures: frozenset[str]
    # The string arguments of every invocation the test reaches, one tuple per
    # call and in argument order: `("--project", "lock")`, `("index", "update")`.
    argvs: tuple[tuple[str, ...], ...] = ()

    @property
    def node_id(self) -> str:
        return f"{self.path.name}::{self.name}"


# `OcxRunner`'s entry points; a call to any of them is an `ocx` invocation.
RUNNER_METHODS = frozenset({"run", "json", "plain"})


def _is_smoke_mark(node: ast.expr) -> bool:
    if isinstance(node, ast.Call):
        node = node.func
    return isinstance(node, ast.Attribute) and node.attr == "smoke" and ast.unparse(node) in (
        "pytest.mark.smoke",
        "mark.smoke",
    )


def _pytestmark_is_smoke(body: list[ast.stmt]) -> bool:
    """A `pytestmark = ...` in a module or class body that carries `smoke`."""
    for node in body:
        if isinstance(node, ast.Assign) and any(
            isinstance(t, ast.Name) and t.id == "pytestmark" for t in node.targets
        ):
            marks = node.value.elts if isinstance(node.value, (ast.List, ast.Tuple)) else [node.value]
            if any(_is_smoke_mark(m) for m in marks):
                return True
    return False


def _used_fixtures(decorators: list[ast.expr]) -> frozenset[str]:
    return frozenset(
        c.value
        for d in decorators
        if isinstance(d, ast.Call) and ast.unparse(d.func).endswith("usefixtures")
        for c in d.args
        if isinstance(c, ast.Constant) and isinstance(c.value, str)
    )


def _collected_tests(
    body: list[ast.stmt], smoke: bool, fixtures: frozenset[str], prefix: str = ""
) -> Iterator[tuple[str, ast.FunctionDef | ast.AsyncFunctionDef, bool, frozenset[str]]]:
    """(name, node, smoke-marked?, inherited fixtures) for every test pytest collects.

    A marker on a `Test*` class, a `pytestmark` in a class or module body and
    a class-level `usefixtures` all apply to every method beneath them exactly
    as if each carried the decorator — so they are carried down here rather
    than read from the function alone.
    """
    smoke = smoke or _pytestmark_is_smoke(body)
    for node in body:
        if isinstance(node, ast.ClassDef) and node.name.startswith("Test"):
            yield from _collected_tests(
                node.body,
                smoke or any(_is_smoke_mark(d) for d in node.decorator_list),
                fixtures | _used_fixtures(node.decorator_list),
                f"{prefix}{node.name}::",
            )
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith("test"):
            marked = smoke or any(_is_smoke_mark(d) for d in node.decorator_list)
            yield f"{prefix}{node.name}", node, marked, fixtures


def _string_args(elts: list[ast.expr]) -> tuple[str, ...]:
    return tuple(e.value for e in elts if isinstance(e, ast.Constant) and isinstance(e.value, str))


def _starts_with_binary(node: ast.expr) -> bool:
    return isinstance(node, (ast.List, ast.Tuple)) and bool(node.elts) and ".binary" in ast.unparse(node.elts[0])


def _invocations(fn: ast.AST, helpers: dict[str, ast.AST]) -> list[tuple[str, ...]]:
    """The literal arguments of every call in `fn` that reaches the binary.

    Three shapes carry a verb in this suite: a runner call (`ocx.run(...)`,
    `.json(...)`, `.plain(...)`), an argv list built on the binary
    (`[str(ocx.binary), "lock", ...]`, plus a `cmd += [...]` that extends
    one), and a call to a same-module helper (`_run(ocx, project, "init")`).
    A string anywhere else — a dict key, a package name handed to
    `make_package`, an expected substring — is never an invocation.
    """
    argv_names = {
        t.id
        for n in ast.walk(fn)
        if isinstance(n, ast.Assign) and _starts_with_binary(n.value)
        for t in n.targets
        if isinstance(t, ast.Name)
    }
    out: list[tuple[str, ...]] = []
    for node in ast.walk(fn):
        if isinstance(node, ast.Call):
            f = node.func
            if (isinstance(f, ast.Attribute) and f.attr in RUNNER_METHODS) or (
                isinstance(f, ast.Name) and f.id in helpers
            ):
                out.append(_string_args(node.args))
        elif _starts_with_binary(node):
            out.append(_string_args(node.elts))
        elif (
            isinstance(node, ast.AugAssign)
            and isinstance(node.target, ast.Name)
            and node.target.id in argv_names
            and isinstance(node.value, (ast.List, ast.Tuple))
        ):
            out.append(_string_args(node.value.elts))
    return out


def _called_names(fn: ast.AST) -> set[str]:
    return {
        n.func.id for n in ast.walk(fn) if isinstance(n, ast.Call) and isinstance(n.func, ast.Name)
    }


def _reachable_invocations(fn: ast.AST, helpers: dict[str, ast.AST]) -> tuple[tuple[str, ...], ...]:
    """Invocations of `fn` plus those of every same-module helper it reaches."""
    seen: set[str] = set()
    todo = [fn]
    argvs: list[tuple[str, ...]] = []
    while todo:
        node = todo.pop()
        argvs += _invocations(node, helpers)
        for name in _called_names(node) - seen:
            if name in helpers:
                seen.add(name)
                todo.append(helpers[name])
    return tuple(argvs)


def smoke_tests() -> list[SmokeTest]:
    found: list[SmokeTest] = []
    for path in sorted(TESTS_DIR.glob("test_*.py")):
        tree = ast.parse(path.read_text(), filename=str(path))
        helpers = {
            n.name: n for n in tree.body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
        }
        for name, node, marked, inherited in _collected_tests(tree.body, False, frozenset()):
            if not marked:
                continue
            args = node.args
            fixtures = (
                frozenset(a.arg for a in [*args.posonlyargs, *args.args, *args.kwonlyargs])
                | _used_fixtures(node.decorator_list)
                | inherited
            )
            found.append(
                SmokeTest(
                    path=path,
                    name=name,
                    fixtures=fixtures,
                    argvs=_reachable_invocations(node, helpers),
                )
            )
    return found


# ---------------------------------------------------------------------------
# (a) every visible verb is invoked by a smoke test
# ---------------------------------------------------------------------------


def _invokes(argv: tuple[str, ...], verb: str, verbs: set[str]) -> bool:
    """`argv` runs `verb` as its top-level verb: the first verb-named argument.

    `("direnv", "init")` covers `direnv` and not `init`; `("index", "update")`
    covers `index` and not `update`; `("--project", "lock")` covers `lock`.
    """
    return next((a for a in argv if a in verbs), None) == verb


def test_every_visible_verb_has_a_smoke_test() -> None:
    verbs = visible_verbs()
    marked = smoke_tests()
    assert marked, "no `@pytest.mark.smoke` test found under test/tests — the tier is empty"
    uncovered = {
        verb for verb in verbs if not any(_invokes(argv, verb, verbs) for t in marked for argv in t.argvs)
    }
    assert not uncovered, (
        f"visible verbs without a smoke test that invokes them: {sorted(uncovered)}. "
        f"Every top-level verb needs one `@pytest.mark.smoke` happy path whose body "
        f"(or a helper it calls) runs it as the top-level verb — the first verb-named "
        f"argument of an `ocx.run(...)`, a `[str(ocx.binary), ...]` argv or a helper call. "
        f"Smoke-marked: {[t.node_id for t in marked]}"
    )


# ---------------------------------------------------------------------------
# (b) the selection never reaches the signing surface
# ---------------------------------------------------------------------------


def test_smoke_selection_is_disjoint_from_signing() -> None:
    marked = smoke_tests()
    assert marked, "no `@pytest.mark.smoke` test found under test/tests — the tier is empty"
    by_name = [t.node_id for t in marked if SIGNING_SURFACE.search(t.node_id)]
    by_fixture = [t.node_id for t in marked if t.fixtures & SIGNING_FIXTURES]
    assert not by_name, (
        f"smoke-marked tests on the signing surface (name or file matches "
        f"{SIGNING_SURFACE.pattern!r}): {by_name}"
    )
    assert not by_fixture, (
        f"smoke-marked tests taking {sorted(SIGNING_FIXTURES)}: {by_fixture} — "
        f"these start the Sigstore stack"
    )


# ---------------------------------------------------------------------------
# (c) the deep tier does NOT run per pull request — this tier is the gate
# ---------------------------------------------------------------------------


def test_deep_workflow_is_opt_in_and_runs_in_the_merge_queue() -> None:
    """`verify-deep.yml` has no `pull_request` trigger, so this tier is the gate.

    A deep run is ≈113 runner-minutes against the basic tier's ≈18, so it is
    opt-in per branch (`gh workflow run verify-deep.yml --ref <branch>`) —
    which makes `workflow_dispatch` load-bearing rather than incidental. What
    a pull request now gets automatically is the smoke tier asserted above and
    nothing else, which is why (a)'s per-verb obligation is not negotiable.
    `merge_group` is what runs the checks on the queued merge commit.
    `.claude/tests/test_workflows.py` asserts the same shape plus the absence
    of any per-job pull-request guard and the queue-safe cancellation.
    """
    import yaml  # a suite dependency (test/pyproject.toml); nothing above needs it

    workflow = yaml.safe_load((REPO_ROOT / ".github" / "workflows" / "verify-deep.yml").read_text())
    # PyYAML reads the bare `on` key as the YAML 1.1 boolean `True`.
    triggers = workflow.get("on", workflow.get(True))
    assert isinstance(triggers, dict), f"verify-deep.yml has no `on:` mapping: {triggers!r}"
    assert "pull_request" not in triggers, (
        f"verify-deep.yml `on` is {sorted(triggers)}, including `pull_request` — the deep tier "
        f"is opt-in, and this file's smoke-tier obligations are written on the premise that a "
        f"pull request gets the basic tier and nothing more. Change both together"
    )
    assert "workflow_dispatch" in triggers, (
        f"verify-deep.yml `on` is {sorted(triggers)} — no `workflow_dispatch`, so with no "
        f"`pull_request` trigger either there is no way to run the deep tier on a branch at all"
    )
    assert "merge_group" in triggers, (
        f"verify-deep.yml `on` is {sorted(triggers)} — no `merge_group`, so nothing runs the "
        f"deep tier on the queued merge commit"
    )
