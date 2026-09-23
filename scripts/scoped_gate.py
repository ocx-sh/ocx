#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Decide what `task verify:scoped` runs, and write the verify mark it reads back.

    scripts/scoped_gate.py --plan                          # JSON, consumed by the task
    scripts/scoped_gate.py --mark full|scoped [--crates <crate>...]
    scripts/scoped_gate.py --self-test

The gate (plan_crate_split_workspace.md C-019, C-020, C-021):

  1. base = the `head` of the last *full* verify mark when it is an ancestor of
     HEAD, else `git merge-base origin/main HEAD`. Changed paths are
     `git diff --name-only <base>...HEAD` plus the working tree (tracked
     modifications and untracked files).
  2. Non-member paths route. `.agents/**` (swarm memory), `CLAUDE.md` and the
     `.claude/` subtrees
     whose only gate is `claude:tests` (CLAUDE_TESTS_READS, a permit list —
     any other `.claude/` path escalates, `.claude/taskfile.yml` included,
     so a new subtree is loud until someone routes it) → `task
     claude:tests`;
     `.github/**` → actionlint plus `.claude/tests/test_workflows.py`; an
     existing `test/tests/test_<f>.py` → `task test:parallel --
     tests/test_<f>.py`; `scripts/**` → `task scripts:self-test` AND the full
     verify (the gate tooling decides what every other gate runs, so a change
     there is never certified by less than the full run — the route is what
     `--plan` reports, the escalation is what runs it). Every other
     non-member path — root manifests, the lockfile, every taskfile,
     `test/src/**`, `test/conftest.py`, `test/tests/` helpers, `external/**`,
     the nextest floor files under `crates/` — escalates to `task verify` and
     is printed as the reason.
  3. `crates/<dir>/Cargo.toml` and `crates/<dir>/README.md` are workspace
     structure rather than that crate's code, so they route to the
     workspace-structure guards (`cargo nextest run -p ocx_test_support
     --test workspace_structure`) instead of resolving to their own package.
     Every other member path (`crates/<dir>/...`) maps to a package name
     through `cargo metadata`, so `crates/ocx_cli/` is the package `ocx`.
  4. A changed crate escalates when it is a hub: at least HUB_RDEPS reverse
     dependents inside the workspace (counted from the full `cargo metadata`
     resolve graph every run, dev-dependencies included) or a member of
     ECOSYSTEM. A crate whose `test:scoped` row reads `escalate`
     (TABLE_ESCALATES) escalates too, before the per-crate steps are spent.
  5. Otherwise the decision is `scoped` (the task runs the per-crate steps and
     `test:scoped`) or `routed` (nothing under `crates/` changed).

The mark, `.claude/hooks/.state/commit-verified`, is JSON:
`{"timestamp": <epoch>, "head": "<sha>", "scope": "full"|"scoped",
"crates": [...], "toplevel": "<working tree>"}`. A scoped mark also carries
`full_head`, the head of the last full mark, so the base of step 1 survives any
number of scoped runs. `scripts/commit_gate.py` is the reader, run by git
itself through the hooks `task git:hooks` installs; a bare integer (the retired `echo $(date +%s)`
stamp) is not a mark, and neither is one with no `toplevel`. Where the file
lives is `mark_file()`, and it is the reader's `.claude/`, not this script's —
see there for why an agent worktree made C-021 void without anything looking
wrong, and see `worktree_id` for why one shared home needs the writer named.

Stdlib only. `--self-test` drives the path→crate map, the routing table and
the hub predicate over a fixture metadata document, the mark round-trip over a
throwaway repository, `--mark`'s choice of file from a real linked worktree of
a throwaway project, and shows that a failing `cargo metadata` is a loud exit
— never an empty crate set.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path

from _git import git

REPO_ROOT = Path(__file__).resolve().parents[1]


def mark_file() -> Path:
    """Where the verify mark lives — the file the pre-commit hook reads.

    NOT `REPO_ROOT`. The reader builds `.state/` from `CLAUDE_PROJECT_DIR`,
    which Claude Code sets to the session's checkout and keeps there even when
    the cwd is `.agents/worktrees/<slug>`; that shared mark home is deliberate,
    so the reader is right and this writer is the half that drifted. Hanging the
    mark off `REPO_ROOT` put it in whatever worktree the script happens to be
    checked out in, and the failure has the worst possible shape: `--mark`
    PRINTS the JSON it wrote, so the stamp looks like it worked, while the
    gate decides on a file that stamp never touched — a success
    indistinguishable from never having run. C-021's "a mark certifies the
    HEAD it was written on" is then void in both directions: a stale mark
    blocks a verified commit, and a mark left by an unrelated HEAD passes one.

    Deliberately not `git rev-parse --git-common-dir`: this repository's four
    sibling checkouts (`goat`, `evelynn`, `sion`, `soraka`) share one common
    dir and each verifies its own HEAD, so a mark resolved that way would
    make them thrash over a single file. `REPO_ROOT` stays right for what it
    is actually for — locating `scripts/` and the crate map — and
    is the fallback when no harness set `CLAUDE_PROJECT_DIR`, which is the
    shape a plain CLI or CI invocation has.
    """
    project_dir = os.environ.get("CLAUDE_PROJECT_DIR")
    home = Path(project_dir) if project_dir and (Path(project_dir) / ".claude").is_dir() else REPO_ROOT
    return home / ".claude" / "hooks" / ".state" / "commit-verified"

