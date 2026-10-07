# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Runtime conformance of `ocx --format json` stdout to the committed schemas.

`OcxRunner.run` hands every JSON-mode stdout to `observe`, which selects the
schema root from the command's `output` modes in `cli.json` (an `error` key
selects the errors document) and validates the document against a closed copy
of that root: each `x-ocx-enum` becomes an `enum` and each unknown-variant arm
is dropped, so ocx's own output may not use the openness the published schema
grants consumers. Validated roots and findings land on the current test's
JUnit record as properties; `scripts/conformance_floor.py` reads them.
"""

from __future__ import annotations

import copy
import dataclasses
import functools
import json
import re
from collections.abc import Iterator, Sequence
from pathlib import Path
from typing import Any
from urllib.parse import quote

import pytest
from jsonschema import Draft202012Validator
from jsonschema.exceptions import best_match
from referencing import Registry, Resource

# A finding fails the test that captured it; tests flip this only to show the warn-only path.
BLOCKING = True
GOLDEN = Path(__file__).resolve().parents[2] / "crates" / "ocx_schema" / "tests" / "golden"
ERRORS_ROOT = "ErrorEnvelope"
ROOT_PROPERTY = "conformance_root"
FINDING_PROPERTY = "conformance_finding"
# Stdout here belongs to a child process, a shell or a foreign document format.
_UNJUDGED_MODES = frozenset({"passthrough", "shell_stream", "raw_document"})


@dataclasses.dataclass(frozen=True, slots=True)
class Finding:
    command: str
    roots: tuple[str, ...]
    rule: str
    detail: str

    def to_property(self) -> str:
        return json.dumps(dataclasses.asdict(self))


@dataclasses.dataclass(frozen=True, slots=True)
class Verdict:
    root: str | None = None
    finding: Finding | None = None


class _DuplicateKeyError(ValueError):
    pass


def _unique_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for key, value in pairs:
        if key in out:
            raise _DuplicateKeyError(key)
        out[key] = value
    return out


def _closed(node: Any) -> Any:
    """A copy of `node` whose `x-ocx-enum`s and tagged unions admit only registered values."""
    if isinstance(node, list):
        return [_closed(item) for item in node]
    if not isinstance(node, dict):
        return node
    out = {key: _closed(value) for key, value in node.items()}
    if "x-ocx-enum" in out:
        out["enum"] = [entry["value"] for entry in out["x-ocx-enum"]]
    if "oneOf" in out:
        out["oneOf"] = [arm for arm in out["oneOf"] if not arm.get("x-ocx-unknown-variant")]
    return out


def _flags(node: dict[str, Any], inherited: list[dict[str, Any]]) -> Iterator[dict[str, Any]]:
    yield from node["args"]
    yield from inherited


def _takes_value(spec: dict[str, Any], following: str | None) -> bool:
    arity = spec.get("num_args", {})
    if arity.get("max", 1) == 0:
        return False
    if arity.get("min", 1) == 0:
        # An optional value is consumed only when the next token is one of its choices.
        choices = spec.get("value", {}).get("choices", [])
        return following in {choice["value"] for choice in choices}
    return True


class Contract:
    def __init__(self, cli: dict[str, Any], reports: dict[str, Any], errors: dict[str, Any]) -> None:
        self._cli = cli
        self._reports_id = reports["$id"]
        self._errors_id = errors["$id"]
        self._report_roots = frozenset(reports["reports"])
        self._registry = Registry().with_resources(
            [
                (self._reports_id, Resource.from_contents(_closed(copy.deepcopy(reports)))),
                (self._errors_id, Resource.from_contents(_closed(copy.deepcopy(errors)))),
            ]
        )
        self._validators: dict[str, Draft202012Validator] = {}

    @classmethod
    def load(cls, golden: Path = GOLDEN) -> Contract:
        def read(name: str) -> dict[str, Any]:
            return json.loads((golden / name).read_text(encoding="utf-8"))

        return cls(read("cli.json"), read("reports.json"), read("errors.json"))

    def report_root_count(self) -> int:
        return len(self._report_roots)

    def _validator(self, root: str) -> Draft202012Validator:
        validator = self._validators.get(root)
        if validator is None:
            ref = self._errors_id if root == ERRORS_ROOT else f"{self._reports_id}#/reports/{quote(root, safe='')}"
            validator = Draft202012Validator(
                {"$ref": ref}, registry=self._registry, format_checker=Draft202012Validator.FORMAT_CHECKER
            )
            self._validators[root] = validator
        return validator

    def resolve(self, args: Sequence[str]) -> dict[str, Any] | None:
        """The `CommandSpec` argv selects, or `None` when `--help`/`-h` hands output to clap."""
        node = self._cli["root"]
        inherited: list[dict[str, Any]] = []
        i = 0
        while i < len(args):
            token = args[i]
            i += 1
            if token == "--":
                break
            if token in ("--help", "-h"):
                return None
            if token.startswith("-") and token != "-":
                if "=" in token:
                    continue
                name = token.lstrip("-")
                long = token.startswith("--")
                spec = next((a for a in _flags(node, inherited) if a.get("long" if long else "short") == name), None)
                if spec is not None and _takes_value(spec, args[i] if i < len(args) else None):
                    i += 1
                continue
            child = next((c for c in node["commands"] if c["path"][-1] == token), None)
            if child is None:
                break
            inherited = [a for a in _flags(node, inherited) if a.get("global")]
            node = child
        return node

    def judge(self, args: Sequence[str], stdout: str, returncode: int) -> Verdict | None:
        """Validate one stdout; `None` when there is no document to judge."""
        node = self.resolve(args)
        if node is None:
            return None
        command = " ".join(node["path"]) or " ".join(args)
        modes = node["output"]
        kinds = {mode["type"] for mode in modes}
        if not stdout.strip():
            if returncode == 0 and modes and not kinds & (_UNJUDGED_MODES | {"empty"}):
                return self._finding(command, (), "empty-stdout", "stdout is empty and no `empty` mode is declared")
            return None
        try:
            doc = json.loads(stdout, object_pairs_hook=_unique_pairs)
        except _DuplicateKeyError as dup:
            return self._finding(command, (), "duplicate-key", f"key {dup} appears twice in one object")
        except json.JSONDecodeError as err:
            if kinds & _UNJUDGED_MODES:
                return Verdict()
            return self._finding(command, (), "not-json", f"stdout is not a JSON document: {err}")
        if isinstance(doc, dict) and "error" in doc:
            return self._validate(command, (ERRORS_ROOT,), doc)
        if not modes:
            return self._finding(command, (), "unresolved-command", f"argv {list(args)} names no leaf in cli.json")
        wanted = "report" if returncode == 0 else "report_then_fail"
        roots = tuple(mode["root"] for mode in modes if mode["type"] == wanted)
        if roots:
            verdict = self._validate(command, roots, doc)
            if verdict.root is not None or not kinds & _UNJUDGED_MODES:
                return verdict
        if kinds & _UNJUDGED_MODES:
            return Verdict()
        return self._finding(command, (), "undeclared-mode", f"a document on exit {returncode} needs a `{wanted}` mode")

    def _validate(self, command: str, roots: tuple[str, ...], doc: Any) -> Verdict:
        for root in roots:
            if self._validator(root).is_valid(doc):
                return Verdict(root=root)
        error = best_match(self._validator(roots[0]).iter_errors(doc))
        # Indices and map keys vary per document; the grouping key must not.
        where = re.sub(r"\[(\d+|'[^']*')\]", "[*]", error.json_path)
        rule = f"{error.validator} at {where}"
        return self._finding(command, roots, rule, f"{error.json_path}: {error.message}"[:500])

    @staticmethod
    def _finding(command: str, roots: tuple[str, ...], rule: str, detail: str) -> Verdict:
        return Verdict(finding=Finding(command, roots, rule, detail))


_recorded: dict[tuple[str, str], None] = {}


@functools.cache
def _contract() -> Contract:
    return Contract.load()


def observe(args: Sequence[str], stdout: str, returncode: int, contract: Contract | None = None) -> Verdict | None:
    """Judge one JSON-mode stdout and record the outcome on the running test."""
    verdict = (contract or _contract()).judge(args, stdout, returncode)
    if verdict is None:
        return None
    if verdict.root is not None:
        _recorded[(ROOT_PROPERTY, verdict.root)] = None
    if verdict.finding is not None:
        _recorded[(FINDING_PROPERTY, verdict.finding.to_property())] = None
        if BLOCKING:
            raise AssertionError(f"conformance: {verdict.finding.command}: {verdict.finding.detail}")
    return verdict


def drain() -> list[tuple[str, str]]:
    """The properties recorded since the last drain, each once."""
    out = list(_recorded)
    _recorded.clear()
    return out


@pytest.hookimpl(wrapper=True, tryfirst=True)
def pytest_runtest_makereport(item: pytest.Item, call: pytest.CallInfo[None]):
    # Before the report is built: the JUnit writer copies `user_properties` from it.
    item.user_properties.extend(drain())
    return (yield)
