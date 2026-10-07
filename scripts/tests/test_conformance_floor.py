"""The conformance hook (`test/src/conformance.py`) and its floor checker, over synthetic inputs.

The hook cases build a two-command contract by hand, so each red names one
schema clause the document breaks; one case loads the committed goldens to
prove the real files resolve. The checker cases write JUnit reports carrying
the properties the hook records.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from xml.sax.saxutils import quoteattr

import conformance_floor as floor
import pytest
from jsonschema import Draft202012Validator

sys.path.insert(0, str(floor.REPO_ROOT / "test" / "src"))
import conformance

REPORTS_ID = "https://ocx.sh/schemas/reports/v1.json"
ERRORS_ID = "https://ocx.sh/schemas/errors/v1.json"


def _command(path: list[str], output: list[dict], args: list[dict] | None = None) -> dict:
    return {"path": path, "output": output, "args": args or [], "commands": []}


CLI = {
    "schema_version": 1,
    "root": {
        "path": [],
        "output": [],
        "args": [
            {"id": "project", "long": "project", "value": {"type": "path"}, "num_args": {"min": 1, "max": 1}},
            {"id": "offline", "long": "offline", "value": {"type": "switch"}, "num_args": {"min": 0, "max": 0}},
        ],
        "commands": [
            _command(["show"], [{"type": "report", "root": "Show"}, {"type": "report_then_fail", "root": "Show"}]),
            _command(["other"], [{"type": "report", "root": "Other"}]),
            _command(["raw"], [{"type": "report", "root": "Other"}, {"type": "raw_document"}]),
            {
                "path": ["group"],
                "output": [],
                "args": [],
                "commands": [_command(["group", "leaf"], [{"type": "report", "root": "Other"}])],
            },
        ],
    },
}

REPORTS = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "$id": REPORTS_ID,
    "reports": {"Show": {"$ref": "#/$defs/Show"}, "Other": {"$ref": "#/$defs/Other"}},
    "$defs": {
        "Show": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "status": {"$ref": "#/$defs/Status"},
                "source": {"$ref": "#/$defs/Source"},
            },
            "required": ["name", "status", "source"],
        },
        "Status": {
            "type": "string",
            "x-ocx-enum": [{"value": "ok", "description": "fine"}, {"value": "failed", "description": "not"}],
        },
        "Source": {
            "oneOf": [
                {
                    "type": "object",
                    "properties": {"type": {"const": "registry"}, "host": {"type": "string"}},
                    "required": ["type", "host"],
                },
                {
                    "x-ocx-unknown-variant": True,
                    "type": "object",
                    "required": ["type"],
                    "properties": {"type": {"type": "string", "not": {"enum": ["registry"]}}},
                },
            ]
        },
        "Other": {"type": "object", "properties": {"count": {"type": "integer"}}, "required": ["count"]},
    },
}

ERRORS = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "$id": ERRORS_ID,
    "title": "ErrorEnvelope",
    "type": "object",
    "properties": {
        "schema_version": {"type": "integer"},
        "exit_code": {"type": "integer", "x-ocx-enum": [{"value": 65, "description": "data"}]},
        "error": {"type": "object"},
    },
    "required": ["schema_version", "exit_code", "error"],
}

SHOW = {"name": "cmake", "status": "ok", "source": {"type": "registry", "host": "localhost"}}


@pytest.fixture
def contract() -> conformance.Contract:
    return conformance.Contract(CLI, REPORTS, ERRORS)


def _judge(contract: conformance.Contract, args: list[str], doc: object, rc: int = 0) -> conformance.Verdict:
    stdout = doc if isinstance(doc, str) else json.dumps(doc)
    verdict = contract.judge(args, stdout, rc)
    assert verdict is not None, "the document was not judged at all"
    return verdict


def _finding(contract: conformance.Contract, args: list[str], doc: object, rc: int = 0) -> conformance.Finding:
    verdict = _judge(contract, args, doc, rc)
    assert verdict.root is None, f"validated as {verdict.root}, expected a finding"
    assert verdict.finding is not None
    return verdict.finding


# --- green: what a conforming document records ------------------------------


def test_a_conforming_report_records_its_root(contract: conformance.Contract) -> None:
    assert _judge(contract, ["show", "cmake"], SHOW) == conformance.Verdict(root="Show")


def test_flags_and_their_values_are_skipped_to_find_the_command(contract: conformance.Contract) -> None:
    node = contract.resolve(["--offline", "--project", "show", "show", "x"])
    assert node is not None and node["path"] == ["show"]
    assert contract.resolve(["--project=p", "group", "leaf"])["path"] == ["group", "leaf"]


def test_an_error_document_selects_the_errors_schema(contract: conformance.Contract) -> None:
    doc = {"schema_version": 1, "exit_code": 65, "error": {"kind": "data_error"}}
    assert _judge(contract, ["show", "x"], doc, rc=65) == conformance.Verdict(root=conformance.ERRORS_ROOT)


def test_a_raw_document_is_not_validated(contract: conformance.Contract) -> None:
    assert _judge(contract, ["raw"], {"spdxVersion": "SPDX-2.3"}) == conformance.Verdict()


def test_a_report_on_a_failing_exit_selects_report_then_fail(contract: conformance.Contract) -> None:
    assert _judge(contract, ["show"], SHOW, rc=1) == conformance.Verdict(root="Show")


# --- red: each V11 case yields a finding ------------------------------------


def test_a_dropped_wire_field_is_a_finding(contract: conformance.Contract) -> None:
    doc = {k: v for k, v in SHOW.items() if k != "source"}
    finding = _finding(contract, ["show"], doc)
    assert finding.roots == ("Show",)
    assert "'source' is a required property" in finding.detail


def test_a_root_outside_the_declared_modes_is_a_finding(contract: conformance.Contract) -> None:
    finding = _finding(contract, ["other"], SHOW)
    assert finding.roots == ("Other",)
    assert "'count' is a required property" in finding.detail


def test_a_report_on_a_failing_exit_without_report_then_fail_is_a_finding(contract: conformance.Contract) -> None:
    finding = _finding(contract, ["other"], {"count": 1}, rc=1)
    assert finding.rule == "undeclared-mode"
    assert "report_then_fail" in finding.detail


def test_an_unlisted_status_is_a_finding(contract: conformance.Contract) -> None:
    finding = _finding(contract, ["show"], {**SHOW, "status": "weird"})
    assert "'weird' is not one of ['ok', 'failed']" in finding.detail


def test_an_unregistered_exit_code_in_an_error_is_a_finding(contract: conformance.Contract) -> None:
    finding = _finding(contract, ["show"], {"schema_version": 1, "exit_code": 99, "error": {}}, rc=99)
    assert finding.roots == (conformance.ERRORS_ROOT,)
    assert "99 is not one of [65]" in finding.detail


def test_a_payload_missing_its_union_tag_is_a_finding(contract: conformance.Contract) -> None:
    finding = _finding(contract, ["show"], {**SHOW, "source": {"host": "localhost"}})
    assert "'type' is a required property" in finding.detail


def test_an_unknown_union_variant_is_a_finding(contract: conformance.Contract) -> None:
    """The published schema accepts it through the unknown arm; ocx's own output may not."""
    unknown = {"type": "mirror"}
    assert Draft202012Validator(REPORTS["$defs"]["Source"]).is_valid(unknown), "fixture: the open schema accepts it"
    finding = _finding(contract, ["show"], {**SHOW, "source": unknown})
    assert finding.rule == "const at $.source.type"