# MACHINE_READ held the one artifact a gate script read as input —
# `edge_inventory.py`'s FILE_MAP — and routed it to the full verify because
# `rust:deps:inventory` was the only consumer. Both left with `ocx_lib` at
# WP-37. Restore the constant, not a hard-coded path, the day another gate
# script reads an artifact: it was imported from its consumer on purpose, so a
# rename there moved the route with it.
# The .claude/ subtrees whose only gate is `claude:tests`: nothing else —
# no cargo target, no task, no gate script — consumes them, so that run
# certifies a change there. A permit list: a path under .claude/ outside it
# escalates. `.claude/scripts/` is out — no gate exercises what it holds,
# and a route to `claude:tests` would certify nothing.
CLAUDE_TESTS_READS = (
    ".claude/agents/",
    ".claude/artifacts/",
    ".claude/hooks/",
    ".claude/rules/",
    ".claude/rules.md",
    ".claude/settings.json",
    ".claude/skills/",
    ".claude/templates/",
    ".claude/tests/",
)
WORKFLOW_TEST = Path(".claude") / "tests" / "test_workflows.py"
# A crate directory's workspace-structure surface: what it declares to the
# workspace rather than what it compiles. `crates/<crate>/Cargo.toml` carries
# the dependency edges C-009 constrains and the feature sets C-019 does;
# `crates/<crate>/README.md` carries the may-depend-on rows C-046 checks.
MANIFEST_FILES = frozenset({"Cargo.toml", "README.md"})

# C-019 (4): the ecosystem tier of the ADR crate map. Any change here is a
# change every satellite links, so the scoped gate never certifies it.
ECOSYSTEM = frozenset(
    {
        "ocx_util",
        "ocx_console",
        "ocx_oci",
        "ocx_trust",
        "ocx_sign",
        "ocx_config",
        "ocx_index",
        "ocx_package",
        "ocx_python",
    }
)
HUB_RDEPS = 4
# Rows of test/taskfile.yml's `test:scoped` table that read `escalate`: their
# acceptance subset is the whole suite, so the gate escalates before spending
# the per-crate steps. `--self-test` checks the two lists agree. `ocx_lib`'s row
# left with the crate at WP-37. `ocx` is here for phase 1 only: the CLI crate still
# holds every command file (crates/ocx_cli/src/command/*), so a change there
# can touch any verb — the row reverts to its own subset when the verbs move out.
# `ocx_python` has no acceptance subset: `ocx` does not link it.
TABLE_ESCALATES = frozenset({"ocx_test_support", "ocx", "ocx_python"})
TEST_TASKFILE = REPO_ROOT / "test" / "taskfile.yml"
ROOT_TASKFILE = REPO_ROOT / "taskfile.yml"


# ---------------------------------------------------------------------------
# Workspace (cargo metadata)
# ---------------------------------------------------------------------------


@dataclass(slots=True)
class Workspace:
    """What the gate needs from `cargo metadata`."""

    crate_of_dir: dict[str, str]  # "crates/<dir>" → package name
    rdeps: dict[str, int]  # package name → reverse dependents among members

    def hubs(self) -> list[str]:
        """Crates the scoped gate never certifies: by reverse-dependent count or tier."""
        return sorted(name for name, n in self.rdeps.items() if n >= HUB_RDEPS or name in ECOSYSTEM)

    def hub_reasons(self, crate: str) -> list[str]:
        reasons = []
        if crate in ECOSYSTEM:
            reasons.append("ecosystem tier")
        if self.rdeps.get(crate, 0) >= HUB_RDEPS:
            reasons.append(f"{self.rdeps[crate]} reverse dependents (hub at {HUB_RDEPS})")
        if crate in TABLE_ESCALATES:
            reasons.append("test:scoped row reads escalate")
        return reasons


def cargo_metadata(root: Path) -> dict:
    """The full `cargo metadata --locked` document, or a loud SystemExit."""
    try:
        result = subprocess.run(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--manifest-path",
                str(root / "Cargo.toml"),
            ],
            cwd=root,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
    except OSError as err:
        raise SystemExit(f"scoped gate: cargo metadata failed to start: {err}") from err
    if result.returncode != 0:
        raise SystemExit(
            f"scoped gate: cargo metadata failed (rc={result.returncode}) — the crate set is"
            f" unknown, not empty:\n{result.stderr}"
        )
    return json.loads(result.stdout)


def parse_workspace(meta: dict) -> Workspace:
    """Member dirs → names, and reverse-dependent counts among members.

    Counts come from the resolve graph, so every dependency kind (normal,
    build, dev) counts once per dependent — a shared test-support crate is a
    hub as soon as four members' test suites lean on it.
    """
    root = Path(meta["workspace_root"])
    members = set(meta["workspace_members"])
    name_of = {pkg["id"]: pkg["name"] for pkg in meta["packages"]}
    crate_of_dir = {
        Path(pkg["manifest_path"]).parent.relative_to(root).as_posix(): pkg["name"]
        for pkg in meta["packages"]
        if pkg["id"] in members
    }
    rdeps = {name_of[pkg_id]: 0 for pkg_id in members}
    for node in meta["resolve"]["nodes"]:
        if node["id"] not in members:
            continue
        for dep in node["deps"]:
            dep_name = name_of.get(dep["pkg"])
            if dep_name in rdeps:
                rdeps[dep_name] += 1
    return Workspace(crate_of_dir=crate_of_dir, rdeps=rdeps)


# ---------------------------------------------------------------------------
# Git plumbing
# ---------------------------------------------------------------------------


