#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`bazel:build:drift` — C-010. Every package's Bazel edge set vs its Cargo one.

    scripts/bazel_build_drift.py            # gate the live tree
    scripts/bazel_build_drift.py --self-test
    scripts/bazel_build_drift.py --update   # regenerate scripts/bazel_label_map.toml

`bazel_gate_proofs.py` (WP-13) already proved the comparator (`build_drift`)
red and green on tables it built by hand. This file supplies the two live
readings WP-13 left for WP-15 to own — a `bazel query --output=build` and a
`cargo metadata --locked` — and hands them to the imported comparator
unchanged. Nothing here re-spells the comparison.

**Why `bazel query --output=build` and never the BUILD file's text.** WP-12's
BUILD files get their third-party edges from crate_universe's
`all_crate_deps()` / `aliases()`, so the literal file text does not contain a
dep list at all: `crates/ocx_util/BUILD.bazel` says `deps = all_crate_deps(normal
= True)` and nothing else. A text parse would read every package as having zero
third-party edges and agree with itself forever. `--output=build` prints the
rule as loaded, after macro expansion, which is the only place the 81 distinct
`@crates//` labels on this tree exist.

**Why the resolved `cargo metadata` graph and not the manifests.** Three
reasons, all measured on this tree and all of them ways a manifest read goes
quietly wrong:

* A Cargo **rename** hides the package name. `crates/ocx_util/Cargo.toml` says
  `pki-types = { package = "rustls-pki-types" }`, and the Bazel label is
  `@crates//rustls-pki-types-1.15.1:rustls-pki-types-1.15.1`. The resolve graph
  reports the real name and the rename separately, so the two halves meet on
  the name that is actually in both. Four such renames exist here
  (`rustls-pki-types` three times, `sha2 -> sha2_oid` once).
* An **optional** dependency is in the manifest whether or not its feature is
  on; `all_crate_deps()` emits it only when it is. `ocx_config`'s `tempfile` is
  the one optional dep in this workspace and it happens to be enabled — so a
  manifest read is green today and reds the first time someone adds one that
  is not.
* A `cfg(...)`-**targeted** dependency is one manifest entry and a `select()`
  branch on the Bazel side. Both readers below take the union over every
  branch, which is the comparison that means "this edge exists somewhere",
  the only claim a `bazel query` (unconfigured) can support.