def test_a_duplicate_key_is_a_finding(contract: conformance.Contract) -> None:
    stdout = '{"schema_version": 1, "schema_version": 2, "count": 1}'
    finding = _finding(contract, ["other"], stdout)
    assert finding.rule == "duplicate-key"
    assert "schema_version" in finding.detail


def test_non_json_stdout_is_a_finding(contract: conformance.Contract) -> None:
    assert _finding(contract, ["other"], "not json").rule == "not-json"


def test_an_unresolved_command_is_a_finding(contract: conformance.Contract) -> None:
    assert _finding(contract, ["nope"], {"count": 1}).rule == "unresolved-command"


# --- recording and the blocking switch --------------------------------------


def test_observe_records_roots_and_findings_as_properties(contract: conformance.Contract) -> None:
    conformance.drain()
    conformance.observe(["show"], json.dumps(SHOW), 0, contract)
    conformance.observe(["show"], json.dumps(SHOW), 0, contract)
    with pytest.raises(AssertionError):
        conformance.observe(["other"], "{}", 0, contract)
    props = conformance.drain()
    assert props.count((conformance.ROOT_PROPERTY, "Show")) == 1, "a root is recorded once per test"
    findings = [json.loads(v) for k, v in props if k == conformance.FINDING_PROPERTY]
    assert [f["roots"] for f in findings] == [["Other"]]
    assert conformance.drain() == []


def test_a_finding_fails_the_capturing_test_by_default(contract: conformance.Contract) -> None:
    with pytest.raises(AssertionError, match="'count' is a required property"):
        conformance.observe(["other"], "{}", 0, contract)
    props = conformance.drain()
    assert [k for k, _ in props] == [conformance.FINDING_PROPERTY], "the finding is recorded before it raises"


