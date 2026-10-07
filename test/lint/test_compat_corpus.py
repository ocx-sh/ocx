# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Well-formedness of the compat mutation corpus.

`crates/ocx_sdkgen/tests/compat_corpus/<case>/` holds `base.json`,
`current.json` and `expect.json`; the compat gate diffs the first two and
compares the result with the third. Until the differ exists this test holds
the data to the shape that gate reads, and the set of cases to every differ
code and every change class the corpus is required to carry.
"""

from __future__ import annotations

import json
import shutil
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[2]
CORPUS = PROJECT_ROOT / "crates" / "ocx_sdkgen" / "tests" / "compat_corpus"

CASE_FILES = {"base.json", "current.json", "expect.json"}
EXPECT_KEYS = {"kind", "verdict", "change", "findings"}
FINDING_KEYS = {"code", "pointer", "subjects"}

OUTPUT_CODES = {f"B0{n}" for n in range(1, 9)} | {"D01", "U01"}
ERRORS_CODES = OUTPUT_CODES | {"R01", "R02", "R03"}
CLI_CODES = {f"G{n:02}" for n in range(1, 16)}
CODES_BY_KIND = {"reports": OUTPUT_CODES, "errors": ERRORS_CODES, "cli": CLI_CODES}
ALL_CODES = ERRORS_CODES | CLI_CODES

# A named tuple, not `except A, B:`: CI's census parses this file with the
# runner's Python 3.12, which rejects the unparenthesized 3.14 form.
_UNREADABLE_EXPECT = (OSError, ValueError, KeyError, TypeError)

# Change classes the corpus must carry beyond one case per code. Each name says
# its class; a missing one means the differ's handling of it is untested.
REQUIRED_CASES = {
    "b05_enum_value_removed",
    "b08_enum_name_changed",
    "d01_enum_entry_description",
    "r02_exit_code_category_changed",
    "r03_slug_exit_code_changed",
    "u01_enum_entry_member",
    "u01_min_items",
    "b05_union_variant_removed",
    "b05_union_unknown_arm_removed",
    "d01_unknown_arm_description",
    "g02_long_renamed_with_id_short_kept",
    "g03_short_dropped_with_id_rename",
    "g06_value_type_changed_with_id_rename",
    "union_variant_added",
    "b03_required_to_optional",
    "optional_to_required",
    "d01_description_doc",
    "d01_description_semantic",
    "enum_entries_reordered",
    "union_arms_reordered",
    "named_args_reordered",
    "env_entries_reordered",
    "g04_required_arg_added",
    "g01_removed_before_removal_release",
    "payload_def_reached_from_another_root",
}


def resolve(doc: object, pointer: str) -> bool:
    """Whether an RFC 6901 pointer names a node of `doc`."""
    if pointer == "":
        return True
    if not pointer.startswith("/"):
        return False
    node = doc
    for raw in pointer[1:].split("/"):
        token = raw.replace("~1", "/").replace("~0", "~")
        if isinstance(node, dict) and token in node:
            node = node[token]
        elif isinstance(node, list) and token.isdigit() and int(token) < len(node):
            node = node[int(token)]
        else:
            return False
    return True


def command_paths(cli: dict) -> set[str]:
    paths: set[str] = set()
    stack = [cli.get("root", {})]
    while stack:
        node = stack.pop()
        paths.add(" ".join(node.get("path", [])))
        stack.extend(node.get("commands", []))
    return paths


def subject_universe(kind: str, docs: list[dict]) -> set[str]:
    if kind == "reports":
        return {name for doc in docs for name in doc.get("reports", {})}
    if kind == "cli":
        return {"*"} | {path for doc in docs for path in command_paths(doc)}
    return {"*"}


def case_problems(case: Path) -> list[str]:
    """Everything wrong with one case directory."""
    names = {entry.name for entry in case.iterdir()}
    if names != CASE_FILES:
        return [f"{case.name}: holds {sorted(names)}, expected {sorted(CASE_FILES)}"]
    try:
        base, current, expect = (
            json.loads((case / name).read_text(encoding="utf-8"))
            for name in sorted(CASE_FILES)
        )
    except json.JSONDecodeError as error:
        return [f"{case.name}: invalid JSON: {error}"]
    problems: list[str] = []
    if not all(isinstance(doc, dict) for doc in (base, current, expect)):
        return [f"{case.name}: every file must hold a JSON object"]
    if base == current:
        problems.append(f"{case.name}: base and current are identical")
    if set(expect) != EXPECT_KEYS:
        return problems + [
            f"{case.name}: expect.json keys {sorted(expect)}, expected {sorted(EXPECT_KEYS)}"
        ]
    kind, verdict, findings = expect["kind"], expect["verdict"], expect["findings"]
    if kind not in CODES_BY_KIND:
        return problems + [f"{case.name}: unknown kind {kind!r}"]
    if verdict not in {"break", "non_break"}:
        problems.append(f"{case.name}: unknown verdict {verdict!r}")
    if not isinstance(expect["change"], str) or not expect["change"].strip():
        problems.append(f"{case.name}: `change` must describe the mutation")
    if not isinstance(findings, list):
        return problems + [f"{case.name}: `findings` must be a list"]
    universe = subject_universe(kind, [base, current])
    for index, finding in enumerate(findings):
        at = f"{case.name}: findings[{index}]"
        if not isinstance(finding, dict) or set(finding) != FINDING_KEYS:
            problems.append(f"{at}: keys must be {sorted(FINDING_KEYS)}")
            continue
        if finding["code"] not in CODES_BY_KIND[kind]:
            problems.append(
                f"{at}: code {finding['code']!r} is not a {kind} differ code"
            )
        pointer = finding["pointer"]
        if not isinstance(pointer, str) or not (
            resolve(base, pointer) or resolve(current, pointer)
        ):
            problems.append(f"{at}: pointer {pointer!r} resolves in neither document")
        subjects = finding["subjects"]
        if (
            not isinstance(subjects, list)
            or not subjects
            or not set(subjects) <= universe
        ):
            problems.append(
                f"{at}: subjects {subjects!r} must be a non-empty subset of {sorted(universe)}"
            )
    codes = {finding.get("code") for finding in findings if isinstance(finding, dict)}
    if verdict == "break" and not findings:
        problems.append(f"{case.name}: a break with no finding")
    if verdict == "non_break" and not codes <= {"D01"}:
        problems.append(
            f"{case.name}: non_break carries breaking codes {sorted(codes - {'D01'})}"
        )
    return problems


def corpus_problems(corpus: Path) -> list[str]:
    cases = sorted(entry for entry in corpus.iterdir() if entry.is_dir())
    problems = [problem for case in cases for problem in case_problems(case)]
    covered: set[str] = set()
    for case in cases:
        try:
            covered |= {
                finding["code"]
                for finding in json.loads(
                    (case / "expect.json").read_text(encoding="utf-8")
                )["findings"]
            }
        except _UNREADABLE_EXPECT:
            continue
    if missing := sorted(ALL_CODES - covered):
        problems.append(f"no case for differ codes {missing}")
    if absent := sorted(REQUIRED_CASES - {case.name for case in cases}):
        problems.append(f"missing required cases {absent}")
    return problems


def test_corpus_is_well_formed() -> None:
    assert corpus_problems(CORPUS) == []


def test_checker_reds_a_malformed_corpus(tmp_path: Path) -> None:
    corpus = tmp_path / "compat_corpus"
    shutil.copytree(CORPUS, corpus)
    assert corpus_problems(corpus) == []

    (corpus / "b02_property_removed" / "expect.json").unlink()
    expect = corpus / "g15_stdin_secret_changed" / "expect.json"
    doc = json.loads(expect.read_text(encoding="utf-8"))
    doc["findings"][0]["pointer"] = "/root/commands/9"
    expect.write_text(json.dumps(doc), encoding="utf-8")
    expect = corpus / "flag_added" / "expect.json"
    doc = json.loads(expect.read_text(encoding="utf-8"))
    doc["findings"] = [{"code": "G04", "pointer": "", "subjects": ["package push"]}]
    expect.write_text(json.dumps(doc), encoding="utf-8")
    shutil.copy(
        corpus / "root_added" / "base.json", corpus / "root_added" / "current.json"
    )

    problems = "\n".join(corpus_problems(corpus))
    for needle in (
        "b02_property_removed: holds",
        "g15_stdin_secret_changed: findings[0]: pointer",
        "flag_added: non_break carries breaking codes ['G04']",
        "root_added: base and current are identical",
        "no case for differ codes ['B02']",
    ):
        assert needle in problems, problems