**Build-kind dependencies are excluded from the Cargo side, and say so.**
WP-12 generates no `cargo_build_script` target, so `ocx`'s `vergen-gix`
(`kind = "build"`, the workspace's only one) has no Bazel edge to drift
against; comparing it would red permanently and teach the operator to ignore
this gate. The exclusion is self-announcing rather than silent: the day a
`cargo_build_script` lands, its dep appears in the Bazel set with nothing
opposite it and this gate reds with `in BUILD not in Cargo`, which is the
prompt to revisit this paragraph.

**The self-edge is dropped, syntactically.** A `crates/<p>/tests/*.rs`
integration target depends on `//crates/<p>:<p>` — its own lib, which Cargo
links without a manifest entry. Five such targets exist. Only the label that
is exactly the package's own is dropped, so a real edge to a sibling crate is
never swallowed.

**The label map is the thing that makes a version bump loud.** A resolved
third-party label is `@crates//<name>-<version>:<name>-<version>` and the
version is not separable from the name by inspection: `toml` resolves to
`@crates//toml-1.1.4+spec-1.1.0:toml-1.1.4+spec-1.1.0`, where a `rsplit("-", 1)`
answers `toml-1.1.4+spec`. So no version stripping happens anywhere in this
file — every label is declared in `scripts/bazel_label_map.toml` or it is its
own exit 1 (C-010). `--update` regenerates that table from the same two
readings, which is what makes 82 rows maintainable; the diff it produces is
the review.

`--self-test` runs no subprocess and reads no built graph: WP-17 wires it into
`scripts:verify`, which must stay runnable on a tree where Bazel has never
been invoked. Every mutation in it is proven to have landed before its result
is trusted, fixtures live under this repo's own `.tmp/` (never `/tmp`), and
nothing is ever restored with `git checkout --`, which restores from the index
and would make the whole run vacuous.

Not wired into `taskfiles/scripts.taskfile.yml` — WP-17 owns that file. The
line it owes the `self-test:` list is named in `--self-test`'s closing output.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import re
import subprocess
import tempfile
import tomllib
from pathlib import Path

from bazel_gate_proofs import (
    CRATE_PACKAGES,
    CRATES_BINARY_TARGETS,
    CRATES_FILEGROUP_TARGETS,
    CRATES_LIB_TARGETS,
    CRATES_TEST_TARGETS,
    DRIFT_PACKAGE_FLOOR,
    DRIFT_TARGET_FLOOR,
    REPO_ROOT,
    Finding,
    build_drift,
    codes,
    report,
    resolve_label,
)

LABEL_MAP_PATH = REPO_ROOT / "scripts" / "bazel_label_map.toml"
TEST_TARGET_MAP_PATH = REPO_ROOT / "crates" / "TEST_TARGET_MAP.toml"

#: The one query. `kind(rule, ...)` and not `//crates/...:all`, because the
#: latter also yields source files, and `kind(rule, ...)` is the universe
#: `DRIFT_TARGET_FLOOR` was measured over.
QUERY = "kind(rule, //crates/...)"

# ---------------------------------------------------------------------------
# Messages for the reader floors this file owns. The comparison's own four
# messages live in `bazel_gate_proofs.py` and are not restated here.
# ---------------------------------------------------------------------------

QUERY_FAIL_MSG = (
    "BUILD drift: `bazel query '{query}' --output=build` failed (rc {rc}) — the gate read no "
    "Bazel edges at all. First 400 bytes of stderr: {stderr}"
)
CARGO_FAIL_MSG = (
    "BUILD drift: `cargo metadata --locked --format-version 1` failed (rc {rc}) — the gate read "
    "no Cargo edges at all. First 400 bytes of stderr: {stderr}"
)
CARGO_FLOOR_MSG = (
    "BUILD drift: the cargo reader yielded {members} workspace members, expected >= "
    "{floor} — it stopped early, and an empty Cargo side reads as 'every BUILD edge is extra'"
)
MAP_FILE_MSG = "BUILD drift: could not read the label map {path}: {reason}"
MAP_STALE_MSG = (
    "BUILD drift: the label map declares {label}, which no target under //crates/... depends on "
    "— a stale row. Every stale row is one more label the unmapped check can no longer catch; "
    "drop it, or re-run --update"
)
ORPHAN_RULE_MSG = (
    "BUILD drift: {count} rule(s) in the query output carry no "
    "`# .../crates/<pkg>/BUILD.bazel:<line>:<col>` header, so the reader cannot place them in a "
    "package — their edges were read by nobody"
)
UPDATE_UNDERIVABLE_MSG = (
    "BUILD drift --update: {label} matches no `<name>-<version>` in the cargo resolve graph and "
    "has no existing row to keep. Add the row by hand; the map was NOT rewritten"
)

# ---------------------------------------------------------------------------
# The `--output=build` grammar, as this binary prints it (measured on bazel
# 9.2.0). Three facts the parser rests on, each checked by `--self-test`:
#
#   1. Every rule is preceded by a `# <abs path>:<line>:<col>` header naming
#      the BUILD file it was instantiated from. That is the only place the
#      package appears — `--output=build` prints `name = "..."` but never a
#      full label.
#   2. Every attribute is exactly ONE line, `  <name> = <value>`, whether the
#      value is a list, a `select()`, or a concatenation of both. Measured:
#      56 rules, 15 distinct attribute names, zero continuation lines.
#   3. Labels are plain double-quoted strings containing no escape.
#
# A `select()`'s KEYS are string literals too (`"//conditions:default"`,
# `"@rules_rust//rust/platform:..."`). Neither matches the two dep-label
# prefixes, which is why extracting every string on the line is safe here and
# would not be if the prefixes were widened.
# ---------------------------------------------------------------------------

BUILD_HEADER = re.compile(r"^# (?:.*/)?crates/(?P<pkg>[A-Za-z0-9_]+)/BUILD\.bazel:\d+:\d+$")
RULE_OPEN = re.compile(r"^(?P<kind>[a-z_][a-z0-9_]*)\($")
RULE_CLOSE = re.compile(r"^\)$")
DEP_ATTR = re.compile(r"^  (?:deps|proc_macro_deps) = (?P<value>.*)$")
STRING_LITERAL = re.compile(r'"([^"]*)"')
DEP_LABEL_PREFIX = ("@crates//", "//crates/")
# Every kind that is a `crates/TEST_TARGET_MAP.toml` row. `sh_test` is the one
# `ocx_cli:ocx_cli_seam_test`: it runs `ocx_cli_test`'s own binary under a
# poisoned environment rather than compiling the crate a second time, and it
# still carries a row (and a floored case count) of its own.
TEST_RULE_KINDS = frozenset({"rust_test", "sh_test"})


@dataclasses.dataclass
class BuildRead:
    """What one `bazel query --output=build` yielded, counts kept separately.

    `packages` / `targets` / `rust_tests` are the reader's own tallies and are
    deliberately not derived from `deps`: a package whose only rule is a
    `filegroup` contributes no dep entry, so a count taken from `len(deps)`
    would report 19 of 20 packages on a tree where nothing is wrong — and the
    same arithmetic would report 19 on a tree where a whole package vanished.
    """

    deps: dict[str, set[str]] = dataclasses.field(default_factory=dict)
    packages: set[str] = dataclasses.field(default_factory=set)
    targets: int = 0
    rust_tests: int = 0
    orphan_rules: int = 0


def read_build_output(text: str) -> BuildRead:
    """`--output=build` text -> `{crates/<pkg>: {label}}` plus the reader's tallies.

    The package's own lib label is dropped where it appears inside that same
    package: that is the integration-test self-edge, which Cargo does not
    spell in a manifest. Dropped by exact string equality with
    `//crates/<pkg>:<pkg>`, so an edge to a sibling crate cannot be caught by
    it.

    The current package is cleared at each rule's closing `)`, so "orphan"
    means *this rule* had no header of its own rather than "no header has been
    seen yet". Without the reset, one missing header would silently file a
    whole rule's edges under the previous rule's package — a mis-attribution
    that reads as drift in two packages at once and names neither cause.
    """
    read = BuildRead()
    package: str | None = None
    for line in text.splitlines():
        header = BUILD_HEADER.match(line)
        if header is not None:
            package = f"crates/{header.group('pkg')}"
            read.packages.add(package)
            read.deps.setdefault(package, set())
            continue
        opener = RULE_OPEN.match(line)
        if opener is not None:
            read.targets += 1
            if opener.group("kind") in TEST_RULE_KINDS:
                read.rust_tests += 1
            if package is None:
                read.orphan_rules += 1
            continue
        if RULE_CLOSE.match(line):
            package = None
            continue
        attribute = DEP_ATTR.match(line)
        if attribute is None or package is None:
            continue
        self_edge = f"//{package}:{package.rsplit('/', 1)[1]}"
        for literal in STRING_LITERAL.findall(attribute.group("value")):
            if literal.startswith(DEP_LABEL_PREFIX) and literal != self_edge:
                read.deps[package].add(literal)
    return read


def read_cargo_metadata(document: dict) -> tuple[dict[str, set[str]], dict[str, str]]:
    """`cargo metadata --locked` -> `{crates/<dir>: {package name}}`, and the
    directory-to-package-name table `--update` derives the first-party rows from.

    Read off `resolve.nodes`, not off `packages[].dependencies`: the resolve
    graph is feature-resolved and rename-resolved, and it is the same lock
    crate_universe generated the `@crates//` labels from.
    """
    by_id = {package["id"]: package for package in document["packages"]}
    nodes = {node["id"]: node for node in document["resolve"]["nodes"]}
    cargo: dict[str, set[str]] = {}
    own_name: dict[str, str] = {}
    for member in document["workspace_members"]:
        package = by_id[member]
        key = "crates/" + package["manifest_path"].rsplit("/", 2)[-2]
        own_name[key] = package["name"]
        names: set[str] = set()
        for dependency in nodes[member]["deps"]:
            kinds = {entry["kind"] for entry in dependency["dep_kinds"]}
            # `{"build"}` only. A dep that is *also* normal or dev keeps its
            # edge — `kinds <= {"build"}` and not `"build" in kinds`.
            if kinds <= {"build"}:
                continue
            names.add(by_id[dependency["pkg"]]["name"])
        cargo[key] = names
    return cargo, own_name


def read_label_map(path: Path) -> tuple[dict[str, str], Finding | None]:
    """`scripts/bazel_label_map.toml`'s `[label]` table, or a reader-floor Finding."""
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}, Finding("drift-map-file", MAP_FILE_MSG.format(path=path, reason="<absent>"))
    except tomllib.TOMLDecodeError as error:
        return {}, Finding(
            "drift-map-file", MAP_FILE_MSG.format(path=path, reason=f"invalid TOML: {error}")
        )
    table = document.get("label")
    if not isinstance(table, dict) or not table:
        return {}, Finding(
            "drift-map-file", MAP_FILE_MSG.format(path=path, reason="no non-empty [label] table")
        )
    return dict(table), None


def read_test_map_rows(path: Path) -> int:
    """`crates/TEST_TARGET_MAP.toml`'s `[[target]]` row count, 0 if unreadable.

    0 is the honest answer for "could not read it": `build_drift` compares it
    against the test targets (`TEST_RULE_KINDS`) the query found, so an unreadable map is a
    `drift-map-parity` red rather than an exception nobody sees.
    """
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, tomllib.TOMLDecodeError):
        return 0
    rows = document.get("target")
    return len(rows) if isinstance(rows, list) else 0