def is_ancestor(root: Path, sha: str) -> bool:
    """rc 1 is the answer "no", not a failure — hence not `_git.git`."""
    result = subprocess.run(
        ["git", "-C", str(root), "merge-base", "--is-ancestor", sha, "HEAD"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    return result.returncode == 0


# ---------------------------------------------------------------------------
# The verify mark
# ---------------------------------------------------------------------------


def read_mark(mark_file: Path) -> dict:
    """The mark as a dict; `{}` when missing, unparseable or not an object."""
    try:
        mark = json.loads(mark_file.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return mark if isinstance(mark, dict) else {}


def full_head_of(mark: dict) -> str | None:
    """The head of the last full mark this mark remembers, if any."""
    if mark.get("scope") == "full":
        return mark.get("head")
    return mark.get("full_head")


def worktree_id(repo: Path) -> str:
    """The realpath'd working tree of ``repo`` — the identity a mark is bound to.

    The mark home is the *project dir*, shared by every linked worktree under
    `.agents/worktrees/<slug>`, so a batch that branches seventeen of them from
    one HEAD has seventeen trees reading and writing one file. HEAD alone then
    stops discriminating and each certifies the others' unverified trees — the
    reader in `commit_gate.py` compares this instead (R17).
    """
    return str(Path(git(repo, "rev-parse", "--show-toplevel").strip()).resolve())


def write_mark(
    mark_file: Path, scope: str, crates: list[str], head: str, toplevel: str
) -> dict:
    """Write the mark; a scoped one carries the previous full head forward."""
    mark = {
        "timestamp": int(time.time()),
        "head": head,
        "scope": scope,
        "crates": sorted(set(crates)),
        "toplevel": toplevel,
    }
    if scope != "full":
        full_head = full_head_of(read_mark(mark_file))
        if full_head:
            mark["full_head"] = full_head
    mark_file.parent.mkdir(parents=True, exist_ok=True)
    mark_file.write_text(json.dumps(mark, sort_keys=True) + "\n", encoding="utf-8")
    return mark


def resolve_base(root: Path, mark_file: Path) -> tuple[str, str]:
    """(base sha, source) — source is "full-mark" or "merge-base(origin/main)"."""
    full_head = full_head_of(read_mark(mark_file))
    if full_head and is_ancestor(root, full_head):
        return full_head, "full-mark"
    return git(root, "merge-base", "origin/main", "HEAD").strip(), "merge-base(origin/main)"


# ---------------------------------------------------------------------------
# Changed paths and their classification
# ---------------------------------------------------------------------------


def changed_paths(root: Path, base: str) -> list[str]:
    """Paths changed between base and HEAD, plus the working tree (tracked and untracked)."""
    listed = (
        git(root, "diff", "--name-only", f"{base}...HEAD")
        + git(root, "diff", "--name-only", "HEAD")
        + git(root, "ls-files", "--others", "--exclude-standard")
    )
    return sorted({line for line in listed.splitlines() if line})


@dataclass(slots=True)
class Plan:
    base: str
    base_source: str
    changed: list[str]
    crates: list[str] = field(default_factory=list)
    hubs: list[str] = field(default_factory=list)
    routes: dict[str, list[str]] = field(
        default_factory=lambda: {"claude": [], "workflows": [], "tests": [], "scripts": [], "manifests": []}
    )
    workflow_test_cmd: str = ""
    escalate: list[str] = field(default_factory=list)
    decision: str = ""

    def as_json(self) -> str:
        return json.dumps(asdict(self), indent=2, sort_keys=True, ensure_ascii=False)


def classify(plan: Plan, ws: Workspace, root: Path) -> Plan:
    """Fill crates, hubs, routes, escalate and decision from plan.changed."""
    crates: set[str] = set()
    for path in plan.changed:
        parts = path.split("/")
        if path == "CLAUDE.md" or parts[0] == ".agents" or path.startswith(CLAUDE_TESTS_READS):
            # `.agents/**` is swarm memory (hex.md, discussions): AI config
            # like `.claude/**`, and `claude:tests` is the cheapest gate
            # that reads it — an escalation to the full run bought nothing.
            plan.routes["claude"].append(path)
        elif parts[0] == ".github":
            plan.routes["workflows"].append(path)
        elif parts[0] == "scripts":
            # Gate tooling: its self-tests run (`task scripts:self-test`), and
            # the change still escalates — these scripts decide what every
            # other gate runs, so nothing less than the full verify (whose
            # `.verify:lint` runs `scripts:verify`) certifies an edit here.
            plan.routes["scripts"].append(path)
            plan.escalate.append(f"{path}: gate tooling — self-tests routed, full verify still required")
        elif parts[0] == "crates" and len(parts) == 3 and parts[2] in MANIFEST_FILES:
            # C-019 (2): a crate's manifest and README are workspace
            # structure, not that crate's code. Every guard over them —
            # `deps_direction` (C-009), `crate_map_toml_matches_rust_table`
            # (C-046), the internal-crates block, the may-depend-on rows,
            # `release_feature_set_excludes_testing_seams` — lives in one
            # test target, and resolving the path to its *own* package and
            # running `nextest -p <crate>` runs none of them: today's 17
            # shells have no reverse dependents, so a disallowed edge added
            # to one would pass a scoped gate green.
            #
            # The guards are not the whole route (D40). They compile
            # `ocx_test_support` and nothing else, while a manifest decides
            # what the WORKSPACE compiles — `[features]`, a dependency edge,
            # `default-features` — and `Cargo.lock` carries no feature data,
            # so such an edit never drags the lock in and never escalates.
            # `verify:scoped` therefore runs `cargo check --workspace
            # --all-targets --locked` on this route as well, and
            # `the_manifest_route_reaches_a_workspace_compile` pins that step.
            #
            # That target used to be a test of package `ocx`, which is why
            # this arm escalated to the full verify — reaching it meant
            # building the whole CLI. D34 moved it to
            # `crates/ocx_test_support/tests/workspace_structure.rs`, whose
            # only dependencies are syn/proc-macro2/quote plus four dev
            # crates, so `verify:scoped` can run it directly (measured: 23
            # tests, 6.3 s) and the route is now what certifies the path.
            # The command lives in that step, beside `ci:actionlint`, not in
            # a plan field — a field nothing consumes is dead state, and a
            # self-test comparing it to the constant it was assigned from is
            # a green that cannot tell itself from never having run.
            plan.routes["manifests"].append(path)
        elif parts[0] == "crates" and len(parts) > 2 and f"crates/{parts[1]}" in ws.crate_of_dir:
            crates.add(ws.crate_of_dir[f"crates/{parts[1]}"])
        elif (
            len(parts) == 3
            and parts[:2] == ["test", "tests"]
            and parts[2].startswith("test_")
            and parts[2].endswith(".py")
        ):
            if (root / path).is_file():
                plan.routes["tests"].append("/".join(parts[1:]))
            else:
                plan.escalate.append(f"{path}: deleted acceptance file — full verify")
        else:
            plan.escalate.append(f"{path}: no route — full verify")
    plan.crates = sorted(crates)
    plan.hubs = ws.hubs()
    for crate in plan.crates:
        for reason in ws.hub_reasons(crate):
            plan.escalate.append(f"{crate}: {reason} — full verify")
    if plan.routes["workflows"]:
        plan.workflow_test_cmd = (
            f"uv run --directory {WORKFLOW_TEST.parent.as_posix()} pytest {WORKFLOW_TEST.name} -q"
        )
    if plan.escalate:
        plan.decision = "escalate"
    elif plan.crates:
        plan.decision = "scoped"
    else:
        plan.decision = "routed"
    return plan


def make_plan(root: Path, mark_file: Path) -> Plan:
    ws = parse_workspace(cargo_metadata(root))
    base, source = resolve_base(root, mark_file)
    plan = Plan(base=base, base_source=source, changed=changed_paths(root, base))
    return classify(plan, ws, root)


# ---------------------------------------------------------------------------
# --self-test
# ---------------------------------------------------------------------------


def _fixture_metadata(root: Path) -> dict:
    """A `cargo metadata` document in cargo's shape, small enough to reason about.

    `ocx_store` has five reverse dependents (a hub by count, not by tier);
    `ocx_exit` has exactly HUB_RDEPS (the boundary: `>=` is the rule, and a
    `>` would let it through); `ocx_oci` has three (a hub by tier only);
    `ocx_shell` has two; `serde` is the external crate everything depends on
    and must never surface.
    """
    dirs = {
        "ocx_exit": "ocx_exit",
        "ocx": "ocx_cli",
        "ocx_schema": "ocx_schema",
        "ocx_setup": "ocx_setup",
        "ocx_oci": "ocx_oci",
        "ocx_util": "ocx_util",
        "ocx_store": "ocx_store",
        "ocx_index": "ocx_index",
        "ocx_package": "ocx_package",
        "ocx_shell": "ocx_shell",
        "ocx_project": "ocx_project",
        "ocx_test_support": "ocx_test_support",
    }
    deps = {
        "ocx": ["ocx_exit"],
        "ocx_schema": ["ocx_package"],
        "ocx_oci": ["ocx_util"],
        "ocx_store": ["ocx_oci", "ocx_util"],
        "ocx_index": ["ocx_store", "ocx_oci", "ocx_util"],
        "ocx_package": ["ocx_store", "ocx_index", "ocx_oci", "ocx_util"],
        "ocx_shell": ["ocx_store", "ocx_package", "ocx_util", "ocx_exit"],
        "ocx_project": ["ocx_store", "ocx_shell", "ocx_package", "ocx_util", "ocx_exit"],
        "ocx_setup": ["ocx_shell", "ocx_store", "ocx_util", "ocx_test_support", "ocx_exit"],
    }

    def pkg_id(name: str) -> str:
        return f"path+file://{root}/crates/{dirs.get(name, name)}#{name}@0.6.2"

    packages = [
        {
            "name": name,
            "id": pkg_id(name),
            "manifest_path": str(root / "crates" / d / "Cargo.toml"),
        }
        for name, d in dirs.items()
    ]
    packages.append(
        {"name": "serde", "id": "registry+serde@1.0.0", "manifest_path": "/reg/serde/Cargo.toml"}
    )
    nodes = [
        {
            "id": pkg_id(name),
            "deps": [{"name": d, "pkg": pkg_id(d)} for d in deps.get(name, [])]
            + [{"name": "serde", "pkg": "registry+serde@1.0.0"}],
        }
        for name in dirs
    ]
    nodes.append({"id": "registry+serde@1.0.0", "deps": []})
    return {
        "packages": packages,
        "workspace_members": [pkg_id(name) for name in dirs],
        "workspace_root": str(root),
        "resolve": {"nodes": nodes},
    }


def _plan_for(paths: list[str], ws: Workspace, root: Path) -> Plan:
    return classify(Plan(base="base", base_source="fixture", changed=sorted(paths)), ws, root)


def _self_test_cases(tmp: Path) -> list[tuple[str, object]]:
    """(name, thunk) pairs; a thunk returns None when the rule holds, else the problem."""
    root = tmp / "repo"
    (root / "test" / "tests").mkdir(parents=True)
    (root / "test" / "tests" / "test_install.py").write_text("def test_x(): pass\n", encoding="utf-8")
    ws = parse_workspace(_fixture_metadata(root))

    def expect(cond: bool, problem: str) -> str | None:
        return None if cond else problem

    def crate_map() -> str | None:
        return expect(
            ws.crate_of_dir.get("crates/ocx_cli") == "ocx"
            and ws.crate_of_dir.get("crates/ocx_setup") == "ocx_setup"
            and len(ws.crate_of_dir) == 12
            and "serde" not in ws.rdeps,
            f"crate map wrong: {ws.crate_of_dir}",
        )

    def rdeps() -> str | None:
        return expect(
            ws.rdeps["ocx_store"] == 5
            and ws.rdeps["ocx_exit"] == HUB_RDEPS
            and ws.rdeps["ocx_shell"] == 2
            and ws.rdeps["ocx_setup"] == 0,
            f"reverse-dependent counts wrong: {ws.rdeps}",
        )

    def hubs() -> str | None:
        got = set(ws.hubs())
        return expect(
            "ocx_store" in got
            and "ocx_exit" in got
            and "ocx_oci" in got
            and "ocx_util" in got
            and "ocx_shell" not in got
            and "ocx_setup" not in got,
            f"hub set wrong: {sorted(got)}",
        )

    def routes() -> str | None:
        plan = _plan_for(
            [
                ".agents/memory/hex.md",
                ".claude/rules/x.md",
                "CLAUDE.md",
                ".github/workflows/ci.yml",
                "test/tests/test_install.py",
            ],
            ws,
            root,
        )
        return expect(
            plan.routes["claude"] == [".agents/memory/hex.md", ".claude/rules/x.md", "CLAUDE.md"]
            and plan.routes["workflows"] == [".github/workflows/ci.yml"]
            and plan.routes["tests"] == ["tests/test_install.py"]
            and plan.routes["scripts"] == []
            and plan.workflow_test_cmd == "uv run --directory .claude/tests pytest test_workflows.py -q"
            and plan.escalate == []
            and plan.decision == "routed",
            f"routing wrong: {plan.as_json()}",
        )

    def nested_taskfile_escalates() -> str | None:
        # `.claude/taskfile.yml` is not in the permit list, so it escalates
        # like every other taskfile instead of riding the `.claude/` route.
        plan = _plan_for([".claude/taskfile.yml", "website/sbom.taskfile.yml", ".claude/rules/x.md"], ws, root)
        return expect(
            plan.decision == "escalate"
            and plan.routes["claude"] == [".claude/rules/x.md"]
            and [r.split(":", 1)[0] for r in plan.escalate] == [".claude/taskfile.yml", "website/sbom.taskfile.yml"],
            f"every taskfile must escalate by name: {plan.as_json()}",
        )

    def unlisted_claude_subtree_escalates() -> str | None:
        # The permit list, not a forbid list: a .claude/ path nothing tests is
        # loud, and every listed subtree still routes.
        plan = _plan_for([".claude/scripts/review_surface.py"], ws, root)
        listed = _plan_for([f"{p}x.md" if p.endswith("/") else p for p in CLAUDE_TESTS_READS], ws, root)
        return expect(
            plan.decision == "escalate"
            and plan.routes["claude"] == []
            and listed.decision == "routed"
            and len(listed.routes["claude"]) == len(CLAUDE_TESTS_READS),
            f"unlisted .claude/ path must escalate, listed ones route: {plan.as_json()} {listed.as_json()}",
        )

    def scripts_route_and_escalate() -> str | None:
        plan = _plan_for(["scripts/scoped_gate.py"], ws, root)
        return expect(
            plan.routes["scripts"] == ["scripts/scoped_gate.py"]
            and plan.decision == "escalate"
            and any(r.startswith("scripts/scoped_gate.py:") and "self-tests routed" in r for r in plan.escalate),
            f"scripts/** must be routed to the self-tests AND escalate: {plan.as_json()}",
        )

    def manifest_routes_without_escalating() -> str | None:
        # `ocx_setup` is the fixture's zero-reverse-dependent shell, the shape
        # all 17 shells have today: before C-019 (2) named the manifest, this
        # resolved to the package and decided `scoped`, and the scoped tier's
        # `nextest -p ocx_setup` runs none of the workspace guards. Since D34
        # the guards are a test target of `ocx_test_support`, so the route
        # certifies the path on its own and the full verify is not spent.
        for path in ("crates/ocx_setup/Cargo.toml", "crates/ocx_setup/README.md"):
            plan = _plan_for([path], ws, root)
            problem = expect(
                plan.routes["manifests"] == [path]
                and plan.crates == []
                and plan.decision == "routed"
                and plan.escalate == [],
                f"{path} must route to the workspace-structure guards and not escalate: {plan.as_json()}",
            )
            if problem:
                return problem
        return None

    def crate_source_is_still_scoped_beside_a_manifest() -> str | None:
        # The manifest arm must not swallow the crate's own code: a source
        # edit still scopes with no manifest route, and a mixed change set
        # carries both — the crate's per-crate steps and the guards.
        source = _plan_for(["crates/ocx_setup/src/lib.rs"], ws, root)
        mixed = _plan_for(["crates/ocx_setup/src/lib.rs", "crates/ocx_setup/Cargo.toml"], ws, root)
        return expect(
            source.decision == "scoped"
            and source.routes["manifests"] == []
            and mixed.decision == "scoped"
            and mixed.crates == ["ocx_setup"]
            and mixed.routes["manifests"] == ["crates/ocx_setup/Cargo.toml"],
            f"a source edit must stay scoped and a mixed set carry both: {source.as_json()} {mixed.as_json()}",
        )

    def a_manifest_beside_an_escalating_path_does_not_run_the_guards_twice() -> str | None:
        # The taskfile step is guarded by `decision != escalate`, so the route
        # may stay populated on an escalate — the full verify runs the guards.
        # What must never happen is the escalation being *caused* by the
        # manifest: the reason names the other path and only the other path.
        plan = _plan_for(["crates/ocx_setup/Cargo.toml", "Cargo.lock"], ws, root)
        return expect(
            plan.decision == "escalate"
            and plan.routes["manifests"] == ["crates/ocx_setup/Cargo.toml"]
            and [reason.split(":", 1)[0] for reason in plan.escalate] == ["Cargo.lock"],
            f"only the unrouted path may escalate a manifest-plus-lockfile set: {plan.as_json()}",
        )

    def unrouted_paths_escalate() -> str | None:
        paths = [
            "test/tests/test_missing.py",
            "test/tests/conftest.py",
            "test/tests/fake_forge.py",
            "test/conftest.py",
            "test/src/x.py",
            "taskfile.yml",
            "Cargo.lock",
            "external/rust-oci-client/src/lib.rs",
            "crates/NEXTEST_FLOOR",
        ]
        plan = _plan_for(paths, ws, root)
        named = [reason.split(":", 1)[0] for reason in plan.escalate]
        return expect(
            plan.decision == "escalate" and named == sorted(paths) and plan.crates == [],
            f"unrouted paths must each escalate by name: {plan.as_json()}",
        )

    def non_hub_crate_is_scoped() -> str | None:
        plan = _plan_for(["crates/ocx_setup/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "scoped" and plan.crates == ["ocx_setup"] and plan.escalate == [],
            f"ocx_setup must be scoped: {plan.as_json()}",
        )

    def ecosystem_crate_escalates() -> str | None:
        plan = _plan_for(["crates/ocx_oci/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate" and any("ecosystem" in r for r in plan.escalate),
            f"ocx_oci must escalate as ecosystem tier: {plan.as_json()}",
        )

    def hub_by_count_escalates() -> str | None:
        plan = _plan_for(["crates/ocx_store/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate" and any("5 reverse dependents" in r for r in plan.escalate),
            f"ocx_store must escalate by reverse-dependent count: {plan.as_json()}",
        )

    def hub_at_the_boundary_escalates() -> str | None:
        # Exactly HUB_RDEPS dependents is a hub: the one fixture that tells
        # `>=` from `>`.
        plan = _plan_for(["crates/ocx_exit/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate"
            and any(f"{HUB_RDEPS} reverse dependents (hub at {HUB_RDEPS})" in r for r in plan.escalate),
            f"ocx_exit ({HUB_RDEPS} dependents) must escalate as a hub: {plan.as_json()}",
        )

    def table_escalate_row_escalates() -> str | None:
        # Was `ocx_lib`, whose row left with the crate at WP-37.
        plan = _plan_for(["crates/ocx_test_support/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate"
            and any("escalate" in r and "ocx_test_support" in r for r in plan.escalate),
            f"ocx_test_support must escalate (test:scoped row): {plan.as_json()}",
        )

    def cli_crate_escalates_in_phase_1() -> str | None:
        # A command file in the CLI crate can touch any verb until they move out.
        plan = _plan_for(["crates/ocx_cli/src/command/package_push.rs"], ws, root)
        return expect(
            plan.decision == "escalate"
            and plan.crates == ["ocx"]
            and any(r.startswith("ocx:") and "escalate" in r for r in plan.escalate),
            f"ocx must escalate (test:scoped row, phase 1): {plan.as_json()}",
        )

    def crates_and_routes_mix() -> str | None:
        plan = _plan_for(["crates/ocx_setup/src/lib.rs", ".claude/rules/x.md"], ws, root)
        return expect(
            plan.decision == "scoped" and plan.routes["claude"] == [".claude/rules/x.md"],
            f"a crate plus a routed path is scoped with the route kept: {plan.as_json()}",
        )

    def nothing_changed_is_routed_with_nothing() -> str | None:
        plan = _plan_for([], ws, root)
        return expect(
            plan.decision == "routed" and not any(plan.routes.values()) and plan.crates == [],
            f"an empty change set is routed with no routes: {plan.as_json()}",
        )

    def escalation_keeps_crates_informational() -> str | None:
        plan = _plan_for(["taskfile.yml", "crates/ocx_setup/src/lib.rs"], ws, root)
        return expect(
            plan.decision == "escalate" and plan.crates == ["ocx_setup"],
            f"escalation still lists the changed crates: {plan.as_json()}",
        )

    def mark_round_trip() -> str | None:
        repo = tmp / "marks"
        repo.mkdir()
        git(repo, "init", "-q", "-b", "work")
        (repo / "a").write_text("a\n", encoding="utf-8")
        git(repo, "add", "a")
        git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "A")
        sha_a = git(repo, "rev-parse", "HEAD").strip()
        mark = repo / "mark.json"
        if read_mark(mark) != {}:
            return "a missing mark must read as {}"
        mark.write_text("1758000000\n", encoding="utf-8")
        if read_mark(mark) != {}:
            return "a bare-integer stamp must read as {}"
        mark.write_text("not json", encoding="utf-8")
        if read_mark(mark) != {}:
            return "garbage must read as {}"
        top = worktree_id(repo)
        if top != str(repo.resolve()):
            return f"the writer identity must be the realpath'd working tree: {top}"
        full = write_mark(mark, "full", [], sha_a, top)
        if full.get("scope") != "full" or full.get("head") != sha_a or "full_head" in full:
            return f"full mark wrong: {full}"
        if not isinstance(full.get("timestamp"), int) or full.get("crates") != []:
            return f"full mark fields wrong: {full}"
        # R17: without this the mark says nothing about WHICH tree earned it,
        # and a sibling worktree at the same HEAD reads it as its own.
        if full.get("toplevel") != top:
            return f"the mark must record the tree that wrote it: {full}"
        if full_head_of(read_mark(mark)) != sha_a:
            return "full_head_of(full mark) must be its head"
        (repo / "b").write_text("b\n", encoding="utf-8")
        git(repo, "add", "b")
        git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "B")
        sha_b = git(repo, "rev-parse", "HEAD").strip()
        scoped = write_mark(mark, "scoped", ["ocx_setup"], sha_b, top)
        if scoped.get("full_head") != sha_a or scoped.get("crates") != ["ocx_setup"]:
            return f"scoped mark must carry the full head forward: {scoped}"
        if scoped.get("toplevel") != top:
            return f"a scoped mark must record its tree too: {scoped}"
        scoped = write_mark(mark, "scoped", ["ocx_shell"], sha_b, top)
        if scoped.get("full_head") != sha_a:
            return f"a second scoped mark must keep carrying the full head: {scoped}"
        if resolve_base(repo, mark) != (sha_a, "full-mark"):
            return f"base must be the full mark's head: {resolve_base(repo, mark)}"
        git(repo, "update-ref", "refs/remotes/origin/main", sha_a)
        mark.write_text(json.dumps({**scoped, "full_head": "0" * 40}), encoding="utf-8")
        if resolve_base(repo, mark) != (sha_a, "merge-base(origin/main)"):
            return f"an unreachable full head must fall back to the merge base: {resolve_base(repo, mark)}"
        mark.unlink()
        if resolve_base(repo, mark) != (sha_a, "merge-base(origin/main)"):
            return "no mark must fall back to the merge base"
        (repo / "a").write_text("a2\n", encoding="utf-8")
        (repo / "c").write_text("c\n", encoding="utf-8")
        changed = changed_paths(repo, sha_a)
        return expect(
            changed == ["a", "b", "c"], f"changed paths must span commits and the tree: {changed}"
        )

    def mark_home_follows_the_project_dir() -> str | None:
        """The mark lands where the hook reads it, not where the script sits.

        Driven end to end, because the defect is invisible in process: a
        stamp from a worktree prints the JSON it wrote and looks like it
        worked while the gate reads a different file. So this builds a
        throwaway project, adds a real linked worktree of it, runs the
        script *from the worktree* and asserts on the two paths.
        """
        proj = tmp / "markhome"
        (proj / "scripts").mkdir(parents=True)
        (proj / ".claude").mkdir()
        (proj / ".claude" / ".keep").write_text("", encoding="utf-8")
        for name in ("scoped_gate.py", "_git.py"):
            (proj / "scripts" / name).write_bytes((REPO_ROOT / "scripts" / name).read_bytes())
        git(proj, "init", "-q", "-b", "work")
        git(proj, "add", "-A")
        git(proj, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "A")
        wt = tmp / "markhome-wt"
        git(proj, "worktree", "add", "-q", str(wt), "-b", "side")
        script = wt / "scripts" / "scoped_gate.py"
        at_project = proj / ".claude" / "hooks" / ".state" / "commit-verified"
        at_worktree = wt / ".claude" / "hooks" / ".state" / "commit-verified"

        def stamp(project_dir: str | None) -> subprocess.CompletedProcess[str]:
            env = {k: v for k, v in os.environ.items() if k != "CLAUDE_PROJECT_DIR"}
            if project_dir is not None:
                env["CLAUDE_PROJECT_DIR"] = project_dir
            return subprocess.run(
                [sys.executable, str(script), "--mark", "scoped"],
                capture_output=True, text=True, env=env, cwd=str(wt), check=False,
            )

        # (a) CLAUDE_PROJECT_DIR set, cwd inside a linked worktree of it.
        run = stamp(str(proj))
        if run.returncode != 0:
            return f"--mark failed under a project dir: {run.stderr.strip()}"
        if not at_project.is_file():
            return f"the mark must land at the project dir, not at {list(wt.rglob('commit-verified'))}"
        if at_worktree.exists():
            return "the mark must NOT land in the worktree when a project dir is set"
        # (b) the reader's contract: the same shape, this worktree's HEAD.
        mark = json.loads(at_project.read_text(encoding="utf-8"))
        head = git(wt, "rev-parse", "HEAD").strip()
        if set(mark) != {"timestamp", "head", "scope", "crates", "toplevel"}:
            return f"mark key set drifted from the reader's contract: {sorted(mark)}"
        if mark["head"] != head or mark["scope"] != "scoped" or not isinstance(mark["timestamp"], int):
            return f"mark must certify the worktree's HEAD {head}: {mark}"
        # The mark HOME is the project dir (DX-52) but the writer identity is
        # the tree that ran: shared file, one tree named in it.
        if mark["toplevel"] != str(wt.resolve()):
            return f"mark must name the worktree that wrote it, not the project dir: {mark}"
        # (c) unset, the CLI and CI shape: the fallback is still REPO_ROOT.
        before = at_project.read_text(encoding="utf-8")
        run = stamp(None)
        if run.returncode != 0:
            return f"--mark failed with no project dir: {run.stderr.strip()}"
        if not at_worktree.is_file():
            return "with CLAUDE_PROJECT_DIR unset the mark must fall back to REPO_ROOT"
        if at_project.read_text(encoding="utf-8") != before:
            return "the fallback must not touch the project dir's mark"
        return None

    def metadata_failure_is_loud() -> str | None:
        bare = tmp / "bare"
        bare.mkdir()
        try:
            cargo_metadata(bare)
        except SystemExit as stop:
            return expect("cargo metadata failed" in str(stop), f"wrong failure text: {stop}")
        return "cargo metadata on a directory without a manifest must raise SystemExit"

    def the_manifest_route_reaches_a_workspace_compile() -> str | None:
        """D40: the route's steps must compile what a manifest decides.

        `classify` sends a `crates/<c>/Cargo.toml` edit to `routed`, and the
        guards target that route runs compiles `ocx_test_support` alone. Since
        `Cargo.lock` carries `name`/`version`/`source`/`checksum`/`dependencies`
        and no feature data, a `[features]` edit never drags the lock into the
        change set and never escalates — so the workspace compile is the only
        step that can go red on it, and what this pins is the condition that
        reaches it. The red/green of the step itself is the gate run.
        """
        text = ROOT_TASKFILE.read_text(encoding="utf-8")
        steps = re.findall(r"- if: '([^']*)'\n\s+cmd: (cargo [^\n]+)", text)
        compile_conditions = [
            condition for condition, cmd in steps if cmd.startswith("cargo check --workspace")
        ]
        guard_conditions = [
            condition for condition, cmd in steps if "--test workspace_structure" in cmd
        ]
        if len(compile_conditions) != 1 or len(guard_conditions) != 1:
            return (
                "verify:scoped must run exactly one `cargo check --workspace` and one guards"
                f" step: {compile_conditions} {guard_conditions}"
            )
        if "ROUTE_MANIFESTS" not in guard_conditions[0]:
            return f"the guards step must be the manifest route's: {guard_conditions[0]}"
        return expect(
            "ROUTE_MANIFESTS" in compile_conditions[0],
            f"a manifest route that compiles nothing certifies nothing (D40): {compile_conditions[0]}",
        )

    def table_rows_agree() -> str | None:
        rows = set(re.findall(r"^\s+(\w+): escalate\s*$", TEST_TASKFILE.read_text(encoding="utf-8"), re.MULTILINE))
        return expect(
            rows == set(TABLE_ESCALATES),
            f"test/taskfile.yml escalate rows {sorted(rows)} != TABLE_ESCALATES {sorted(TABLE_ESCALATES)}",
        )

    return [
        ("path→crate map", crate_map),
        ("reverse-dependent counts", rdeps),
        ("hub predicate", hubs),
        ("routing table", routes),
        ("nested taskfile escalates", nested_taskfile_escalates),
        ("unlisted .claude/ subtree escalates", unlisted_claude_subtree_escalates),
        ("scripts/** routes to the self-tests and escalates", scripts_route_and_escalate),
        ("crate manifest/README routes to the workspace guards", manifest_routes_without_escalating),
        ("crate source stays scoped beside a manifest", crate_source_is_still_scoped_beside_a_manifest),
        ("a manifest never causes the escalation", a_manifest_beside_an_escalating_path_does_not_run_the_guards_twice),
        ("unrouted paths escalate by name", unrouted_paths_escalate),
        ("non-hub crate is scoped", non_hub_crate_is_scoped),
        ("ecosystem crate escalates", ecosystem_crate_escalates),
        ("hub by count escalates", hub_by_count_escalates),
        ("hub at the boundary escalates", hub_at_the_boundary_escalates),
        ("test:scoped escalate row escalates", table_escalate_row_escalates),
        ("CLI crate escalates in phase 1", cli_crate_escalates_in_phase_1),
        ("crate plus route stays scoped", crates_and_routes_mix),
        ("empty change set is routed", nothing_changed_is_routed_with_nothing),
        ("escalation keeps crates informational", escalation_keeps_crates_informational),
        ("mark round trip and base selection", mark_round_trip),
        ("the verify mark lands where the hook reads it", mark_home_follows_the_project_dir),
        ("cargo metadata failure is loud", metadata_failure_is_loud),
        ("the manifest route reaches a workspace compile", the_manifest_route_reaches_a_workspace_compile),
        ("TABLE_ESCALATES matches the taskfile", table_rows_agree),
    ]


def self_test() -> int:
    scratch_root = REPO_ROOT / ".tmp"
    scratch_root.mkdir(exist_ok=True)
    failures = 0
    with tempfile.TemporaryDirectory(prefix="scoped-gate-", dir=scratch_root) as tmp:
        cases = _self_test_cases(Path(tmp))
        for name, thunk in cases:
            try:
                problem = thunk()
            except (SystemExit, NotImplementedError) as stop:
                problem = f"{type(stop).__name__}: {stop}"
            status = "ok  " if problem is None else "FAIL"
            failures += problem is not None
            print(f"{status} {name}")
            if problem is not None:
                print(f"       {problem}")
    print(f"self-test: {len(cases) - failures}/{len(cases)} rules hold ({failures} wrong)")
    return 1 if failures else 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--plan", action="store_true", help="print the gate plan as JSON")
    mode.add_argument(
        "--mark", choices=("full", "scoped"), help="write the verify mark at this scope"
    )
    mode.add_argument("--self-test", action="store_true", help="show every rule red/green")
    parser.add_argument("--crates", nargs="*", default=[], help="crates a scoped mark records")
    ns = parser.parse_args(argv)
    if ns.self_test:
        return self_test()
    if ns.mark:
        head = git(REPO_ROOT, "rev-parse", "HEAD").strip()
        mark = write_mark(mark_file(), ns.mark, ns.crates, head, worktree_id(REPO_ROOT))
        print(f"verify mark: {json.dumps(mark, sort_keys=True)}")
        return 0
    print(make_plan(REPO_ROOT, mark_file()).as_json())
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