def test_the_warn_only_switch_records_without_raising(
    contract: conformance.Contract, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(conformance, "BLOCKING", False)
    verdict = conformance.observe(["other"], "{}", 0, contract)
    assert verdict is not None and verdict.finding is not None
    conformance.drain()


def test_the_committed_goldens_load_and_resolve() -> None:
    real = conformance.Contract.load()
    assert real.report_root_count() >= 40, "the reports map was read short"
    node = real.resolve(["package", "push", "-p", "linux/amd64", "x:1"])
    assert node is not None and node["path"] == ["package", "push"]
    doc = {"schema_version": 1, "command": "x", "exit_code": 65, "error": {"kind": "data_error", "message": "m", "context": {}}}
    assert real.judge(["version"], json.dumps(doc), 65) == conformance.Verdict(root=conformance.ERRORS_ROOT)


# --- the floor checker ------------------------------------------------------


def _junit(path: Path, cases: list[list[tuple[str, str]]]) -> Path:
    body = []
    for i, props in enumerate(cases):
        rendered = "".join(f"<property name={quoteattr(k)} value={quoteattr(v)}/>" for k, v in props)
        body.append(f'<testcase classname="t" name="c{i}"><properties>{rendered}</properties></testcase>')
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f'<testsuites><testsuite name="pytest">{"".join(body)}</testsuite></testsuites>', encoding="utf-8")
    return path


def _files(tmp_path: Path, floor_value: int, suite_floor: int) -> tuple[Path, Path]:
    (tmp_path / "CONFORMANCE_FLOOR").write_text(f"{floor_value}\n", encoding="utf-8")
    (tmp_path / "SUITE_FLOOR").write_text(f"{suite_floor}\n", encoding="utf-8")
    return tmp_path / "CONFORMANCE_FLOOR", tmp_path / "SUITE_FLOOR"


ROOTED = [
    [(conformance.ROOT_PROPERTY, "Show")],
    [(conformance.ROOT_PROPERTY, "Other"), (conformance.ROOT_PROPERTY, "Show")],
    [(conformance.ROOT_PROPERTY, conformance.ERRORS_ROOT)],
]


def test_floor_red_when_distinct_roots_fall_below_it(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    junit = _junit(tmp_path / "junit.xml", ROOTED)
    assert floor.run(junit, *_files(tmp_path, 4, 3)) == 1
    err = capsys.readouterr().err
    assert "3 distinct roots validated, " in err and "CONFORMANCE_FLOOR is 4" in err


def test_floor_red_names_a_root_that_stopped_validating(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    finding = conformance.Finding("other", ("Gone",), "required at $", "'count' is required")
    junit = _junit(tmp_path / "junit.xml", [*ROOTED, [(conformance.FINDING_PROPERTY, finding.to_property())]])
    assert floor.run(junit, *_files(tmp_path, 4, 3)) == 1
    err = capsys.readouterr().err
    assert "stopped validating" in err and "failing only: ['Gone']" in err
    assert "stopped being exercised" not in err


def test_floor_green_at_the_floor(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    junit = _junit(tmp_path / "junit.xml", ROOTED)
    assert floor.run(junit, *_files(tmp_path, 3, 3)) == 0
    assert "3 distinct roots validated (floor 3)" in capsys.readouterr().out


def test_floor_reads_a_directory_of_per_module_reports(tmp_path: Path) -> None:
    _junit(tmp_path / "accept" / "a" / "junit.xml", ROOTED[:1])
    _junit(tmp_path / "accept" / "b" / "junit.xml", ROOTED[1:])
    census = floor.read(tmp_path / "accept")
    assert census.cases == 3
    assert census.roots == {"Show", "Other", conformance.ERRORS_ROOT}


def test_a_partial_run_is_not_floored(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    junit = _junit(tmp_path / "junit.xml", ROOTED)
    assert floor.run(junit, *_files(tmp_path, 99, 4)) == 0
    out = capsys.readouterr().out
    assert "partial run: 3 cases, " in out and "SUITE_FLOOR is 4 — floor not checked" in out


def test_no_report_is_red(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert floor.run(tmp_path / "missing.xml", *_files(tmp_path, 0, 0)) == 1
    assert "no JUnit testcase" in capsys.readouterr().err


def test_findings_fail_the_floor_and_are_reported_grouped_by_root(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    finding = conformance.Finding("other", ("Other",), "required at $", "'count' is required")
    junit = _junit(tmp_path / "junit.xml", [*ROOTED, [(conformance.FINDING_PROPERTY, finding.to_property())]] * 2)
    assert floor.run(junit, *_files(tmp_path, 3, 1)) == 1
    err = capsys.readouterr().err
    assert "2 findings" in err
    assert "Other: 2 × required at $" in err