def stale_map_rows(label_map: dict[str, str], read: BuildRead) -> list[Finding]:
    """Rows for labels nothing depends on — the mirror of `drift-unmapped`.

    This defends the gate rather than the tree. `drift-unmapped` only bites on
    a label with no row, so a map that only ever grows converges on a table
    that maps everything and catches nothing.

    Scoped to `@crates//` rows, and that scope is the point. A first-party row
    states a fact about the workspace LAYOUT — `crates/ocx_cli` is the package
    `ocx` — which stays true on a day nothing happens to depend on that crate.
    Sweeping it as stale would delete the row, and the edge's return would then
    resolve `//crates/ocx_cli:ocx_cli` to `ocx_cli` through `FIRST_PARTY_LABEL`
    and report drift against a tree where nothing is wrong.
    """
    live = {label for labels in read.deps.values() for label in labels}
    declared = {label for label in label_map if label.startswith("@crates//")}
    return [
        Finding("drift-map-stale", MAP_STALE_MSG.format(label=label))
        for label in sorted(declared - live)
    ]


def check_drift(
    *,
    build_text: str,
    metadata: dict,
    label_map: dict[str, str],
    test_map_rows: int,
) -> list[Finding]:
    """The two live readings, then the imported comparator. No comparison here."""
    read = read_build_output(build_text)
    cargo, _ = read_cargo_metadata(metadata)

    findings: list[Finding] = []
    if read.orphan_rules:
        findings.append(
            Finding("drift-orphan-rule", ORPHAN_RULE_MSG.format(count=read.orphan_rules))
        )
    if len(cargo) < CRATE_PACKAGES:
        findings.append(
            Finding(
                "drift-cargo-floor",
                CARGO_FLOOR_MSG.format(members=len(cargo), floor=CRATE_PACKAGES),
            )
        )
    # The stale sweep's premise is a COMPLETE read: on a run that read
    # nothing, every row in the map is trivially "stale", and 82 lines saying
    # so would corroborate the phantom that the map is wrong instead of
    # leaving the reader floor to say what actually happened. Measured — a
    # `bazel` that exits 0 with empty stdout produced exactly that.
    if len(read.packages) >= DRIFT_PACKAGE_FLOOR and read.targets >= DRIFT_TARGET_FLOOR:
        findings += stale_map_rows(label_map, read)
    findings += build_drift(
        cargo=cargo,
        build=read.deps,
        label_map=label_map,
        packages_read=len(read.packages),
        targets_read=read.targets,
        rust_test_targets_read=read.rust_tests,
        test_map_rows=test_map_rows,
    )
    # One line per distinct reason. `build_drift` raises `drift-unmapped`
    # inside its per-package loop, so a single retired map row otherwise
    # prints the same sentence once per package that depends on it — measured
    # here: deleting the `async-trait` row printed it 9 times, interleaved
    # with the 9 `drift-set` lines that name different packages and must all
    # survive. Exact-message duplicates only; nothing package-specific is
    # collapsed.
    return list(dict.fromkeys(findings))


# ---------------------------------------------------------------------------
# The subprocesses. `--self-test` reaches none of them.
# ---------------------------------------------------------------------------


def run_query(bazel_bin: str) -> tuple[str, list[Finding]]:
    """`bazel query 'kind(rule, //crates/...)' --output=build`.

    A zero-match query exits 0 with empty stdout (measured), so rc is never the
    floor here — `build_drift`'s package/target counts are. rc is still read,
    because a *failed* query and an *empty* one deserve different sentences.
    """
    command = [bazel_bin, "--host_jvm_args=-Xmx2g", "query", QUERY, "--output=build"]
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, encoding="utf-8", check=False, timeout=900
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return "", [Finding("drift-query", QUERY_FAIL_MSG.format(query=QUERY, rc="n/a", stderr=error))]
    if result.returncode != 0:
        return "", [
            Finding(
                "drift-query",
                QUERY_FAIL_MSG.format(query=QUERY, rc=result.returncode, stderr=result.stderr[:400]),
            )
        ]
    return result.stdout, []


def run_cargo_metadata() -> tuple[dict, list[Finding]]:
    """`cargo metadata --locked --format-version 1`, parsed."""
    command = ["cargo", "metadata", "--locked", "--format-version", "1"]
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
            timeout=900,
            cwd=REPO_ROOT,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return {}, [Finding("drift-cargo", CARGO_FAIL_MSG.format(rc="n/a", stderr=error))]
    if result.returncode != 0:
        return {}, [
            Finding(
                "drift-cargo",
                CARGO_FAIL_MSG.format(rc=result.returncode, stderr=result.stderr[:400]),
            )
        ]
    try:
        return json.loads(result.stdout), []
    except json.JSONDecodeError as error:
        return {}, [Finding("drift-cargo", CARGO_FAIL_MSG.format(rc=0, stderr=f"unparseable: {error}"))]


def gather(
    *, bazel_bin: str, query_output: Path | None, metadata_path: Path | None
) -> tuple[str, dict, list[Finding]]:
    """The live tree's two readings, or the pre-captured ones."""
    if query_output is not None:
        build_text, query_findings = query_output.read_text(encoding="utf-8"), []
    else:
        build_text, query_findings = run_query(bazel_bin)
    if metadata_path is not None:
        metadata, cargo_findings = json.loads(metadata_path.read_text(encoding="utf-8")), []
    else:
        metadata, cargo_findings = run_cargo_metadata()
    return build_text, metadata, query_findings + cargo_findings


def run_check(*, bazel_bin: str, query_output: Path | None, metadata_path: Path | None) -> list[Finding]:
    build_text, metadata, findings = gather(
        bazel_bin=bazel_bin, query_output=query_output, metadata_path=metadata_path
    )
    if findings:
        # A reading that failed outright makes every downstream verdict
        # unattributable — report the failure alone rather than 20 packages'
        # worth of drift caused by it.
        return findings
    label_map, map_finding = read_label_map(LABEL_MAP_PATH)
    if map_finding is not None:
        return [map_finding]
    return check_drift(
        build_text=build_text,
        metadata=metadata,
        label_map=label_map,
        test_map_rows=read_test_map_rows(TEST_TARGET_MAP_PATH),
    )


# ---------------------------------------------------------------------------
# `--update` — the map's derivation, in code rather than in someone's head.
# ---------------------------------------------------------------------------

