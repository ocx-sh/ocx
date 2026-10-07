# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Interim bump rule: a contract golden that changes bytes bumps that item's version.

Compares the tree's goldens with the newest release tag's, item by item: a report
root (wrapper plus reached `$defs`, `schema_version` excluded; version = the
wrapper's `schema_version` const), `errors` (whole document minus `$id` and
`schema_version`; version = the `$id` major) and each `cli.json` command node (own
fields and args; version = its `version`). Changed -> the tag's version + 1,
unchanged -> the tag's, added -> 1, removed -> unchecked. Inert once the compat
gate is live (`baseline/` present and `ocx_sdkgen`'s differ in the tree).
"""

from __future__ import annotations

import copy
import json
import re
from pathlib import Path

from test_contract_baseline import (
    BASELINE_DIR,
    GOLDEN_DIR,
    Repo,
    blob_at,
    bootstrap_tag,
    git,
)

SDKGEN_SRC = "crates/ocx_sdkgen/src"
DIFFER = f"{SDKGEN_SRC}/compat.rs"
DOCUMENTS = ("reports.json", "errors.json", "cli.json")
_ERRORS_ID = re.compile(r"/v([0-9]+)\.json")
_DEF_REF = "#/$defs/"

# label -> (version as read, canonical bytes of what that version covers)
Items = dict[str, tuple[object, str]]


def _canonical(value: object) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _refs(value: object) -> list[str]:
    """Every `$def` name `value` points at, one level deep."""
    if isinstance(value, dict):
        found = []
        ref = value.get("$ref")
        if isinstance(ref, str) and ref.startswith(_DEF_REF):
            found.append(
                ref[len(_DEF_REF) :].split("/")[0].replace("~1", "/").replace("~0", "~")
            )
        for child in value.values():
            found += _refs(child)
        return found
    if isinstance(value, list):
        return [name for child in value for name in _refs(child)]
    return []


def report_items(doc: dict) -> Items:
    defs = doc.get("$defs", {})
    items: Items = {}
    for name, entry in doc.get("reports", {}).items():
        refs = _refs({"$ref": entry.get("$ref")}) if "$ref" in entry else []
        root = copy.deepcopy(defs.get(refs[0], {}) if refs else entry)
        version = root.get("properties", {}).pop("schema_version", {}).get("const")
        # The closure, not literal inlining: recursive types terminate, and any reached byte still counts.
        reached, queue = set(), _refs(root)
        while queue:
            ref = queue.pop()
            if ref not in reached and ref in defs:
                reached.add(ref)
                queue += _refs(defs[ref])
        items[f"report {name}"] = (
            version,
            _canonical([root, {ref: defs[ref] for ref in sorted(reached)}]),
        )
    return items


def errors_items(doc: dict) -> Items:
    if not doc:
        return {}
    body = copy.deepcopy(doc)
    match = _ERRORS_ID.search(str(body.pop("$id", "")))
    body.get("properties", {}).pop("schema_version", None)
    return {
        "errors document": (int(match.group(1)) if match else None, _canonical(body))
    }


def command_items(doc: dict) -> Items:
    items: Items = {}
    queue = [doc["root"]] if isinstance(doc.get("root"), dict) else []
    while queue:
        node = queue.pop()
        queue += node.get("commands", [])
        own = {
            key: value
            for key, value in node.items()
            if key not in ("version", "commands")
        }
        label = "command `" + " ".join(["ocx", *node.get("path", [])]) + "`"
        items[label] = (node.get("version"), _canonical(own))
    return items


def _is_version(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _bump_problems(head: Items, at_tag: Items, tag: str) -> list[str]:
    problems = []
    for label, (version, body) in sorted(head.items()):
        if not _is_version(version):
            problems.append(f"{label}: no integer version")
            continue
        if label not in at_tag:
            expected, why = 1, f"added since {tag}"
        else:
            tag_version, tag_body = at_tag[label]
            if not _is_version(tag_version):
                problems.append(f"{label}: no integer version at {tag}")
                continue
            changed = body != tag_body
            expected = tag_version + 1 if changed else tag_version
            why = f"bytes {'changed' if changed else 'unchanged'} since {tag} at version {tag_version}"
        if version != expected:
            problems.append(f"{label}: version {version}, expected {expected} ({why})")
    return problems


def version_byte_problems(repo: Path) -> list[str]:
    tag = bootstrap_tag(repo)
    if tag is None:
        return []
    if (repo / BASELINE_DIR).is_dir() and (repo / DIFFER).is_file():
        return []
    problems = []
    extract = {
        "reports.json": report_items,
        "errors.json": errors_items,
        "cli.json": command_items,
    }
    for name in DOCUMENTS:
        tree_file = repo / GOLDEN_DIR / name
        tag_blob = blob_at(repo, tag, f"{GOLDEN_DIR}/{name}")
        if not tree_file.is_file() or tag_blob is None:
            problems.append(
                f"{name}: missing at {'HEAD' if tag_blob is not None else tag}"
            )
            continue
        head_doc = json.loads(tree_file.read_text(encoding="utf-8"))
        head, at_tag = extract[name](head_doc), extract[name](json.loads(tag_blob))
        if not head or not at_tag:
            problems.append(
                f"{name}: read {len(head)} items at HEAD and {len(at_tag)} at {tag}"
            )
            continue
        problems += _bump_problems(head, at_tag, tag)
        if name == "errors.json":
            const = (
                head_doc.get("properties", {}).get("schema_version", {}).get("const")
            )
            major = head["errors document"][0]
            if const is not None and const != major:
                problems.append(
                    f"errors document: schema_version const {const} disagrees with the $id major {major}"
                )
    return problems


# --- the repository itself --------------------------------------------------


def test_the_repository_goldens_follow_the_bump_rule() -> None:
    assert version_byte_problems(Path(__file__).resolve().parents[2]) == []


# --- a scratch repository ---------------------------------------------------


def _reports(
    push: int = 1, copy: int = 3, digest: str = "^sha256:", extra: dict | None = None
) -> dict:
    defs: dict = {
        "PushReportRoot": {
            "type": "object",
            "properties": {
                "schema_version": {"const": push},
                "digest": {"$ref": "#/$defs/Digest"},
            },
            "required": ["schema_version", "digest"],
        },
        "CopyReportRoot": {
            "type": "object",
            "properties": {
                "schema_version": {"const": copy},
                "count": {"type": "integer"},
            },
            "required": ["schema_version", "count"],
        },
        "Digest": {"type": "string", "pattern": digest},
    }
    roots = {
        "PushReport": {"$ref": "#/$defs/PushReportRoot"},
        "CopyReport": {"$ref": "#/$defs/CopyReportRoot"},
    }
    for name, root in (extra or {}).items():
        defs[f"{name}Root"] = root
        roots[name] = {"$ref": f"#/$defs/{name}Root"}
    return {
        "$id": "https://ocx.sh/schemas/reports/v2.json",
        "reports": roots,
        "$defs": defs,
    }


def _errors(
    major: int = 1, command: str = "Canonical command path.", const: int | None = None
) -> dict:
    version: dict = (
        {"type": "integer"} if const is None else {"type": "integer", "const": const}
    )
    return {
        "$id": f"https://ocx.sh/schemas/errors/v{major}.json",
        "properties": {
            "schema_version": version,
            "command": {"type": "string", "description": command},
        },
    }


def _cli(
    push_version: int = 2, push_help: str = "Tag to push", group_version: int = 1
) -> dict:
    push = {
        "path": ["package", "push"],
        "version": push_version,
        "args": [{"id": "tag", "help": push_help}],
        "commands": [],
    }
    group = {
        "path": ["package"],
        "version": group_version,
        "args": [],
        "commands": [push],
    }
    return {
        "schema_version": 1,
        "root": {"path": [], "version": 1, "args": [], "commands": [group]},
    }


def _goldens(
    reports: dict | None = None, errors: dict | None = None, cli: dict | None = None
) -> dict[str, str | bytes | None]:
    docs = {
        "reports.json": reports or _reports(),
        "errors.json": errors or _errors(),
        "cli.json": cli or _cli(),
    }
    return {
        f"{GOLDEN_DIR}/{name}": json.dumps(doc, indent=2) + "\n"
        for name, doc in docs.items()
    }


def tagged(tmp_path: Path) -> Repo:
    """A repository whose bootstrap release `v1.0.0` carries the synthetic goldens."""
    repo = Repo(tmp_path)
    repo.commit(_goldens(), "release")
    repo.tag("v1.0.0")
    repo.commit({"README": "work\n"})
    return repo


def edit(repo: Repo, **docs: dict) -> None:
    repo.commit(_goldens(**docs))


def test_no_bootstrap_tag_leaves_the_rule_inert(tmp_path: Path) -> None:
    repo = Repo(tmp_path)
    repo.commit({"README": "x\n"})
    repo.tag("v0.6.4")
    repo.commit(_goldens(reports=_reports(digest="^changed")))
    assert version_byte_problems(repo.path) == []


def test_an_unchanged_tree_at_the_tag_versions_is_green(tmp_path: Path) -> None:
    assert version_byte_problems(tagged(tmp_path).path) == []


def test_a_reached_def_changed_without_a_bump_is_red(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    edit(repo, reports=_reports(digest="^sha512:"))
    assert version_byte_problems(repo.path) == [
        "report PushReport: version 1, expected 2 (bytes changed since v1.0.0 at version 1)"
    ]


def test_a_reached_def_changed_with_a_bump_is_green(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    edit(repo, reports=_reports(push=2, digest="^sha512:"))
    assert version_byte_problems(repo.path) == []


def test_a_bump_without_a_byte_change_is_red(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    edit(repo, reports=_reports(copy=4))
    assert version_byte_problems(repo.path) == [
        "report CopyReport: version 4, expected 3 (bytes unchanged since v1.0.0 at version 3)"
    ]


def test_an_errors_change_without_an_id_bump_is_red(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    edit(repo, errors=_errors(command="The command path."))
    assert version_byte_problems(repo.path) == [
        "errors document: version 1, expected 2 (bytes changed since v1.0.0 at version 1)"
    ]


def test_an_errors_change_with_an_id_bump_is_green(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    edit(repo, errors=_errors(major=2, command="The command path."))
    assert version_byte_problems(repo.path) == []


def test_an_in_band_errors_version_disagreeing_with_the_id_is_red(
    tmp_path: Path,
) -> None:
    repo = tagged(tmp_path)
    edit(repo, errors=_errors(major=2, command="The command path.", const=1))
    assert version_byte_problems(repo.path) == [
        "errors document: schema_version const 1 disagrees with the $id major 2"
    ]


def test_a_command_node_change_bumps_only_that_node(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    edit(repo, cli=_cli(push_help="Tag or digest to push"))
    assert version_byte_problems(repo.path) == [
        "command `ocx package push`: version 2, expected 3 (bytes changed since v1.0.0 at version 2)"
    ]
    edit(repo, cli=_cli(push_version=3, push_help="Tag or digest to push"))
    assert version_byte_problems(repo.path) == []


def test_an_added_root_starts_at_one(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    root = {"type": "object", "properties": {"schema_version": {"const": 2}}}
    edit(repo, reports=_reports(extra={"NewReport": root}))
    assert version_byte_problems(repo.path) == [
        "report NewReport: version 2, expected 1 (added since v1.0.0)"
    ]
    root["properties"]["schema_version"]["const"] = 1
    edit(repo, reports=_reports(extra={"NewReport": root}))
    assert version_byte_problems(repo.path) == []


def test_a_removed_root_is_unchecked(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    reports = _reports()
    del reports["reports"]["CopyReport"], reports["$defs"]["CopyReportRoot"]
    edit(repo, reports=reports)
    assert version_byte_problems(repo.path) == []


def test_a_root_without_a_version_const_is_red(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    reports = _reports()
    del reports["$defs"]["CopyReportRoot"]["properties"]["schema_version"]
    edit(repo, reports=reports)
    assert version_byte_problems(repo.path) == ["report CopyReport: no integer version"]


def test_a_document_read_as_empty_is_red(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    reports = _reports()
    reports["roots"] = reports.pop("reports")
    edit(repo, reports=reports)
    assert version_byte_problems(repo.path) == [
        "reports.json: read 0 items at HEAD and 2 at v1.0.0"
    ]


def test_a_live_gate_leaves_the_rule_inert(tmp_path: Path) -> None:
    repo = tagged(tmp_path)
    repo.commit({f"{BASELINE_DIR}/RELEASE": "v1.0.0\n"})
    edit(repo, reports=_reports(digest="^sha512:"))
    assert version_byte_problems(repo.path) != [], (
        "control: without the differ the rule is active"
    )
    repo.commit({DIFFER: "// differ\n"})
    assert version_byte_problems(repo.path) == []


def test_the_tag_reads_from_git_not_the_tree(tmp_path: Path) -> None:
    """The comparison side is the tag's blob: a tree edit at HEAD is the change, not the reference."""
    repo = tagged(tmp_path)
    (repo.path / GOLDEN_DIR / "cli.json").write_text("{}\n", encoding="utf-8")
    assert git(repo.path, "diff", "--quiet").returncode == 1, "mutation did not land"
    assert version_byte_problems(repo.path) == [
        "cli.json: read 0 items at HEAD and 3 at v1.0.0"
    ]