MAP_PREAMBLE = """\
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# C-010's name-mapping table: every Bazel dep label under `//crates/...` that
# `bazel:build:drift` may see, mapped to the Cargo package name it is.
# GENERATED — regenerate with `python3 scripts/bazel_build_drift.py --update`.
#
# A label with no row here is its own exit 1 (`drift-unmapped`), and a row for
# a label nothing depends on is its own exit 1 too (`drift-map-stale`). Both
# directions are gated because only the pair keeps this table honest: a map
# that only grows ends up mapping everything, and then the unmapped check
# catches nothing.
#
# Two derivations, both mechanical:
#
#   `@crates//<name>-<version>:<name>-<version>` -> the workspace-resolved
#       package whose `<name>-<version>` is exactly that stem. Never a
#       `rsplit("-", 1)`: `toml` resolves to `toml-1.1.4+spec-1.1.0`, whose
#       version contains a `-`, so the split answers `toml-1.1.4+spec`. The
#       table exists precisely because the two halves are not separable by
#       inspection — which is also why a `cargo update` that moves a pin
#       retires the row and reds this gate until it is re-derived.
#
#   `//crates/<dir>:<dir>` -> the member's Cargo package name, listed ONLY
#       where the two differ. `bazel_gate_proofs.resolve_label` falls through
#       to `//crates/<x>:<x>` -> `<x>` for the other 20, so listing them would
#       be 20 rows saying nothing. One member differs: `crates/ocx_cli` is the
#       package `ocx`.
#
# The Cargo-rename trap the plan recorded does NOT appear here. This workspace
# renames `rustls-pki-types` to `pki-types` and `sha2` to `sha2_oid`, and
# neither spelling occurs in any label: crate_universe keys on the package
# name. `sha2` is in this table twice, at 0.10.9 and 0.11.0, which is the case
# a single `@crates//:sha2` alias could not have expressed.

[label]
"""


def derive_label_map(
    *, build_text: str, metadata: dict, existing: dict[str, str]
) -> tuple[dict[str, str], list[Finding]]:
    """Both halves of the table, from the same two readings the gate uses."""
    read = read_build_output(build_text)
    _, own_name = read_cargo_metadata(metadata)
    by_stem = {f"{p['name']}-{p['version']}": p["name"] for p in metadata["packages"]}

    derived: dict[str, str] = {}
    findings: list[Finding] = []
    for label in sorted({label for labels in read.deps.values() for label in labels}):
        if label.startswith("@crates//"):
            stem = label[len("@crates//") :].split(":", 1)[0]
            if stem in by_stem:
                derived[label] = by_stem[stem]
            elif label in existing:
                # Kept, not dropped: a hand-added row for a label crate_universe
                # spells some other way must survive a regeneration.
                derived[label] = existing[label]
            else:
                findings.append(
                    Finding("update-underivable", UPDATE_UNDERIVABLE_MSG.format(label=label))
                )
    for key, name in sorted(own_name.items()):
        directory = key.rsplit("/", 1)[1]
        if directory != name:
            derived[f"//{key}:{directory}"] = name
    return derived, findings


def render_label_map(mapping: dict[str, str]) -> str:
    lines = [f'"{label}" = "{name}"' for label, name in sorted(mapping.items())]
    return MAP_PREAMBLE + "\n".join(lines) + "\n"


def run_update(*, bazel_bin: str, query_output: Path | None, metadata_path: Path | None) -> int:
    build_text, metadata, findings = gather(
        bazel_bin=bazel_bin, query_output=query_output, metadata_path=metadata_path
    )
    if findings:
        return report(findings)
    existing, _ = read_label_map(LABEL_MAP_PATH)
    mapping, derive_findings = derive_label_map(
        build_text=build_text, metadata=metadata, existing=existing
    )
    if derive_findings:
        return report(derive_findings)
    rendered = render_label_map(mapping)
    previous = LABEL_MAP_PATH.read_text(encoding="utf-8") if LABEL_MAP_PATH.is_file() else ""
    LABEL_MAP_PATH.write_text(rendered, encoding="utf-8")
    verb = "unchanged" if rendered == previous else "rewritten"
    print(f"{LABEL_MAP_PATH.relative_to(REPO_ROOT)}: {len(mapping)} rows, {verb}")
    return 0


# ---------------------------------------------------------------------------
# Self-test. No subprocess, no built graph: WP-17 wires this into
# `scripts:verify`, which runs on trees where Bazel has never been invoked.
# ---------------------------------------------------------------------------

#: The fixture's third-party labels. `toml` carries the real `+spec-1.1.0`
#: version so the fixture exercises the stem a `rsplit("-", 1)` would maul,
#: and `sha2` appears at two versions so the many-labels-one-name shape is in
#: the GREEN half rather than only asserted about in a comment.
TOKIO = "@crates//tokio-1.53.1:tokio-1.53.1"
SERDE = "@crates//serde-1.0.229:serde-1.0.229"
TOML = "@crates//toml-1.1.4+spec-1.1.0:toml-1.1.4+spec-1.1.0"
ASYNC_TRAIT = "@crates//async-trait-0.1.91:async-trait-0.1.91"
SHA2_OLD = "@crates//sha2-0.10.9:sha2-0.10.9"
SHA2_NEW = "@crates//sha2-0.11.0:sha2-0.11.0"
PKI_TYPES = "@crates//rustls-pki-types-1.15.1:rustls-pki-types-1.15.1"
OCX_CLI_LABEL = "//crates/ocx_cli:ocx_cli"

FIXTURE_MAP = {
    TOKIO: "tokio",
    SERDE: "serde",
    TOML: "toml",
    ASYNC_TRAIT: "async-trait",
    SHA2_OLD: "sha2",
    SHA2_NEW: "sha2",
    PKI_TYPES: "rustls-pki-types",
    OCX_CLI_LABEL: "ocx",
}

HUB = "ocx_util"
LEAF = "ocx_exit"


def expect(condition: bool, problem: str) -> None:
    """A loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not condition:
        raise SystemExit(f"bazel build drift self-test: {problem}")


def _rule(package: str, kind: str, name: str, *, deps: list[str], proc_macro: list[str]) -> str:
    """One `--output=build` record, in the grammar bazel 9.2.0 prints."""
    body = [
        f"# /abs/path/{package}/BUILD.bazel:9:13",
        f"{kind}(",
        f'  name = "{name}",',
        "  deps = [" + ", ".join(f'"{d}"' for d in deps) + "],",
    ]
    if proc_macro:
        body.append("  proc_macro_deps = [" + ", ".join(f'"{d}"' for d in proc_macro) + "],")
    body.append(")")
    body.append("# Rule instantiated at (most recent call last):")
    return "\n".join(body)


def sample_tree() -> tuple[str, dict]:
    """A synthetic tree with this repository's real shape: `CRATE_PACKAGES`
    packages, `DRIFT_TARGET_FLOOR` rule targets, `CRATES_TEST_TARGETS` of them
    `rust_test`, and a Cargo side that agrees with all of it.

    The shape is copied because two of its irregularities are the ones a
    reader gets wrong. `ocx_shim` is a `[[bin]]` with no lib, so it holds a
    `rust_test` and no `rust_library` — that is why `CRATES_LIB_TARGETS` is 19
    against 20 packages. And `crates/ocx_cli` is the package `ocx`, which is
    the one first-party mapping row that has to exist.
    """
    directories = [LEAF, HUB, "ocx_shim"] + [
        f"pkg{index:02d}" for index in range(4, CRATE_PACKAGES + 1)
    ]
    expect(len(directories) == CRATE_PACKAGES, f"the fixture has {len(directories)} packages")
    directories[-1] = "ocx_cli"

    records: list[str] = []
    packages: list[dict] = []
    nodes: list[dict] = []
    members: list[str] = []
    third_party = {
        TOKIO: ("tokio", "1.53.1"),
        SERDE: ("serde", "1.0.229"),
        TOML: ("toml", "1.1.4+spec-1.1.0"),
        ASYNC_TRAIT: ("async-trait", "0.1.91"),
        SHA2_OLD: ("sha2", "0.10.9"),
        SHA2_NEW: ("sha2", "0.11.0"),
        PKI_TYPES: ("rustls-pki-types", "1.15.1"),
    }
    for label, (name, version) in third_party.items():
        packages.append({"id": label, "name": name, "version": version, "manifest_path": "/x/Cargo.toml"})

    libs = 0
    tests = 0
    binaries = 0
    filegroups = 0
    integration_budget = CRATES_TEST_TARGETS - CRATE_PACKAGES
    for index, directory in enumerate(directories):
        package = f"crates/{directory}"
        name = "ocx" if directory == "ocx_cli" else directory
        # Third-party edges: every package takes tokio; a rotating extra makes
        # the per-package assertion in the mutation matrix non-vacuous.
        normal = [TOKIO] + ([SERDE] if index % 2 else [TOML])
        if index == 3:
            normal.append(SHA2_OLD)
        if directory == HUB:
            normal += [SHA2_NEW, PKI_TYPES]
        first_party = [] if directory in (LEAF, HUB) else [HUB, LEAF]
        # One package reaches the crate whose directory and package name
        # differ, so `//crates/ocx_cli:ocx_cli -> ocx` is exercised by the
        # GREEN comparison and not only by a `resolve_label` assertion.
        if index == CRATE_PACKAGES - 2:
            first_party.append("ocx_cli")
        edges = normal + [f"//crates/{crate}:{crate}" for crate in first_party]
        if directory == "ocx_shim":
            # No lib: the package's only rule is the `[[bin]]` unit-test
            # target, and it carries the package's edges.
            records.append(
                _rule(package, "rust_test", f"{directory}_bin_test", deps=edges, proc_macro=[ASYNC_TRAIT])
            )
            tests += 1
        else:
            records.append(
                _rule(package, "rust_library", directory, deps=edges, proc_macro=[ASYNC_TRAIT])
            )
            libs += 1
            records.append(_rule(package, "rust_test", f"{directory}_test", deps=[], proc_macro=[]))
            tests += 1
            if integration_budget > 0:
                # The integration target, carrying the self-edge back onto its
                # own lib — the label Cargo never spells in a manifest.
                records.append(
                    _rule(
                        package,
                        "rust_test",
                        f"{directory}_integration",
                        deps=[f"//crates/{directory}:{directory}"],
                        proc_macro=[],
                    )
                )
                tests += 1
                integration_budget -= 1
        if index < CRATES_FILEGROUP_TARGETS:
            records.append(_rule(package, "filegroup", f"{directory}_data", deps=[], proc_macro=[]))
            filegroups += 1
        if binaries < CRATES_BINARY_TARGETS and directory != "ocx_shim":
            # The `rust_binary` over a package's own `src/main.rs`
            # (`ocx_schema:ocx_schema_bin`, plan_test_speed_tiers.md C-020): its
            # one edge is its own lib, which the self-edge rule drops.
            records.append(
                _rule(package, "rust_binary", f"{directory}_bin", deps=[f"//crates/{directory}:{directory}"], proc_macro=[])
            )
            binaries += 1

        member_id = f"member::{name}"
        members.append(member_id)
        packages.append(
            {
                "id": member_id,
                "name": name,
                "version": "0.6.2",
                "manifest_path": f"/abs/path/crates/{directory}/Cargo.toml",
            }
        )
        deps = [
            {"pkg": label, "name": label, "dep_kinds": [{"kind": None, "target": None}]}
            for label in normal + [ASYNC_TRAIT]
        ]
        deps += [
            {
                "pkg": f"member::{'ocx' if crate == 'ocx_cli' else crate}",
                "name": crate,
                "dep_kinds": [{"kind": None, "target": None}],
            }
            for crate in first_party
        ]
        nodes.append({"id": member_id, "deps": deps})
    # The build-kind dep the Bazel side deliberately has no edge for.
    packages.append({"id": "vergen", "name": "vergen-gix", "version": "9.1.0", "manifest_path": "/x/Cargo.toml"})
    nodes[-1]["deps"].append(
        {"pkg": "vergen", "name": "vergen_gix", "dep_kinds": [{"kind": "build", "target": None}]}
    )

    expect(
        libs == CRATES_LIB_TARGETS
        and tests == CRATES_TEST_TARGETS
        and binaries == CRATES_BINARY_TARGETS
        and filegroups == CRATES_FILEGROUP_TARGETS,
        f"fixture shape is {libs}/{tests}/{binaries}/{filegroups}, expected "
        f"{CRATES_LIB_TARGETS}/{CRATES_TEST_TARGETS}/{CRATES_BINARY_TARGETS}/{CRATES_FILEGROUP_TARGETS}",
    )
    metadata = {"packages": packages, "workspace_members": members, "resolve": {"nodes": nodes}}
    return "\n".join(records) + "\n", metadata


def _first_party_only_gate(text: str, metadata: dict) -> list[Finding]:
    """WRONG, and kept as a named control called from `--self-test` and nowhere else.

    The scope C-010 had to be widened away from: compare only the workspace's
    own crates, on BOTH sides. Silent on a tree where a third-party edge has
    been dropped — which is the whole reason the contract says "full dep set".
    Filtering only the Bazel side would instead be deafening, so it would not
    be the control it is meant to be.
    """
    read = read_build_output(text)
    cargo, own_name = read_cargo_metadata(metadata)
    members = set(own_name.values())
    return build_drift(
        cargo={package: names & members for package, names in cargo.items()},
        build={
            package: {label for label in labels if label.startswith("//crates/")}
            for package, labels in read.deps.items()
        },
        label_map=FIXTURE_MAP,
        packages_read=len(read.packages),
        targets_read=read.targets,
        rust_test_targets_read=read.rust_tests,
        test_map_rows=CRATES_TEST_TARGETS,
    )


def prove_readers(text: str, metadata: dict) -> int:
    """The three grammar facts the parser rests on, asserted rather than assumed."""
    checks = 0
    read = read_build_output(text)
    expect(
        len(read.packages) == DRIFT_PACKAGE_FLOOR and read.targets == DRIFT_TARGET_FLOOR,
        f"reader saw {len(read.packages)} packages / {read.targets} targets, expected "
        f"{DRIFT_PACKAGE_FLOOR} / {DRIFT_TARGET_FLOOR}",
    )
    expect(read.rust_tests == CRATES_TEST_TARGETS, f"reader saw {read.rust_tests} rust_test targets")
    expect(read.orphan_rules == 0, "a well-formed fixture must produce no orphan rules")
    print(
        f"reader GREEN: {len(read.packages)} packages, {read.targets} rule targets, "
        f"{read.rust_tests} rust_test, 0 orphan rules"
    )
    checks += 1

    # The self-edge drop is exact, not a blanket "ignore own-package labels".
    integration = f"//crates/{LEAF}:{LEAF}"
    expect(integration not in read.deps[f"crates/{LEAF}"], "the self-edge was not dropped")
    holder = next(package for package, labels in read.deps.items() if integration in labels)
    expect(holder != f"crates/{LEAF}", "the self-edge drop must not eat a sibling's edge")
    print(f"reader GREEN: {integration} dropped inside crates/{LEAF}, kept in {holder}")
    checks += 1

    # A `select()` on a deps line yields its labels and none of its keys.
    selected = (
        '# /abs/path/crates/ocx_shim/BUILD.bazel:9:13\nrust_library(\n  name = "ocx_shim",\n'
        '  deps = [] + select({"@rules_rust//rust/platform:x86_64-pc-windows-msvc": '
        f'["{TOKIO}"], "//conditions:default": []}}),\n)\n'
    )
    picked = read_build_output(selected).deps["crates/ocx_shim"]
    expect(picked == {TOKIO}, f"select() branch labels not read cleanly, got {picked}")
    print("reader GREEN: a select() deps line yields its branch labels and neither of its keys")
    checks += 1

    # Build-kind deps are excluded, and only build-kind ones.
    cargo, own_name = read_cargo_metadata(metadata)
    expect(own_name["crates/ocx_cli"] == "ocx", "the cargo reader lost the ocx_cli -> ocx rename")
    expect("vergen-gix" not in cargo["crates/ocx_cli"], "a build-kind dep reached the cargo set")
    expect(len(cargo) == CRATE_PACKAGES, f"cargo reader saw {len(cargo)} members")
    print(f"reader GREEN: {CRATE_PACKAGES} members, build-kind `vergen-gix` excluded, ocx_cli -> ocx")
    checks += 1
    return checks


def prove_drift_gate(text: str, metadata: dict) -> int:
    """C-010's three failure modes plus both directions, each shown red and green."""
    checks = 0
    baseline = {"label_map": FIXTURE_MAP, "test_map_rows": CRATES_TEST_TARGETS}

    green = check_drift(build_text=text, metadata=metadata, **baseline)
    expect(green == [], f"the agreeing tree must be silent, got {[f.message for f in green]}")
    expect(
        resolve_label(OCX_CLI_LABEL, FIXTURE_MAP) == "ocx",
        "the first-party rename row is not consulted before FIRST_PARTY_LABEL",
    )
    expect(
        FIXTURE_MAP[SHA2_OLD] == FIXTURE_MAP[SHA2_NEW] == "sha2",
        "the two-versions-one-name row pair is not in the green half",
    )
    print(
        f"C-010 GREEN: {DRIFT_PACKAGE_FLOOR} packages / {DRIFT_TARGET_FLOOR} targets agree, "
        "first-party and @crates//<name>-<version> edges both compared"
    )
    checks += 1

    # --- mode 1: a first-party edge dropped from BUILD.
    dropped = f'"//crates/{HUB}:{HUB}", '
    victim = "crates/pkg05"
    mutated = _drop_in_package(text, victim, dropped)
    expect(dropped in text, "the green fixture never had the first-party edge being dropped")
    expect(
        f"//crates/{HUB}:{HUB}" not in read_build_output(mutated).deps[victim],
        "the first-party drop did not land in the text the reader reads",
    )
    first_party = check_drift(build_text=mutated, metadata=metadata, **baseline)
    expect(codes(first_party) == ["drift-set"], f"got {codes(first_party)}")
    expect(victim in first_party[0].message, "the finding does not name the package")
    expect(
        f"in Cargo not in BUILD: ['{HUB}']" in first_party[0].message,
        f"the finding does not name the direction: {first_party[0].message}",
    )
    print(f"C-010 RED  : {first_party[0].message}")
    checks += 1

    # --- mode 2: a `@crates//` edge dropped — the half a first-party-only
    #     scope cannot see. Spelled as the real mutation: the whole
    #     `proc_macro_deps = all_crate_deps(proc_macro = True)` attribute
    #     going missing, which is how a macro-supplied edge actually vanishes.
    macro_line = f'  proc_macro_deps = ["{ASYNC_TRAIT}"],\n'
    mutated = _drop_in_package(text, victim, macro_line)
    expect(
        ASYNC_TRAIT not in read_build_output(mutated).deps[victim],
        "the @crates// drop did not land in the text the reader reads",
    )
    expect(ASYNC_TRAIT in read_build_output(text).deps[victim], "the green fixture never had it")
    third_party = check_drift(build_text=mutated, metadata=metadata, **baseline)
    expect(codes(third_party) == ["drift-set"], f"got {codes(third_party)}")
    expect(
        "in Cargo not in BUILD: ['async-trait']" in third_party[0].message,
        f"the third-party finding does not name async-trait: {third_party[0].message}",
    )
    print(f"C-010 RED  : {third_party[0].message}")
    checks += 1

    # The named control, on the same bytes: the scope that cannot see it.
    # It is shown green on the CLEAN tree first — a control that reds on
    # everything would be "blind" to this edge for the wrong reason.
    expect(
        _first_party_only_gate(text, metadata) == [],
        "the first-party-only control must be silent on the clean tree to be a control at all",
    )
    blind = _first_party_only_gate(mutated, metadata)
    expect(
        blind == [],
        f"the first-party-only control was supposed to be blind to the dropped @crates// edge, "
        f"got {[f.message for f in blind]}",
    )
    print(
        "C-010 CONTROL: the first-party-only gate is silent on the very bytes the real one reds on "
        "— that blindness is why C-010 says 'full dep set'"
    )
    checks += 1

    # --- the other direction: an edge BUILD has and Cargo does not. Removed
    #     from the Cargo side, and proven present BEFORE as well as absent
    #     after: "it is not there now" is satisfied by a mutation that never
    #     applied, which is the shape that makes a whole case vacuous.
    def _pkgs(document: dict) -> set[str]:
        node = next(n for n in document["resolve"]["nodes"] if n["id"] == "member::pkg05")
        return {dependency["pkg"] for dependency in node["deps"]}

    cut_cargo = json.loads(json.dumps(metadata))
    expect(TOML in _pkgs(cut_cargo), "the green fixture never had the cargo edge being removed")
    for node in cut_cargo["resolve"]["nodes"]:
        if node["id"] == "member::pkg05":
            node["deps"] = [d for d in node["deps"] if d["pkg"] != TOML]
    expect(TOML not in _pkgs(cut_cargo), "the cargo-side removal did not land")
    reverse = check_drift(build_text=text, metadata=cut_cargo, **baseline)
    expect(codes(reverse) == ["drift-set"], f"got {codes(reverse)}")
    expect(
        "in BUILD not in Cargo: ['toml']" in reverse[0].message,
        f"the added-direction finding is wrong: {reverse[0].message}",
    )
    print(f"C-010 RED  : {reverse[0].message}")
    checks += 1

    # --- mode 3: an unmapped label is its own exit 1, naming the label.
    #     Spelled as a VERSION bump, the way a row actually goes missing: the
    #     version is in both halves of the label, so every `cargo update` that
    #     moves a pin retires a row.
    bumped = "@crates//tokio-1.54.0:tokio-1.54.0"
    mutated = text.replace(TOKIO, bumped)
    expect(bumped in read_build_output(mutated).deps[victim], "the version bump did not land")
    expect(bumped not in FIXTURE_MAP, "the bumped label must have no row")
    unmapped = check_drift(build_text=mutated, metadata=metadata, **baseline)
    expect("drift-unmapped" in codes(unmapped), f"got {codes(unmapped)}")
    named = [f for f in unmapped if f.code == "drift-unmapped"]
    expect(len(named) == 1, f"one retired row must print one line, got {len(named)}")
    expect(bumped in named[0].message, "the finding does not name the label")
    expect("add a row" in named[0].message, "the finding does not tell the operator what to do")
    # The per-package drift-set lines must NOT be collapsed with it: the bump
    # touches every package, and each names its own.
    per_package = {f.message for f in unmapped if f.code == "drift-set"}
    expect(
        len(per_package) == DRIFT_PACKAGE_FLOOR,
        f"deduplication swallowed per-package findings: {len(per_package)} of {DRIFT_PACKAGE_FLOOR}",
    )
    print(f"C-010 RED  : {named[0].message}")
    print(
        f"C-010 RED  : ... once, beside {len(per_package)} per-package drift-set lines that are "
        "not collapsed with it"
    )
    checks += 1

    # --- and its mirror: a row for a label nothing depends on.
    stale = check_drift(
        build_text=text,
        metadata=metadata,
        label_map=FIXTURE_MAP | {bumped: "tokio"},
        test_map_rows=CRATES_TEST_TARGETS,
    )
    expect(codes(stale) == ["drift-map-stale"], f"got {codes(stale)}")
    print(f"C-010 RED  : {stale[0].message}")
    checks += 1
    return checks


def prove_floors(text: str, metadata: dict) -> int:
    """Every way this gate could read less than the tree and stay quiet."""
    checks = 0
    baseline = {"label_map": FIXTURE_MAP, "test_map_rows": CRATES_TEST_TARGETS}

    # A zero-match `bazel query` exits 0 and prints nothing (measured on
    # 9.2.0), so the empty read is the shape the floor exists for.
    empty = check_drift(build_text="", metadata=metadata, **baseline)
    expect("drift-reader-floor" in codes(empty), f"an empty query output must red, got {codes(empty)}")
    floor = next(f for f in empty if f.code == "drift-reader-floor")
    expect("0 packages / 0 targets" in floor.message, f"the floor line is wrong: {floor.message}")
    # And it must say only that. Every map row is trivially unused on a read
    # of nothing, so a stale sweep here would answer with one line per row —
    # a corroborating count for a conclusion ("the map is wrong") that the
    # evidence does not support.
    expect(
        "drift-map-stale" not in codes(empty),
        f"a read of nothing must not also accuse the map: {codes(empty)}",
    )
    print(f"C-010 RED  : {floor.message} (and no stale-row line, on a read of nothing)")
    checks += 1

    # A corpus union hides its members: "more than one package walked" is
    # satisfied by 19 of 20. Assert per package, both ways.
    read = read_build_output(text)
    cargo, _ = read_cargo_metadata(metadata)
    expect(set(read.deps) == set(cargo), "the two readers disagree about which packages exist")
    for package in sorted(cargo):
        expect(package in read.deps, f"{package} was not walked on the Bazel side")
        expect(read.deps[package], f"{package} was walked but contributed no edge")
    print(f"C-010 GREEN: all {len(cargo)} packages walked on BOTH sides, each contributing >= 1 edge")
    checks += 1

    # One package short of the floor still reds — the union must not absorb
    # it. The count is asserted EXACTLY: a reader that returned 0 would also
    # satisfy "the floor fired", and then this case would be passing for a
    # reason it does not name.
    minus_one = _drop_package(text, "crates/pkg07")
    trimmed = read_build_output(minus_one)
    expect(
        len(trimmed.packages) == DRIFT_PACKAGE_FLOOR - 1,
        f"the one-package removal left {len(trimmed.packages)} packages, expected "
        f"{DRIFT_PACKAGE_FLOOR - 1} — the mutation did not land the way this case claims",
    )
    expect(trimmed.orphan_rules == 0, "removing a package must not orphan the remaining rules")
    short = check_drift(build_text=minus_one, metadata=metadata, **baseline)
    expect("drift-reader-floor" in codes(short), f"19 of 20 packages must red, got {codes(short)}")
    print(
        "C-010 RED  : "
        + next(f for f in short if f.code == "drift-reader-floor").message
        + " (exactly one package removed)"
    )
    checks += 1

    # The cargo reader's own floor: an empty Cargo side otherwise reads as
    # "every BUILD edge is extra", which is 20 drift-set findings and the
    # wrong diagnosis.
    starved = check_drift(
        build_text=text,
        metadata={"packages": [], "workspace_members": [], "resolve": {"nodes": []}},
        **baseline,
    )
    expect("drift-cargo-floor" in codes(starved), f"got {codes(starved)}")
    print(f"C-010 RED  : {next(f for f in starved if f.code == 'drift-cargo-floor').message}")
    checks += 1

    # A rule with no header is a rule whose edges nobody read.
    orphan = check_drift(
        build_text='rust_library(\n  name = "nowhere",\n  deps = [],\n)\n' + text,
        metadata=metadata,
        **baseline,
    )
    expect("drift-orphan-rule" in codes(orphan), f"got {codes(orphan)}")
    print(f"C-010 RED  : {next(f for f in orphan if f.code == 'drift-orphan-rule').message}")
    checks += 1

    # ... and the case the `)`-reset exists for: a header missing MID-STREAM,
    # where the previous rule's package is still in hand and would otherwise
    # absorb the headerless rule's edges.
    header = "# /abs/path/crates/pkg09/BUILD.bazel:9:13\n"
    expect(text.count(header) >= 1, "the green fixture never had the header being removed")
    headerless = text.replace(header, "", 1)
    read = read_build_output(headerless)
    expect(read.orphan_rules == 1, f"a mid-stream missing header must orphan 1 rule, got {read.orphan_rules}")
    clean = read_build_output(text)
    expect(
        read.deps["crates/pkg08"] == clean.deps["crates/pkg08"],
        "the headerless rule's edges were absorbed by the preceding package",
    )
    mid = check_drift(build_text=headerless, metadata=metadata, **baseline)
    expect("drift-orphan-rule" in codes(mid), f"got {codes(mid)}")
    print(
        "C-010 RED  : "
        + next(f for f in mid if f.code == "drift-orphan-rule").message
        + " (header removed mid-stream; the previous package's edge set is unchanged)"
    )
    checks += 1

    # TEST_TARGET_MAP parity — the generated table the plan requires the floor
    # to be taken against, alongside WP-13's constants.
    parity = check_drift(
        build_text=text, metadata=metadata, label_map=FIXTURE_MAP, test_map_rows=CRATES_TEST_TARGETS - 1
    )
    expect(codes(parity) == ["drift-map-parity"], f"got {codes(parity)}")
    print(f"C-010 RED  : {parity[0].message}")
    checks += 1
    return checks


def prove_map_reader(scratch: Path) -> int:
    """The label map's own reader floor, on real files under `.tmp/`."""
    checks = 0
    path = scratch / "bazel_label_map.toml"
    good = '[label]\n"@crates//tokio-1.53.1:tokio-1.53.1" = "tokio"\n'
    path.write_text(good, encoding="utf-8")
    mapping, finding = read_label_map(path)
    expect(finding is None and mapping == {TOKIO: "tokio"}, f"a good map must read cleanly, got {mapping}")
    print("map GREEN  : a one-row [label] table reads back as one row, no findings")
    checks += 1

    for name, text, reason in [
        ("absent", None, "<absent>"),
        ("empty table", "[label]\n", "no non-empty [label] table"),
        ("wrong table", '[labels]\n"a" = "b"\n', "no non-empty [label] table"),
        ("invalid TOML", "[label\n", "invalid TOML"),
    ]:
        if text is None:
            path.unlink()
            expect(not path.exists(), "the deletion of the map did not land")
        else:
            path.write_text(text, encoding="utf-8")
            expect(path.read_text(encoding="utf-8") == text, f"the {name} mutation did not land")
        mapping, finding = read_label_map(path)
        expect(finding is not None, f"a {name} map must red")
        expect(reason in finding.message, f"the {name} finding does not say why: {finding.message}")
        print(f"map RED    : {finding.message}")
        checks += 1

    # Restore from our own bytes — never `git checkout --`, which restores
    # from the index and would make every result above unattributable.
    path.write_text(good, encoding="utf-8")
    expect(read_label_map(path)[1] is None, "restoring the map must green again")
    return checks


def prove_shipped_map() -> int:
    """The map this repository actually ships, against WP-12's real BUILD files.

    Free when they agree, and the case it matters most not to false-positive
    on. No subprocess: this reads the shipped file only.
    """
    mapping, finding = read_label_map(LABEL_MAP_PATH)
    expect(finding is None, f"the shipped label map must read cleanly: {finding}")
    expect(
        resolve_label(OCX_CLI_LABEL, mapping) == "ocx",
        "the shipped map lacks the //crates/ocx_cli:ocx_cli -> ocx row",
    )
    expect(
        len(mapping) > CRATE_PACKAGES,
        f"the shipped map has {len(mapping)} rows — too few to be this workspace's",
    )
    print(
        f"map GREEN  : the shipped {LABEL_MAP_PATH.name} has {len(mapping)} rows and resolves "
        "//crates/ocx_cli:ocx_cli to the package `ocx`"
    )
    return 1


def _records(text: str) -> tuple[str, list[str]]:
    """Split `--output=build` text back into its per-rule records.

    The header separator is restored on rejoin. Dropping it turns every
    remaining rule into an orphan, which reds for a reason the caller did not
    mean and is indistinguishable from the mutation it intended.
    """
    blocks = text.split("# /abs/path/")
    return blocks[0], blocks[1:]


def _drop_package(text: str, package: str) -> str:
    """Remove every record of one package, leaving the rest intact."""
    head, blocks = _records(text)
    kept = [block for block in blocks if not block.startswith(f"{package}/")]
    return head + "".join("# /abs/path/" + block for block in kept)


def _drop_in_package(text: str, package: str, needle: str) -> str:
    """Remove `needle` from one package's records only.

    Scoped on purpose: a repo-wide `str.replace` would drop the edge from all
    20 packages, and a finding naming 20 packages proves less than one naming
    the one that was touched.
    """
    head, blocks = _records(text)
    return head + "".join(
        "# /abs/path/" + (block.replace(needle, "") if block.startswith(f"{package}/") else block)
        for block in blocks
    )


def self_test() -> int:
    """C-010 red and green, on fixtures this file builds, with no subprocess."""
    scratch = REPO_ROOT / ".tmp"
    scratch.mkdir(exist_ok=True)
    text, metadata = sample_tree()
    checks = 0
    checks += prove_readers(text, metadata)
    checks += prove_drift_gate(text, metadata)
    checks += prove_floors(text, metadata)
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        checks += prove_map_reader(Path(directory))
    checks += prove_shipped_map()
    print(
        f"bazel build drift self-test: {checks} checks passed — C-010 shown red and green on all "
        "three failure modes (a dropped first-party edge, a dropped @crates// edge, an unmapped "
        "label), on both directions, and on every reader floor"
    )
    print(
        "  wired into taskfiles/scripts.taskfile.yml `self-test:` (WP-17): "
        "`- python3 scripts/bazel_build_drift.py --self-test`"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--self-test", action="store_true", help="prove the gate red and green")
    mode.add_argument(
        "--update",
        action="store_true",
        help="regenerate scripts/bazel_label_map.toml from the same two readings the gate uses",
    )
    parser.add_argument(
        "--bazel",
        default="bazel",
        help="the bazel binary to query (default: resolved from PATH, as the per-prompt hook "
        "and CI's setup-ocx action both leave it)",
    )
    parser.add_argument(
        "--query-output",
        type=Path,
        help="read a captured `bazel query --output=build` instead of running one",
    )
    parser.add_argument(
        "--metadata", type=Path, help="read a captured `cargo metadata` JSON instead of running one"
    )
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if args.update:
        return run_update(
            bazel_bin=args.bazel, query_output=args.query_output, metadata_path=args.metadata
        )
    return report(
        run_check(bazel_bin=args.bazel, query_output=args.query_output, metadata_path=args.metadata)
    )


if __name__ == "__main__":
    raise SystemExit(main())
