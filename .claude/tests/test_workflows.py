"""Structural tests for `.github/workflows/*.yml`.

Two shapes the crate split can break without any workflow going red:

- a `paths:` / `paths-ignore:` entry whose last file moved. The workflow then
  never fires, and a workflow that never fires is a green nobody can tell
  from a pass. Every entry must match at least one tracked file.
- the deep tier's trigger shape, which is what makes it opt-in: the conventions
  template's `verify-deep.yml` runs nightly, by `workflow_dispatch`, from the
  readiness gate (`workflow_call`) and on a pull request only once it carries the
  `run-deep` label. A push to `main` runs the basic tier alone.
- the merge-queue shape of the basic tier — `merge_group` and a `cancel-in-progress`
  that cancels pull-request runs only, so a queue run is never a cancelled check.
- the shell zoo's checkout and its task graph, which are one contract: that
  job omits `submodules:` on purpose, so a task it runs must not reach
  `:website:schema:default` and through it a cargo build of a workspace whose
  `external/` patch sources are not on disk.
- the tier partition: `verify-basic.yml` and `basic-repo.yml` are what a pull
  request pays for: Linux plus two native smoke legs (Windows x64, macOS arm64); the
  full native matrix is `deep-repo.yml`'s. A further native leg in basic is a cost every
  push pays; a leg dropped from either tier is a `cfg(windows)` /
  `cfg(target_os = "macos")` arm nothing compiles.

- the Bazel remote-cache lane policy. Every same-repo
  lane holding the secret reads and writes the shared cache: each `bazel-cache-rc`
  step passes `BAZEL_CACHE_WRITE_AUTH` by name with no event predicate, every Bazel
  lane has such a step before its first lane step and an `if: always()` cleanup
  after, no workflow spells the upload flag, and every caller of a reusable workflow
  passes each secret that workflow declares by name (no `secrets: inherit`). Every one
  of those assertions is structural; none dials the cache.

- the shape of the repo-owned workflows behind the conventions template's `repo`
  jobs (`basic-repo.yml`, `deep-repo.yml`) and the reusable workflows they call
  (`acceptance.yml`, `cross.yml`, `shell-activation-deep.yml`): the jobs each tier
  needs, the `workflow_call` inputs and secrets, and the permission ceiling the
  template's caller imposes.

- The evidence predicate: which `TestResult` counts as a **remote** cache hit.
  Measured, the two BEP fields named for that question answer it wrongly — a
  `--disk_cache` hit reports `cachedRemotely: true` with no remote cache configured
  at all — so the predicate keys on `executionInfo.strategy`, and its red and green
  run over real BEP bytes in `test/fixtures/bep/cache_states.json`.

The workflows are read with PyYAML (D11). One YAML 1.1 trap: the bare key
`on` loads as the boolean `True`, so `_on` looks it up under both spellings.
Glob liveness is `git ls-files -- ':(glob)<pattern>'`: git's `:(glob)`
pathspec is GitHub's filter grammar (`*` stops at `/`, `**` crosses it, a
leading `**/` matches at any depth), so no matcher is written here.

Run:
    uv run --directory .claude/tests pytest test_workflows.py -q
"""

from __future__ import annotations

import copy
import functools
import json
import os
import re
import shlex
import subprocess
import sys
from collections.abc import Iterable, Iterator
from pathlib import Path, PurePosixPath
from urllib.parse import urlsplit

import pytest
import yaml

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = sorted((ROOT / ".github" / "workflows").glob("*.yml"))
VERIFY_DEEP = ROOT / ".github" / "workflows" / "verify-deep.yml"
VERIFY_BASIC = ROOT / ".github" / "workflows" / "verify-basic.yml"
# Runner-label prefixes: `ubuntu-latest`, `macos-14`, `windows-2025` all
# belong to their family, so a version pin never reads as a lost OS.
OS_FAMILIES = ("ubuntu", "macos", "windows")

# The label a pull request carries to start the deep tier. The template's `pull_request` trigger is
# `labeled` only, so a push to a labelled pull request does not re-run it.
RUN_DEEP_LABEL = "run-deep"
# Both checks that read deep's triggers and jobs are over a collection, and a check over a collection
# passes identically when the collection is empty, mis-keyed or read by a loader that returned nothing.
# These sets are the floor: exact rather than a count, so the failure names the drift.
DEEP_TRIGGERS = frozenset({"workflow_dispatch", "workflow_call", "schedule", "pull_request"})
DEEP_JOBS = frozenset({"deep", "repo", "nightly-red"})
# The template's expression cancels a pull-request run and nothing else. A queue run has the event
# `merge_group` and a push to `main` the event `push`, so neither is ever cancelled — a cancelled
# merge-queue run is a failed check and drops the entry from the queue.
CANCEL_ONLY_PULL_REQUESTS = "${{ github.event_name == 'pull_request' }}"


def _load(workflow: Path) -> dict:
    return yaml.safe_load(workflow.read_text(encoding="utf-8"))


def _on(workflow: Path) -> dict | list | str:
    """The `on:` triggers; a missing `on:` raises — it is a shape to report, not "nothing to check"."""
    document = _load(workflow)
    triggers = document.get("on", document.get(True))
    if triggers is None:
        raise KeyError(f"{workflow.name}: no `on:`")
    return triggers


# ---------------------------------------------------------------------------
# C-023: every `paths:` / `paths-ignore:` entry matches a tracked file
# ---------------------------------------------------------------------------


def _path_filters(workflow: Path) -> list[str]:
    """Every `paths:` / `paths-ignore:` entry under `on:`, the `!` of a negation stripped.

    A negated entry is only an exclusion while what it names exists — a
    `!docs/**` over a moved `docs/` is as dead as a positive one.
    """
    triggers = _on(workflow)
    found: list[str] = []
    if isinstance(triggers, dict):
        for event in triggers.values():
            if isinstance(event, dict):
                for key in ("paths", "paths-ignore"):
                    found += event.get(key) or []
    return [str(entry).lstrip("!") for entry in found]


def _matches_a_tracked_file(pattern: str) -> bool:
    listed = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files", "--", f":(glob){pattern}"],
        capture_output=True,
        encoding="utf-8",
        check=True,
    ).stdout
    return bool(listed.strip())


def test_the_filter_reader_finds_a_filter() -> None:
    """The reader's own red state: a tree with path filters where it finds none.

    Without this, a reader that stopped recognising `paths:` would leave every
    per-workflow test below green over an empty list.
    """
    assert any(_path_filters(workflow) for workflow in WORKFLOWS), (
        "no `paths:` / `paths-ignore:` entry found in any workflow — the reader drifted, "
        "or every filter was removed (then delete this test with them)"
    )


@pytest.mark.parametrize("workflow", WORKFLOWS, ids=lambda workflow: workflow.name)
def test_path_filters_match_tracked_files(workflow: Path) -> None:
    dead = [entry for entry in _path_filters(workflow) if not _matches_a_tracked_file(entry)]
    assert not dead, (
        f"{workflow.name}: `paths:` / `paths-ignore:` entries matching no tracked file: {dead}. "
        f"The workflow no longer fires for the files these named — re-point the filter at "
        f"where they moved, or drop the entry."
    )


# ---------------------------------------------------------------------------
# The deep tier is opt-in, and the basic tier survives the merge queue
# ---------------------------------------------------------------------------


def test_deep_runs_on_a_pull_request_only_when_it_is_labelled() -> None:
    """The deep tier is opt-in, not per-PR.

    A deep run is the cross-builds plus macOS and Windows minutes, and every pull
    request used to pay it on every push. The template's one pull-request trigger is
    `labeled`, and the `run-deep` guard on its jobs (the next test) is what keeps any
    other label from starting it. Adding `synchronize` or `opened` is a real decision
    about cost and about what the basic tier is for, so it reds here rather than
    arriving in a diff nobody reads. The coverage this gives up is named in
    subsystem-ci.md.
    """
    on = _on(VERIFY_DEEP)
    assert on.get("pull_request") == {"types": ["labeled"]}, (
        f"verify-deep.yml `on.pull_request` is {on.get('pull_request')!r}, expected only "
        f"`types: [labeled]` — any other type runs the deep tier on every push to every pull request"
    )
    # The floor: the assertion above is a negative on every other trigger, and an `on:` block
    # this reader got back empty or under a key it does not know would satisfy it while proving
    # nothing.
    assert set(on) == DEEP_TRIGGERS, (
        f"verify-deep.yml `on` is {sorted(on)}, expected exactly {sorted(DEEP_TRIGGERS)} — "
        f"a trigger gained or lost changes when the deep tier runs"
    )


def test_deep_is_dispatched_by_hand() -> None:
    """`workflow_dispatch` is how a human runs the deep tier on a branch whose pull
    request is not open yet. Lose it and the tier runs nightly and on a label only."""
    on = _on(VERIFY_DEEP)
    assert "workflow_dispatch" in on, (
        f"verify-deep.yml `on` has {sorted(on)} and no `workflow_dispatch` — the one way to "
        f"run the deep tier on a branch before a pull request carries the label"
    )


def test_deep_runs_nightly_and_for_the_readiness_gate() -> None:
    """A push to `main` runs the basic tier alone, so the nightly schedule is the
    only automatic full run; `workflow_call` stays because the template owns the file."""
    on = _on(VERIFY_DEEP)
    assert "schedule" in on, f"verify-deep.yml `on` has {sorted(on)} and no `schedule` — nothing deep runs unattended"
    assert "workflow_call" in on, (
        f"verify-deep.yml `on` has {sorted(on)} and no `workflow_call` — the template's trigger set changed"
    )


def test_basic_triggers_in_the_merge_queue() -> None:
    """A required check that never runs on the queued merge commit blocks the queue forever,
    and `basic-gate` is the one required check."""
    on = _on(VERIFY_BASIC)
    assert "merge_group" in on, (
        f"verify-basic.yml `on` has {sorted(on)} and no `merge_group` — a required check that "
        f"never runs on the queued merge commit blocks the queue forever"
    )


def test_every_deep_job_starts_only_on_the_label_or_the_schedule() -> None:
    """The other half of the trigger shape, and the half a diff hides.

    A pull request event reaches every job of the workflow, so a job with no guard runs
    for *any* label a maintainer adds. Each job either names the `run-deep` label or is
    the scheduled `nightly-red` reporter.
    """
    jobs = _load(VERIFY_DEEP)["jobs"]
    # The floor, before the negative below: a job set this reader got back empty — or one it
    # read past because the loader returned it under some other shape — sweeps clean and says
    # nothing.
    assert set(jobs) == DEEP_JOBS, (
        f"verify-deep.yml jobs are {sorted(jobs)}, expected exactly {sorted(DEEP_JOBS)} — a "
        f"job added here must be checked for a label guard by the assertion below"
    )
    unguarded = {
        name: job.get("if")
        for name, job in jobs.items()
        if RUN_DEEP_LABEL not in str(job.get("if", "")) and "'schedule'" not in str(job.get("if", ""))
    }
    assert not unguarded, (
        f"verify-deep.yml jobs whose `if:` names neither the `{RUN_DEEP_LABEL}` label nor the "
        f"schedule: {unguarded}. A pull request `labeled` event with any other label would run them"
    )


def test_basic_cancel_in_progress_cancels_pull_requests_only() -> None:
    value = _load(VERIFY_BASIC)["concurrency"]["cancel-in-progress"]
    assert value == CANCEL_ONLY_PULL_REQUESTS, (
        f"verify-basic.yml `concurrency.cancel-in-progress` is `{value}`, expected "
        f"`{CANCEL_ONLY_PULL_REQUESTS}` — a cancelled merge-queue run is a failed check and "
        f"drops the entry from the queue, and a cancelled run on `main` leaves a merge unverified"
    )


def _disarmed(job: dict, step: dict) -> list[str]:
    """Why this carrier's red would not fail its job — empty when it would.

    `continue-on-error: true` turns a red into a green annotation, and any
    `if:` is a condition under which the step never runs at all; both leave
    a step present in the file and absent from the verdict. Four steps of
    `verify-basic.yml` already carry `continue-on-error: true`, so this is
    the spelling nearest to hand for anyone quietening this gate.
    """
    reasons = []
    for owner, node in (("job", job), ("step", step)):
        if node.get("continue-on-error", False) is not False:
            reasons.append(f"{owner} `continue-on-error: {node['continue-on-error']}`")
        if "if" in node:
            reasons.append(f"{owner} `if: {node['if']}`")
    return reasons


# ---------------------------------------------------------------------------
# What a `task <name>` step runs: the template's jobs run `task ci:lint|test|build|docs|deep`,
# and the tasks each tier holds are listed in `taskfiles/ci.taskfile.yml`, so a test asking
# "does CI run X" reads the task graph, not the workflow text.
# ---------------------------------------------------------------------------


@functools.cache
def _task_table() -> dict[str, tuple[dict, str]]:
    """Every task of the root taskfile and its includes, `{qualified name: (body, namespace)}`.

    A task of an included taskfile is qualified by the include's key, so `:rust:build` and
    `rust:build` are one entry. Hidden tasks (`.ensure-cargo-tool`) are in it. An include that
    does not exist raises: a reader that silently skipped it would read a smaller graph.
    """
    table: dict[str, tuple[dict, str]] = {}

    def read(path: Path, namespace: str) -> None:
        document = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
        for name, body in (document.get("tasks") or {}).items():
            table[f"{namespace}{name}"] = (body if isinstance(body, dict) else {}, namespace)
        for key, include in (document.get("includes") or {}).items():
            target = include if isinstance(include, str) else include["taskfile"]
            read((path.parent / target).resolve(), f"{namespace}{key}:")

    read(ROOT / "taskfile.yml", "")
    return table


@functools.cache
def _task_reach(name: str) -> frozenset[str]:
    """Every task `task <name>` can run: itself, its `deps:` and the `task:` entries of its `cmds:`.

    Transitive, and an over-approximation: a `task:` entry guarded by `if:` or `platforms:`
    counts as reached. That is the right side to err on for a reader that names a task the
    tier lists, and the wrong one for "this ran on this runner" — which no static read proves.
    """
    table = _task_table()
    seen: set[str] = set()
    stack = [name]
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        body, namespace = table.get(current, ({}, ""))
        for key in ("deps", "cmds"):
            for entry in body.get(key) or []:
                if isinstance(entry, dict) and "task" in entry:
                    target = str(entry["task"])
                    stack.append(target[1:] if target.startswith(":") else f"{namespace}{target}")
    return frozenset(seen)


def _step_reaches(step: dict, task: str) -> bool:
    """Whether the step's `run:` is `task <alias-or-task>` and `<task>` is among what that reaches."""
    found = re.match(r"task ([\w:.-]+)(?![\w:-])", str(step.get("run", "")).strip())
    return bool(found) and task in _task_reach(found.group(1))


@pytest.mark.parametrize(
    ("alias", "task", "reached"),
    [
        pytest.param("ci:lint", "claude:tests", True, id="lint-reaches-the-claude-tests"),
        pytest.param("ci:lint", "bazel:build:drift", True, id="lint-reaches-a-bazel-gate"),
        pytest.param("ci:build", "rust:build", True, id="build-reaches-the-build"),
        pytest.param("ci:build", "claude:tests", False, id="build-does-not-reach-the-claude-tests"),
        pytest.param("ci:docs", "bazel:test:unit", False, id="docs-does-not-reach-the-unit-tests"),
    ],
)
def test_the_task_reader_reaches_through_an_alias_and_stops_at_its_edge(alias: str, task: str, reached: bool) -> None:
    """The reader's own red and green: a graph read as empty reaches nothing, one read as
    everything reaches everything, and each answers the sweeps below the same way."""
    assert (task in _task_reach(alias)) is reached, f"`task {alias}` reaches {sorted(_task_reach(alias))}"


@pytest.mark.parametrize(("caller", "phase"), [("ci:lint", "ci:.lint:phase1"), ("verify", ".verify:lint")])
def test_the_toolchain_installs_before_a_parallel_phase_runs_cargo(caller: str, phase: str) -> None:
    """A parallel phase runs `cargo fmt` beside tests whose `task --dry` evaluates vars that call
    `rustc`; on a cold RUSTUP_HOME each proxy installs the toolchain and two at once collide.
    `rust:toolchain` must run, serially, in the caller's `cmds:` before the phase."""
    body, namespace = _task_table()[caller]
    names = [
        str(entry["task"])[1:] if str(entry["task"]).startswith(":") else f"{namespace}{entry['task']}"
        for entry in body.get("cmds") or []
        if isinstance(entry, dict) and "task" in entry
    ]
    assert "rust:format:check" in _task_reach(phase), f"{phase} no longer runs cargo; drop this case"
    assert "rust:toolchain" in names and names.index("rust:toolchain") < names.index(phase), names


def test_the_claude_structural_tests_run_in_ci() -> None:
    """This file included. `claude:tests` is a member of `ci:lint`, which the template's
    `lint` job runs — and a workflow that ran neither would leave every shape asserted here,
    the tier-partition tests below among them, true on a developer's machine and nowhere else: a leg
    deleted from deep's matrix was green in CI.

    Presence is half of it: a step whose failure is tolerated or skipped is
    the same green as no step. One carrier must run unconditionally and be
    able to fail its job; further conditional carriers are free.
    """
    carriers = [
        (workflow.name, name, _disarmed(job, step))
        for workflow in WORKFLOWS
        for name, job in (_load(workflow).get("jobs") or {}).items()
        for step in (job.get("steps") or [])
        if _step_reaches(step, "claude:tests")
    ]
    assert carriers, (
        "no workflow step runs `task claude:tests` or a `ci:*` alias that reaches it — the "
        "structural tests of `.claude/` are then a gate nothing in CI executes, and this "
        "assertion is the one that says so"
    )
    assert any(not reasons for _, _, reasons in carriers), (
        f"every workflow step reaching `task claude:tests` is disarmed: "
        f"{[(workflow, job, reasons) for workflow, job, reasons in carriers]} — a tolerated or "
        f"skipped failure is the same green as no step at all, so one carrier must be "
        f"unconditional and able to fail its job"
    )


# ---------------------------------------------------------------------------
# R8: no `.claude/**` document claims a job its workflow no longer has
# ---------------------------------------------------------------------------

# The shape a record uses to assert CI coverage: ``verify-deep.yml`'s `build`
# job``. Agents route on these (arch-principles.md sends them to the ADRs
# before a decision in this domain), so a deleted job leaves a document
# asserting coverage that no longer exists — which is how two AI-config files
# came to state opposite things about the same tier. A record that must name a
# retired job writes it in any other shape than this one; this needle reads
# claims, and cannot tell a claim from a quotation of one.
#
# `matrix` and `leg`, not `job` alone. A matrix job is written the way its
# author reads it — ``verify-deep.yml`'s `build` matrix``, ``… matrix Windows
# leg`` — and `job` alone missed exactly the A-10 sentence this sweep exists
# for. It still misses `subsystem-ci.md`'s table idiom (``verify-deep.yml` →
# `build` Windows leg`), where the noun is two words away from the name;
# matching across that gap costs more false-positive surface than the one
# document is worth, and it is not where the drift was.
_JOB_CLAIM = re.compile(r"`([a-z0-9-]+\.yml)`(?:'s)?\s+`([A-Za-z0-9_-]+)`\s+(?:job|matrix|leg)\b")

# Claims that name a job their workflow does not have and are right to — keyed
# by the (path, claim) pair, never by the path: a path-keyed exemption waves
# through every further stale claim the same document later grows, which is
# the hole this test exists to close. A new entry is a document to fix, not a
# line to add.
#
# The one member is not a defect awaiting repair. `plan_crate_split_workspace.md`
# records that ruling D5 DELETED `smoke-windows`, and quotes the ADR sentence
# that outlived it as the finding under repair — naming a job the workflow
# lacks is that document's correct content, and this needle cannot tell a
# deletion record from an assertion of coverage.
_STALE_JOB_CLAIMS = frozenset(
    {
        ".claude/artifacts/plan_crate_split_workspace.md: verify-basic.yml smoke-windows",
        # The adoption of the conventions template retired the repo-written
        # `verify-basic.yml` `smoke` / `workflow-lint` jobs and `verify-deep.yml` `build` /
        # `satellite-verify` jobs: the template owns those two files now and the repo-owned jobs
        # moved to `basic-repo.yml` and `deep-repo.yml`. The records below are dated and quote what
        # existed when they were written, so each is a deletion record in the sense above.
        ".claude/artifacts/adr_bazel_build_adoption.md: verify-basic.yml smoke",
        ".claude/artifacts/adr_test_speed_tiers.md: verify-basic.yml smoke",
        ".claude/artifacts/adr_toolchain_activation.md: verify-deep.yml build",
        ".claude/artifacts/issue_drafts_crate_split_satellites.md: verify-deep.yml satellite-verify",
        ".claude/artifacts/migration_plan_bazel.md: verify-basic.yml smoke",
        ".claude/artifacts/plan_bazel_build_adoption.md: verify-basic.yml smoke",
        ".claude/artifacts/plan_crate_split_workspace.md: verify-basic.yml smoke",
        ".claude/artifacts/plan_crate_split_workspace.md: verify-basic.yml workflow-lint",
        ".claude/artifacts/plan_crate_split_workspace.md: verify-deep.yml build",
        ".claude/artifacts/plan_test_speed_tiers.md: verify-basic.yml smoke",
        ".claude/artifacts/research_test_tier_operability.md: verify-basic.yml smoke",
        ".claude/artifacts/review_adr_bazel_quality.md: verify-basic.yml smoke",
        ".claude/artifacts/review_adr_bazel_security.md: verify-basic.yml smoke",
    }
)


def _workflow_jobs() -> dict[str, frozenset[str]]:
    return {
        workflow.name: frozenset((_load(workflow).get("jobs") or {}))
        for workflow in WORKFLOWS
    }


def _stale_job_claims() -> set[str]:
    """Every `<path>: <workflow>.yml <job>` claim in `.claude/**` that no workflow satisfies."""
    jobs = _workflow_jobs()
    claude = ROOT / ".claude"
    return {
        f"{document.relative_to(ROOT).as_posix()}: {workflow} {job}"
        for document in sorted(claude.rglob("*.md"))
        # A nested worktree is a whole other checkout of this repository, so its
        # documents claim jobs from ITS branch's workflows, not the ones read
        # above. Relative to `.claude/`, never absolute — an agent runs this
        # gate from a path that may itself contain `worktrees`.
        if "worktrees" not in document.relative_to(claude).parts
        for workflow, job in _JOB_CLAIM.findall(document.read_text(encoding="utf-8"))
        if workflow in jobs and job not in jobs[workflow]
    }


@pytest.mark.parametrize(
    "sentence",
    [
        pytest.param("`verify-deep.yml`'s `build` job runs", id="job"),
        pytest.param("`verify-deep.yml`'s `build` matrix runs", id="matrix"),
        pytest.param("`verify-deep.yml`'s `build` matrix Windows leg", id="matrix-leg"),
        pytest.param("`verify-deep.yml`'s `build` leg", id="leg"),
    ],
)
def test_the_job_claim_needle_reads_a_claim(sentence: str) -> None:
    """The needle's own red state: a drifted pattern would leave the sweep vacuous.

    The three nouns are the ones `.claude/**` actually writes — `job` alone
    read past ``verify-deep.yml`'s `build` matrix``, the sentence this sweep
    was added for.
    """
    assert _JOB_CLAIM.findall(sentence) == [("verify-deep.yml", "build")]


def test_the_job_claim_needle_reads_past_a_non_claim() -> None:
    assert not _JOB_CLAIM.findall("`verify-basic.yml` `smoke` jobbing")
    assert not _JOB_CLAIM.findall("`verify-deep.yml` builds on three runners")
    assert not _JOB_CLAIM.findall("a Windows job of `verify-basic.yml`")
    assert not _JOB_CLAIM.findall("`verify-deep.yml`'s `build` step")


def test_no_claude_document_claims_a_job_its_workflow_lacks() -> None:
    stale = _stale_job_claims()
    unlisted = sorted(stale - _STALE_JOB_CLAIMS)
    assert not unlisted, (
        f"`.claude/**` claims naming a workflow job that does not exist: {unlisted}. "
        f"A deleted job leaves the record asserting coverage nothing runs — re-point the "
        f"sentence at the job that carries it now; a record that must name a retired job "
        f"writes it in some other shape than ``<file>.yml`'s `<job>` job/matrix/leg``"
    )
    fixed = sorted(_STALE_JOB_CLAIMS - stale)
    assert not fixed, (
        f"listed claims that are no longer wrong: {fixed} — remove them from "
        f"`_STALE_JOB_CLAIMS`, the set only ever shrinks"
    )


# ---------------------------------------------------------------------------
# Basic runs on Linux except its two native smoke legs; deep owns the full native matrix
# ---------------------------------------------------------------------------


def _off_linux(workflow: Path, inputs: dict | None = None) -> tuple[int, dict[str, str]]:
    """`(jobs read, {<workflow>:<job>: runner label})` for every job of `workflow` not on a literal `ubuntu-*`.

    A job that calls a repo workflow is followed into it with its `with:` as the callee's
    `inputs`, so `runs-on: ${{ inputs.os }}` is read as the label the caller passed. Any other
    expression is returned as written, which reads as non-Linux — a shape this file cannot resolve
    is one it must not wave through.
    """
    read = 0
    off: dict[str, str] = {}
    for name, job in (_load(workflow).get("jobs") or {}).items():
        if callee := _local_callee(job):
            count, found = _off_linux(WORKFLOW_DIR / callee, job.get("with") or {})
            read, off = read + count, off | found
            continue
        read += 1
        label = re.sub(
            r"\$\{\{\s*inputs\.([\w-]+)\s*\}\}",
            lambda match: str((inputs or {}).get(match.group(1), match.group(0))),
            str(job.get("runs-on", "")),
        )
        if not label.startswith("ubuntu-"):
            off[f"{workflow.name}:{name}"] = label
    return read, off


#: The basic tier's only jobs off a literal `ubuntu-*` runner: the native smoke legs. `unit-native` is the
#: unit bundle on macOS and Windows; `acceptance.yml:shard` is read through `acceptance-native`, whose
#: `os` is a matrix expression, so it reads as non-Linux. Anything else is a cost every push pays.
BASIC_NATIVE_JOBS = frozenset({"basic-repo.yml:unit-native", "acceptance.yml:shard"})


def test_basic_runs_on_linux_except_its_native_smoke_legs() -> None:
    """Strict on purpose: a list or a `${{ matrix.* }}` expression cannot be
    read as Linux here, so only a literal `ubuntu-*` label passes, and the two native legs are
    named. The basic tier is the template's `verify-basic.yml` and, through its `repo` job,
    `basic-repo.yml` and the reusable workflows that calls."""
    read, off = _off_linux(VERIFY_BASIC)
    assert read >= 10, f"read {read} basic-tier jobs, expected at least 10 — a caller was not followed"
    assert set(off) == BASIC_NATIVE_JOBS, (
        f"basic-tier jobs not on a literal `ubuntu-*` runner: {off}, expected exactly {sorted(BASIC_NATIVE_JOBS)}. "
        f"Basic is the tier a pull request pays for (D5): Windows x64 and macOS arm64 run the unit bundle and "
        f"the smoke tier there, and every other platform leg is deep-repo.yml's"
    )
    legs = _repo_job(BASIC_REPO, "unit-native")["strategy"]["matrix"]["job"]
    assert {leg["os"] for leg in legs} == {"macos-latest", "windows-latest"}, f"basic `unit-native` legs are {legs}"


def test_the_linux_reader_reds_an_off_linux_job_and_resolves_an_input(tmp_path: Path) -> None:
    """The reader's red and green: a Windows job is reported, and `${{ inputs.os }}` is the label
    the caller passed, so the same workflow is Linux for one caller and not for another."""
    workflow = tmp_path / "w.yml"
    workflow.write_text(
        "on: {workflow_call: {}}\njobs:\n  a: {runs-on: windows-latest}\n  b: {runs-on: '${{ inputs.os }}'}\n",
        encoding="utf-8",
    )
    read, off = _off_linux(workflow, {"os": "macos-latest"})
    assert (read, set(off)) == (2, {"w.yml:a", "w.yml:b"}), off
    read, off = _off_linux(workflow, {"os": "ubuntu-24.04"})
    assert (read, set(off)) == (2, {"w.yml:a"}), off


def _runner_labels(job: dict) -> list[str]:
    """The runner labels a job can land on, the matrix expanded.

    Only `${{ matrix.job.os }}` is expanded; any other expression is returned
    as written, which reads as non-Linux — a shape this file cannot resolve is
    one it must not wave through.
    """
    runs_on = str(job.get("runs-on", ""))
    if runs_on == "${{ matrix.job.os }}":
        return [str(entry["os"]) for entry in job["strategy"]["matrix"]["job"]]
    return [runs_on]


# What a download-and-execute job must not do: build. `cargo` and `bazel` as
# words (so `cargo-nextest` and `bazelisk` read too), and the three setup
# spellings a Rust toolchain arrives through.
_BUILD_TOOLING = re.compile(r"\b(cargo|bazel|bazelisk|rustup|rustc)\b|rust-toolchain|setup-rust|build-rust|bazel-cache-rc")
# `acceptance-native` is a call of `acceptance.yml`, so it has no runner of its own to read here.
NATIVE_DEEP_JOBS = frozenset({"unit-native"})


def test_the_cross_workflow_is_the_one_linux_compile() -> None:
    """Every binary the deep tier tests is compiled on one Linux host, in `cross.yml`.

    The macOS and Windows legs download what that workflow built (the next test holds them to
    it), so a second compile site is a second toolchain to drift.
    """
    cross = _load(CROSS)["jobs"]["cross"]
    assert cross.get("runs-on") == "ubuntu-latest", (
        f"cross.yml `cross.runs-on` is `{cross.get('runs-on')}` — every binary in the deep tier is "
        f"compiled on one Linux host"
    )
    assert any(str(step.get("run", "")).strip().startswith("task cross:build") for step in cross["steps"]), (
        "cross.yml `cross` never runs `task cross:build`"
    )


def test_deep_cross_matrix_is_every_supported_target() -> None:
    """One `cross` leg per `task cross:targets` triple: a missing leg is a
    platform whose binary the run never builds, and nothing else would red."""
    vars_ = _load(ROOT / "taskfiles" / "cross.taskfile.yml")["vars"]
    targets = vars_["TARGETS"].split()
    assert len(targets) >= 8, f"read {targets} from cross.taskfile.yml TARGETS"
    built = sorted(leg["target"] for leg in _cross_matrix())
    assert built == sorted(targets), f"cross.yml's default matrix is {built}, `task cross:targets` is {sorted(targets)}"


@pytest.mark.parametrize("workflow_file", ["deep-repo.yml", "basic-repo.yml"])
def test_non_linux_jobs_only_download_and_execute(workflow_file: str) -> None:
    """Every non-ubuntu job of deep-repo.yml and basic-repo.yml contains no build tooling.

    All compilation happens in `cross.yml`; a macOS or Windows runner that grows a
    `cargo`, `bazel` or Rust-toolchain step is paying 10x-minute rates for a second compile and
    reintroducing the toolchain drift the cross-build removed. A job that calls a reusable
    workflow has no runner of its own and is read in that workflow (the shell activation matrix
    is `shell-activation-deep.yml`'s, and installs what it tests).
    """
    workflow = WORKFLOW_DIR / workflow_file
    jobs = {name: job for name, job in _repo_jobs(workflow).items() if "uses" not in job}
    native = {
        name: job
        for name, job in jobs.items()
        if any(not label.startswith("ubuntu-") for label in _runner_labels(job))
    }
    # The floor, before the negative below: a reader that found no native job
    # sweeps clean and says nothing.
    assert set(native) == NATIVE_DEEP_JOBS, (
        f"{workflow.name} non-Linux jobs are {sorted(native)}, expected exactly "
        f"{sorted(NATIVE_DEEP_JOBS)}"
    )
    for name, job in native.items():
        labels = _runner_labels(job)
        missing = [family for family in ("macos", "windows") if not any(label.startswith(f"{family}-") for label in labels)]
        assert not missing, f"{workflow.name} `{name}` runs on {labels}, no {missing} entry"
        assert "exclude" not in job["strategy"]["matrix"], f"{workflow.name} `{name}` matrix has an `exclude`"
        offending = [
            step.get("name", step.get("uses", step.get("run")))
            for step in job["steps"]
            if _BUILD_TOOLING.search(yaml.safe_dump({k: v for k, v in step.items() if k != "name"}))
        ]
        assert not offending, (
            f"{workflow.name} `{name}` runs on a non-Linux runner and has build steps {offending} — "
            f"non-Linux jobs download what `cross` cross-compiled and execute it, nothing more"
        )


def test_linux_acceptance_is_four_shards_feeding_a_merge_job() -> None:
    """Sharding is only a coverage claim if the floor is judged over the merge."""
    inputs = _workflow_call(ACCEPTANCE)["inputs"]
    assert inputs["shard-count"]["default"] == 4, f"acceptance.yml `shard-count` defaults to {inputs['shard-count']}"
    passed = (_repo_job(BASIC_REPO, "acceptance").get("with") or {}).get("shard-count", 4)
    assert passed == 4, f"basic-repo.yml `acceptance` passes shard-count={passed!r}, expected the default 4"
    jobs = _repo_jobs(ACCEPTANCE)
    shard, merge = jobs["shard"], jobs["merge"]
    assert shard["runs-on"] == "${{ inputs.os }}", f"acceptance.yml `shard.runs-on` is {shard['runs-on']!r}"
    runs = [str(step.get("run", "")) for step in shard["steps"]]
    assert 'task test:accept:native SHARD_COUNT="$SHARD_COUNT" SHARD_INDEX="$SHARD_INDEX"' in runs, (
        f"acceptance.yml `shard` never runs its shard: {runs}"
    )
    run_step = next(step for step in shard["steps"] if "test:accept:native" in str(step.get("run", "")))
    assert str(run_step["env"]["SHARD_INDEX"]) == "${{ matrix.shard }}", (
        f"acceptance.yml `shard` index is {run_step['env'].get('SHARD_INDEX')!r}, expected the matrix value"
    )
    plan = " ".join(str(step.get("run", "")) for step in jobs["plan"]["steps"])
    assert 'seq -s, 0 "$((SHARD_COUNT - 1))"' in plan, (
        "acceptance.yml `plan` does not list the shards 0-based (0..N-1), which is what accept_shard.py "
        "and the merge job's `shard-<I>-of-<N>.xml` names expect"
    )
    assert _needs(merge) == ["shard"], f"acceptance.yml `merge` needs {_needs(merge)}, expected `shard`"
    assert "!cancelled()" in str(merge.get("if", "")), (
        "acceptance.yml `merge` has no `!cancelled()` — a red shard would skip the merge "
        "and the floor would never be judged over what did run"
    )
    assert any(str(step.get("run", "")).startswith("task test:accept:floor ") for step in merge["steps"]), (
        "acceptance.yml `merge` never runs `task test:accept:floor`"
    )


# ---------------------------------------------------------------------------
# WP-42 R16: a parser's gate and its self-test travel together
# ---------------------------------------------------------------------------

#: The parser gate live in CI today, and the invocation that proves it, as two
#: needles into **one task's recipe**. Both are substrings of the same
#: `scripts/bazel_test_floor.py` command, so they are matched with the argument
#: that distinguishes them attached — `--bep` versus the pytest module — never on
#: the script name, which both carry.
#:
#: This was a `<CI gate step>` → `<CI proof step>` pairing over the workflow
#: corpus until the WP-30 lane swap. That swap moved the gate's subject: the
#: Linux unit run is `bazel test //crates/...` floored off its own build event
#: stream, `task rust:test:ceiling` left CI entirely (its self-test step stayed,
#: as a `.verify:build-test` member), and a pairing keyed on a gate no job runs
#: asserts nothing about anything. The property is stronger one layer down —
#: the parser and its proof are two `cmds:` of the *same* task, so no workflow
#: edit can schedule them apart, and a job running the gate runs the proof by
#: construction.
_FLOOR_PARSER_GATE = 'scripts/bazel_test_floor.py" --bep'
_FLOOR_PARSER_PROOF = "scripts/tests/test_bazel_test_floor.py"


def _recipe(taskfile: str, task: str) -> str:
    """Every command string of `task`, including `defer:`/`cmd:` forms, as one blob.

    A `task:` dispatch to another task is **not** followed: what this asserts is
    that two commands share one recipe, and a dispatch is exactly the seam that
    would let them be scheduled apart.
    """
    document = yaml.safe_load((ROOT / "taskfiles" / taskfile).read_text(encoding="utf-8"))
    entries = document["tasks"][task].get("cmds") or []
    return "\n".join(
        entry if isinstance(entry, str) else str(entry.get("cmd", entry.get("defer", "")))
        for entry in entries
    )


def test_the_unit_lane_runs_its_floor_parser_and_that_parser_s_self_test() -> None:
    """WP-42 R16, one layer below CI. `bazel_test_floor.py` is a shell parser —
    it reads each `TestResult`'s `test.log` for libtest's `test result:` line —
    and a lane that runs it without its self-test is green whether or not the
    parser still matches anything at all."""
    recipe = _recipe("bazel.taskfile.yml", "test:unit")
    assert _FLOOR_PARSER_GATE in recipe, (
        f"`bazel:test:unit` no longer runs `{_FLOOR_PARSER_GATE}` — this test then pins the "
        f"self-test of a parser nothing calls, which is what the CI-step pairing it replaced "
        f"decayed into. Re-anchor it on whatever floors the unit lane now"
    )
    assert _FLOOR_PARSER_PROOF in recipe, (
        f"`bazel:test:unit` runs `{_FLOOR_PARSER_GATE}` but never `{_FLOOR_PARSER_PROOF}` — the "
        f"gate's own parser is then proved nowhere the gate runs (WP-42 R16). Keep them in one "
        f"recipe rather than two steps something can schedule apart"
    )


_DURATION_PROOF = "uv run --project test pytest scripts/tests/test_unit_test_duration_gate.py -q"
#: The proof also runs inside `task scripts:self-test`, whose recipe is `pytest scripts/tests` — the
#: directory holding that module (asserted below, so a moved module cannot leave this standing).
_DURATION_PROOF_MODULE = ROOT / "scripts" / "tests" / "test_unit_test_duration_gate.py"
_SELF_TEST_RUN = "task scripts:self-test"


def test_the_duration_budget_is_proved_where_it_runs() -> None:
    """`rust:test:duration` is a parser over a run it does not own, so a job that
    runs it without its proof (`scripts/tests/test_unit_test_duration_gate.py`) is
    green whether or not the parser still matches anything.

    This pairing arrived on `main` while the Bazel branch was replacing the
    `rust:test:ceiling` half of the same table; resolving that rebase dropped the
    table, and with it this assertion. Keyed on whole `run:` lines, never
    substrings: `task rust:test:duration` is a prefix of its own self-test's
    command, so a substring test would find the proof standing in for the gate.

    **No workflow runs the live gate today** — its subject, `target/nextest/run.log`,
    left with the nextest bracket the lane swap removed, exactly as
    `rust:test:ceiling`'s did. So the loop below would be vacuous on its own, and
    the second assertion is what keeps this test honest: the self-test must run
    somewhere. Re-adding the budget without its fixtures reds the loop; deleting
    the fixtures reds the floor.

    The proof runs as the explicit command or as a member of `task scripts:self-test`
    (`basic-repo.yml`'s `scripts-tests` and the template's `ci:deep`), which collects the whole
    of `scripts/tests`.
    """
    assert _DURATION_PROOF_MODULE.is_file(), f"{_DURATION_PROOF_MODULE.relative_to(ROOT)} is gone"
    proved: list[str] = []
    for name, document in _documents():
        for job_name, job in (document.get("jobs") or {}).items():
            runs = {str(step.get("run", "")).strip() for step in (job.get("steps") or [])}
            proves = _DURATION_PROOF in runs or _SELF_TEST_RUN in runs
            if proves:
                proved.append(f"{name}:{job_name}")
            if "task rust:test:duration" not in runs:
                continue
            assert proves, (
                f"{name}:{job_name} runs `task rust:test:duration` and never "
                f"`{_DURATION_PROOF}` or `{_SELF_TEST_RUN}` — the budget parser is then proved "
                f"nowhere the budget runs"
            )
    assert proved, (
        f"no workflow runs `{_DURATION_PROOF}` or `{_SELF_TEST_RUN}` — the budget parser's "
        "red/red/green fixtures then run nowhere, and the loop above is vacuous"
    )


def _rendered_unit_lane(runner_temp: Path | None) -> str:
    """The `bazel …` command `bazel:test:unit` would run, as go-task renders it.

    Through `task --dry` rather than by re-evaluating the `sh:` expression here: the
    expression's value depends on *which shell* runs it, and a probe that picks its
    own shell measures a different thing than the lane does.
    """
    environment = {key: value for key, value in os.environ.items() if key != "RUNNER_TEMP"}
    if runner_temp is not None:
        environment["RUNNER_TEMP"] = str(runner_temp)
    rendered = subprocess.run(
        ["task", "bazel:test:unit", "--dry", "--force"],
        capture_output=True,
        encoding="utf-8",
        cwd=ROOT,
        env=environment,
        check=False,
    )
    lines = [
        line.strip()
        for line in (rendered.stdout + rendered.stderr).splitlines()
        if "test //crates/..." in line
    ]
    assert len(lines) == 1, (
        f"`task bazel:test:unit --dry` rendered {len(lines)} lines invoking the unit lane, "
        f"expected 1 — nothing below can be read from that. stderr: {rendered.stderr[-400:]}"
    )
    return lines[0]


@pytest.mark.skipif(sys.platform != "linux", reason="`bazel:test:unit` is `platforms: [linux]`")
def test_the_unit_lane_passes_the_cache_credential_only_when_it_exists(tmp_path: Path) -> None:
    """The credential's last hop, measured through the shell that actually runs it.

    Two states, and each has a failure mode the other cannot show. **With** the rc
    file, `--bazelrc=<path>` must reach bazel: the first spelling of this used
    `printf -- '--bazelrc=%s'`, and go-task's embedded shell (mvdan/sh, not bash)
    does not honour `--` as printf's end-of-options — it took `--` as the *format*
    and rendered `bazel -- test //crates/...`, dropping the credential with nothing
    failing. **Without** it, the flag must be absent: `--bazelrc` naming a missing
    file is a hard error (9.2.0: exit 2, `Unable to read .bazelrc file`), and that is
    the fork-PR lane, which must read anonymously rather than fail.

    The expression is the root `taskfile.yml`'s `CACHE_RC` var (the one definition every
    task and include inherits); it is read from there as well as rendered, so a second
    definition shadowing it cannot leave this test measuring the wrong one.
    """
    expression = yaml.safe_load((ROOT / "taskfile.yml").read_text(encoding="utf-8"))["vars"]["CACHE_RC"]["sh"]
    assert "printf '%s'" in expression and "printf --" not in expression, (
        f"root taskfile.yml `CACHE_RC` is {expression!r}; go-task's shell takes `printf --` as the format"
    )
    (tmp_path / "bazel-cache.rc").write_text("build --remote_header='x=y'\n", encoding="utf-8")

    with_credential = _rendered_unit_lane(tmp_path)
    assert f"bazel --bazelrc={tmp_path}/bazel-cache.rc test" in with_credential, (
        f"the unit lane does not pass the cache rc as a startup flag: {with_credential!r}. "
        f"Every request is then an anonymous 401 and the lane compiles cold, green"
    )

    for state, runner_temp in (("no $RUNNER_TEMP", None), ("no rc file", tmp_path / "empty")):
        (tmp_path / "empty").mkdir(exist_ok=True)
        without = _rendered_unit_lane(runner_temp)
        assert "--bazelrc" not in without, (
            f"the unit lane names `--bazelrc` with {state}: {without!r}. Bazel exits 2 on an "
            f"rc file it cannot read, so this is a fork PR and a developer shell failing on a "
            f"credential they are not meant to have"
        )


_USER_RC_SILENT = "build --jobs=4\n"
_USER_RC_IMPORTS_THE_CACHE_RC = "try-import {rc}\n"


@pytest.mark.skipif(sys.platform == "win32", reason="go-task's shell runs an external `grep`")
@pytest.mark.parametrize(
    ("user_rc", "cache_rc_exists", "passes_the_flag"),
    [
        pytest.param(None, False, False, id="no-rc-file"),
        pytest.param(_USER_RC_SILENT, True, True, id="rc-file-user-rc-silent"),
        pytest.param(_USER_RC_IMPORTS_THE_CACHE_RC, True, False, id="user-rc-already-imports-it"),
    ],
)
def test_the_cache_rc_is_read_once_whichever_way_it_arrives(
    tmp_path: Path, user_rc: str | None, cache_rc_exists: bool, passes_the_flag: bool
) -> None:
    """`CACHE_RC` in its three states, through go-task's own shell, on the real expression.

    The setup action's trusted mode writes `$RUNNER_TEMP/bazel-cache.rc` and imports it from
    `.bazelrc.user`; a `--bazelrc=` for the same file would then load it twice and repeat its
    `--remote_header`. So the flag is passed only when the file exists **and** `.bazelrc.user`
    does not already name it. A `.bazelrc.user` that merely exists changes nothing: the check
    is on its text. The expression is read from the root `taskfile.yml` and evaluated in a
    scratch taskfile, so `{{.ROOT_DIR}}` is the scratch directory and a developer's own
    `.bazelrc.user` is never read or written.
    """
    expression = yaml.safe_load((ROOT / "taskfile.yml").read_text(encoding="utf-8"))["vars"]["CACHE_RC"]["sh"]
    rc = tmp_path / "bazel-cache.rc"
    if cache_rc_exists:
        rc.write_text("build --remote_header='x=y'\n", encoding="utf-8")
    if user_rc is not None:
        (tmp_path / ".bazelrc.user").write_text(user_rc.format(rc=rc), encoding="utf-8")
    (tmp_path / "Taskfile.yml").write_text(
        yaml.safe_dump(
            {
                "version": "3",
                "vars": {"CACHE_RC": {"sh": expression}},
                "tasks": {"show": {"silent": True, "cmds": ['echo "[{{.CACHE_RC}}]"']}},
            }
        ),
        encoding="utf-8",
    )
    environment = {**os.environ, "RUNNER_TEMP": str(tmp_path)}
    rendered = subprocess.run(
        ["task", "show"], capture_output=True, encoding="utf-8", cwd=tmp_path, env=environment, check=False
    )
    assert rendered.returncode == 0, rendered.stderr
    expected = f"[--bazelrc={rc}]" if passes_the_flag else "[]"
    assert rendered.stdout.strip() == expected, (
        f"CACHE_RC rendered {rendered.stdout.strip()!r}, expected {expected!r}. Passing the flag with "
        f"`.bazelrc.user` already importing the file loads the credential twice; dropping it with no "
        f"import leaves every request an anonymous 401"
    )


# ---------------------------------------------------------------------------
# `cross:*` tasks take their values from the task call, as one literal word each
# ---------------------------------------------------------------------------

CROSS_TASKFILE = ROOT / "taskfiles" / "cross.taskfile.yml"
# Stands in for `python3 scripts/test_bundle.py`: it prints the argv it was handed.
_ARGV_STUB = "import json, sys\nprint(json.dumps(sys.argv[1:]))\n"


def _run_cross(tmp_path: Path, task_name: str, args: list[str], exported: dict[str, str]):
    """`task <task_name> <args>` on the real cross taskfile, `PY` swapped for the argv stub."""
    stub = tmp_path / "argv_stub.py"
    stub.write_text(_ARGV_STUB, encoding="utf-8")
    interpreter = f"{shlex.quote(sys.executable)} {shlex.quote(str(stub))}"
    return subprocess.run(
        ["task", "--taskfile", str(CROSS_TASKFILE), task_name, *args, f"PY={interpreter}", "CACHE_RC="],
        capture_output=True,
        encoding="utf-8",
        cwd=tmp_path,
        env={**os.environ, **exported},
        check=False,
    )


def _argv(done: subprocess.CompletedProcess[str]) -> list[str]:
    assert done.returncode == 0, done.stderr
    return json.loads(done.stdout.strip().splitlines()[-1])


def test_cross_test_run_takes_its_arguments_over_exported_variables_of_the_same_name(tmp_path: Path) -> None:
    """An exported `FILTER` (or `BUNDLE`, `SOURCE_ROOT`, `JUNIT`) must not replace the task argument.

    Run through go-task's own shell on the real `test:run`. A task `env:` block that mirrors the
    arguments lets the caller's shell value win, so a stale `export FILTER=...` silently runs the
    wrong subset and the summary still reads green.
    """
    exported = {name: f"exported-{name}" for name in ("BUNDLE", "SOURCE_ROOT", "JUNIT", "FILTER")}
    args = ["BUNDLE=b.tar", "SOURCE_ROOT=src", "JUNIT=j.xml", "FILTER=arg"]
    assert _argv(_run_cross(tmp_path, "test:run", args, exported)) == [
        "scripts/test_bundle.py",
        "run",
        "--bundle",
        "b.tar",
        "--source-root",
        "src",
        "--junit",
        "j.xml",
        "--filter",
        "arg",
    ]


def test_cross_test_run_passes_shell_metacharacters_as_one_literal_word(tmp_path: Path) -> None:
    """A `"`, `$(`, backtick or `'` in a value never runs and never splits.

    Both command-substitution spellings create the canary, so a value that reaches the shell line
    unquoted or in the wrong quote style shows as a file, not only as a changed argv.
    """
    canary = tmp_path / "canary"
    hostile = f"a\"$(touch {canary})`touch {canary}`'q r"
    root = f"{tmp_path}/with space"
    done = _run_cross(tmp_path, "test:run", ["BUNDLE=b.tar", f"SOURCE_ROOT={root}", f"FILTER={hostile}"], {})
    assert _argv(done) == [
        "scripts/test_bundle.py",
        "run",
        "--bundle",
        "b.tar",
        "--source-root",
        root,
        "--filter",
        hostile,
    ]
    assert not canary.exists(), "a value executed as shell text"


@pytest.mark.parametrize("task_name", ["build", "test:bundle", "test:bin", "build:bundle"])
@pytest.mark.parametrize("breaker", ["x';touch {canary};'", 'x"$(touch {canary})"'], ids=["single-quote", "subshell"])
def test_cross_target_with_shell_metacharacters_is_refused_not_run(
    tmp_path: Path, task_name: str, breaker: str
) -> None:
    """`TARGET` is checked against the supported list as one literal word, before anything runs.

    A `'` in the value ends a single-quoted `'{{.TARGET}}'`; a `"` ends the double-quoted refusal
    message and lets `$(…)` run. The refusal line is matched whole: go-task echoes each command
    before running it, so a substring match would also pass on the echo of the unexpanded text.
    """
    canary = tmp_path / "canary"
    hostile = breaker.format(canary=canary)
    done = _run_cross(tmp_path, task_name, [f"TARGET={hostile}"], {})
    assert not canary.exists(), "a TARGET value executed as shell text"
    assert f"unsupported TARGET '{hostile}'; see: task cross:targets" in done.stderr.splitlines(), done.stderr


# ---------------------------------------------------------------------------
# CI runs every step of `.verify:build-test` (B5R-4)
# ---------------------------------------------------------------------------

#: Phase steps no `ci:*` alias reaches, and the workflow that runs each under the spelling given.
#: Every row is asserted live below: an exemption whose target is gone forbids nothing while
#: reading as coverage, so a task dropped from the phase, or a workflow that stopped running one,
#: reds here.
_BUILD_TEST_ELSEWHERE = {
    "rust:license:check": ("verify-licenses.yml", "task rust:license:check"),
    "rust:license:deps": ("verify-licenses.yml", "task rust:license:deps"),
    "rust:license:notice:check": ("verify-licenses.yml", "task rust:license:notice:check"),
    # The acceptance suite is `acceptance.yml`'s: four pytest shards over the rebuilt binaries
    # (`task test:accept:native`), floored by a merge job. The local gate still runs the Bazel lane.
    "bazel:test:accept": ("acceptance.yml", "task test:accept:native"),
}


def _build_test_steps() -> list[str]:
    """The tasks `.verify:build-test` runs, parsed rather than read line by line.

    Every entry of that phase is a `task:` dispatch, so a step spelled any
    other way is a gate this reader cannot name — and would silently leave the
    set it is compared against. It is refused rather than skipped.
    """
    phase = yaml.safe_load((ROOT / "taskfile.yml").read_text(encoding="utf-8"))["tasks"][".verify:build-test"]
    entries = [*(phase.get("deps") or []), *(phase.get("cmds") or [])]
    assert entries, "taskfile.yml `.verify:build-test` has no steps"
    opaque = [entry for entry in entries if not (isinstance(entry, dict) and "task" in entry)]
    assert not opaque, f"taskfile.yml `.verify:build-test` entries that are not a `task:` dispatch: {opaque}"
    return [entry["task"] for entry in entries]


def _runs(workflow: str, command: str) -> bool:
    """Whether any job of `workflow` runs `command` in a way that can fail it.

    Matched on the step's own `run:`, prefix-anchored so a task is not found
    inside a longer task's name, or on a `task ci:<tier>` step whose tier reaches the task
    (`_step_reaches`). A step tolerated by `continue-on-error` is
    not coverage — that green cannot be told from no step at all — unless it
    carries an `id` a later step of the same job reads, which is section 3 of
    subsystem-ci.md's deliberate shape: a lint step never blocks the test results
    and the job's last step exits 1 on their outcomes.

    `if:` is deliberately not weighed here. A trigger or platform condition
    is scheduling, not disarming, and which events fire which workflow is
    already this file's subject above.
    """
    document = _load(ROOT / ".github" / "workflows" / workflow)
    for job in (document.get("jobs") or {}).values():
        if job.get("continue-on-error", False) is not False:
            continue
        steps = job.get("steps") or []
        for index, step in enumerate(steps):
            literal = re.match(rf"{re.escape(command)}(?![\w:-])", str(step.get("run", "")).strip())
            if not (literal or _step_reaches(step, command.removeprefix("task "))):
                continue
            if step.get("continue-on-error", False) is False:
                return True
            identifier = step.get("id")
            if identifier and any(
                f"steps.{identifier}.outcome" in str(later.get("run", "")) for later in steps[index + 1 :]
            ):
                return True
    return False


@pytest.mark.parametrize("task", _build_test_steps())
def test_ci_runs_every_step_of_the_build_test_phase(task: str) -> None:
    """A step of the full local gate that no workflow runs is a gate CI skips.

    The template's `lint`, `test`, `build` and `docs` jobs run `task ci:lint`, `ci:test`,
    `ci:build` and `ci:docs`, so a phase step is covered when `verify-basic.yml` runs a tier
    that reaches it. The since-deleted `rust:test:doc` was added to the phase and not to the
    workflow: it appeared in none of them, in no spelling, so CI ran zero doctests while every
    local `task verify` ran them. A gate outside the full gate is a gate nobody runs, and a
    hand-kept second list is how that recurs, so the two are held together here instead.
    """
    if task in _BUILD_TEST_ELSEWHERE:
        workflow, command = _BUILD_TEST_ELSEWHERE[task]
        assert _runs(workflow, command), (
            f"`{task}` is exempted from verify-basic.yml on the grounds that {workflow} runs it as "
            f"`{command}`, and {workflow} does not — the exemption forbids nothing while reading "
            f"as coverage. Point it at the file that runs it, or run it in verify-basic.yml"
        )
        return
    assert _runs("verify-basic.yml", f"task {task}"), (
        f"`.verify:build-test` runs `{task}` and verify-basic.yml reaches it through no `ci:*` tier "
        f"— CI would skip it entirely. Add it to a tier in taskfiles/ci.taskfile.yml, or add a "
        f"`_BUILD_TEST_ELSEWHERE` row naming the workflow that runs it (the row is asserted live)"
    )


def test_no_stale_build_test_exemption() -> None:
    """An exemption for a task the phase no longer runs is coverage of nothing."""
    stale = sorted(set(_BUILD_TEST_ELSEWHERE) - set(_build_test_steps()))
    assert not stale, (
        f"_BUILD_TEST_ELSEWHERE exempts {stale}, which `.verify:build-test` no longer runs — "
        f"delete the rows"
    )


# The committed Windows shim blob is what users get; `include_bytes!` ships it
# and only these constants pin its size and digest. Whichever crate holds them
# is the subject the shim gate exists to protect.
_SHIM_BLOB_OWNER = re.compile(r"SHIM_SHA256|SHIM_SIZE_BUDGET")


def _crates_owning_the_shim_blob() -> set[str]:
    """Every workspace crate whose `src/` pins the committed shim blob."""
    owners = set()
    for manifest in (ROOT / "crates").glob("*/Cargo.toml"):
        src = manifest.parent / "src"
        if not src.is_dir():
            continue
        for rs in src.rglob("*.rs"):
            if _SHIM_BLOB_OWNER.search(rs.read_text(encoding="utf-8", errors="ignore")):
                owners.add(manifest.parent.name)
                break
    return owners


def test_the_shim_gate_names_every_crate_that_owns_the_blob() -> None:
    """`build-windows-shims.yml`'s gate must select the crate holding the blob.

    Derived, because the hand-written `-p` list is what went wrong: WP-27 moved
    `shim.rs` and its fifteen tests from `ocx_lib` into `ocx_store` and
    re-pointed eleven other references in this same workflow, but not the
    `cargo nextest` line. The gate then ran zero tests of the crate that owns
    the shim and reported green — on the only trigger path a shim-blob PR has.
    Nine extractions remain, so a list checked by eye will go stale again; a
    list checked against where the constants actually live cannot.

    Floored on its reader: an owner set that comes back empty means the
    constants were renamed, not that the gate is clean.
    """
    owners = _crates_owning_the_shim_blob()
    assert owners, (
        "no crate `src/` defines SHIM_SHA256 or SHIM_SIZE_BUDGET — the reader found "
        "nothing, which is not the same as the gate being correct"
    )

    workflow = ROOT / ".github" / "workflows" / "build-windows-shims.yml"
    runs = [
        str(step.get("run", ""))
        for job in (_load(workflow).get("jobs") or {}).values()
        for step in (job.get("steps") or [])
        if "cargo nextest run" in str(step.get("run", ""))
    ]
    assert runs, f"{workflow.name} runs no `cargo nextest` step at all"

    selected = {p for run in runs for p in re.findall(r"-p\s+(\S+)", run)}
    missing = sorted(owners - selected)
    assert not missing, (
        f"{workflow.name}: the shim gate selects {sorted(selected)} but the committed "
        f"blob is pinned in {missing} — those tests do not run, and the gate is green anyway"
    )


# ---------------------------------------------------------------------------
# The shell zoo's checkout and its task graph are one contract
# ---------------------------------------------------------------------------

SHELL_ACTIVATION = ROOT / ".github" / "workflows" / "shell-activation.yml"
TEST_TASKFILE = ROOT / "test" / "taskfile.yml"
# `test:build`'s `deps:` entry. It is a cross-taskfile reference, so it never
# resolves to a task defined in `test/taskfile.yml` — the name in the graph is
# the whole signal.
SCHEMA_TASK = ":website:schema:default"


def _test_tasks() -> dict[str, dict]:
    return yaml.safe_load(TEST_TASKFILE.read_text(encoding="utf-8"))["tasks"]


def _task_closure(tasks: dict[str, dict], root: str) -> set[str]:
    """Every task name reachable from `root` through `deps:` and `cmds:`.

    A name with no definition here (`:website:schema:default`) is recorded and
    not descended into — which is the point: the foreign name is what the
    caller must not reach.
    """
    seen: set[str] = set()
    stack = [root]
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        body = tasks.get(current) or {}
        for key in ("deps", "cmds"):
            for entry in body.get(key) or []:
                if isinstance(entry, dict) and "task" in entry:
                    stack.append(str(entry["task"]))
    return seen


def test_the_shell_zoo_runs_no_task_its_checkout_cannot_build() -> None:
    """The zoo job checks out without submodules, so its tasks must not run cargo.

    `test:build` generates the JSON schemas through a `deps:` entry, and a
    `deps:` is not covered by the `status:` guard that `SKIP_BUILD=true` sets
    — deliberately, because CI's acceptance legs download the binary and still
    need the schemas. The shell-zoo legs pass the same `SKIP_BUILD=true` but
    mount one binary and three Python modules into a container and read no
    schema at all, and their checkout omits `submodules:` on purpose. Reaching
    `test:build` from there builds `//crates/ocx_schema:schemas` against
    absent `external/` patch sources and reds the job on a tree that is fine —
    which is how both legs failed on `main`.

    The positive control is in this same test: `default` MUST still reach the
    schema task, or the check would pass just as well with the schemas
    generated nowhere.
    """
    jobs = _load(SHELL_ACTIVATION)["jobs"]
    tasks = _test_tasks()

    # Floors, before the negative below. An empty job set, an empty task table,
    # or a zoo job this reader failed to recognise all sweep clean in silence.
    assert tasks, f"{TEST_TASKFILE}: no `tasks:` mapping — the reader found nothing"
    assert SCHEMA_TASK in _task_closure(tasks, "default"), (
        f"`test:default` no longer reaches `{SCHEMA_TASK}` — the acceptance suite's "
        "schema readers are back to passing only where someone ran the website task "
        "by hand, and the negative below would pass for the wrong reason"
    )

    zoo = {
        name: job
        for name, job in jobs.items()
        if any(
            "task test:shells" in str(step.get("run", ""))
            for step in (job.get("steps") or [])
        )
    }
    assert zoo, (
        f"{SHELL_ACTIVATION.name}: no job runs `task test:shells` — the reader that "
        "finds the shell-zoo job found nothing, which is not the same as it being safe"
    )

    for name, job in zoo.items():
        checkout = next(
            (
                step
                for step in (job.get("steps") or [])
                if "actions/checkout" in str(step.get("uses", ""))
            ),
            None,
        )
        assert checkout is not None, f"{name}: no checkout step to read `submodules:` from"
        submodules = (checkout.get("with") or {}).get("submodules")

        invoked = {
            task
            for step in (job.get("steps") or [])
            for task in re.findall(r"\btask\s+test:([A-Za-z0-9:_-]+)", str(step.get("run", "")))
        }
        assert invoked, f"{name}: runs `task test:shells` but no task name parsed out of it"

        needs_cargo = sorted(t for t in invoked if SCHEMA_TASK in _task_closure(tasks, t))
        if submodules:
            continue  # A job that checks the submodules out may build whatever it likes.
        assert not needs_cargo, (
            f"{SHELL_ACTIVATION.name}: job `{name}` checks out without submodules but runs "
            f"test:{', test:'.join(needs_cargo)}, which reach `{SCHEMA_TASK}` and so build `//crates/ocx_schema:schemas` "
            f"against the `external/` patch sources that checkout did not fetch"
        )


# ---------------------------------------------------------------------------
# Bazel remote-cache lane policy
#
# Every same-repo lane that holds the secret reads *and writes* the shared cache:
# each cache step passes the write secret by name, with no event predicate, and the
# rc file the action writes carries the credential helper and the upload grant. A
# fork PR holds no secrets, so the action writes no rc and the lane runs uncached.
# Every assertion is a property of workflow YAML or tracked rc files and needs
# neither a secret nor a network; each reader has a compliant and a violating
# fixture below, run on every invocation.
# ---------------------------------------------------------------------------

#: The one host this repository's lanes may point `--remote_cache` at.
CACHE_HOST = "bazel-cache.ocx.sh"
WRITE_SECRET = "BAZEL_CACHE_WRITE_AUTH"
#: The only binding a cache step may carry: no event predicate, no fallback.
WRITE_AUTH = "${{ secrets.BAZEL_CACHE_WRITE_AUTH }}"
RC_ACTION = ".github/actions/bazel-cache-rc"

#: The conventions template renders these two; a `workflow_call` secret they declare is
#: the baseline check's to hold, so the declare half of the secret sweep skips them.
TEMPLATE_OWNED_WORKFLOWS = frozenset({"verify-basic.yml", "verify-deep.yml"})
#: The reusable workflows the repo owns behind the template's `repo` jobs. Each must
#: exist, so a missing file reads as a finding rather than as nothing to sweep.
REPO_OWNED_REUSABLE = ("basic-repo.yml", "deep-repo.yml", "acceptance.yml", "cross.yml")
#: cargo-dist writes this one and a drift check fails a hand edit, so its
#: `secrets: inherit` cannot be fixed here.
GENERATED_WORKFLOWS = frozenset({"release.yml"})
#: `(caller, callee)` edges the new tier shape implies. Fewer callers than this
#: means the reader stopped matching, which a clean sweep cannot be told from.
REQUIRED_CALL_EDGES = frozenset(
    {
        ("verify-basic.yml", "basic-repo.yml"),
        ("verify-deep.yml", "deep-repo.yml"),
        ("basic-repo.yml", "acceptance.yml"),
        ("deep-repo.yml", "cross.yml"),
        ("basic-repo.yml", "cross.yml"),
    }
)

#: Flags whose *presence* is the finding (BZL-CACHE-26): with no `--remote_executor`
#: a fallback flag is dead configuration, and `--remote_require_cached` is the one
#: flag that turns a cache outage into a red build.
OUTAGE_POLICY_FLAGS = frozenset(
    {
        "remote_local_fallback",
        "incompatible_remote_local_fallback_for_remote_cache",
        "remote_require_cached",
        "experimental_remote_require_cached",
    }
)

#: A job is a Bazel lane when a `run:` body names Bazel or any of these: tasks that
#: reach it through nested `task` calls. Detection is deliberately broad; a job that
#: provably reaches no Bazel is named in `LANE_EXCLUSIONS` instead.
_LANE_NEEDLES = (
    "task cross:",
    "task ci:",
    "schema:generate",
    "rust:build",
    "rust:clippy:check",
    "rust:doc:ratchet",
    "test:smoke",
    "test:accept:native",
    "satellite:",
    "recordings:",
    "website:",
    "test_bundle.py",
)
#: Lane jobs the reader found (exclusions included). A lower count means the reader
#: stopped matching, which a clean tree cannot be told from. The floor tracks the live
#: lane count: a new lane raises it, so a later reader failure cannot hide behind the slack.
BAZEL_LANE_FLOOR = 17
#: Detected lanes that get no cache credential (least privilege), with the reason each
#: reaches no Bazel. A row whose job is no longer a detected lane is a finding.
LANE_EXCLUSIONS = {
    "basic-repo.yml:smoke-acceptance": "pytest on the ocx-binary artifact; CI sets SKIP_BUILD, "
    "so test:.build-binaries is skipped",
    "acceptance.yml:shard": "test:accept:native runs pytest on the binaries the `binary` job built; "
    "the cache credential stays in that job",
    "deep-repo.yml:unit-native": "macOS/Windows runner executing the prebuilt cross bundle",
    "basic-repo.yml:unit-native": "macOS/Windows runner executing the prebuilt cross bundle",
    "cross.yml:plan": "prints the matrix JSON (`task cross:matrix`); the cross legs build, this job does not",
    "deep-repo.yml:satellite-verify": "cargo builds of ocx-mirror; no Bazel",
    "deep-repo.yml:satellite-contract": "cargo builds plus pytest; no Bazel",
}

_WRITE_SECRET_REF = re.compile(r"secrets\.BAZEL_CACHE_WRITE\w*")
_CACHE_SECRET = re.compile(r"secrets\.(BAZEL_CACHE_\w*)")
#: `(?<!\.)` keeps a filename (`crates/ocx_shim/BUILD.bazel`) from reading as an invocation.
_BAZEL = re.compile(r"(?<!\.)\bbazel(?:isk)?\b", re.IGNORECASE)
_RUNNER_TEMP = re.compile(r"RUNNER_TEMP|runner\.temp", re.IGNORECASE)
#: Any spelling of the upload flag, `=false` and the `no` negation included: the rc the
#: action writes is the one place it is set.
_UPLOAD_FLAG = re.compile(r"--(?:no)?remote_upload_local_results")
#: A `${{ … }}` span. A flag inside one is conditional by construction.
_GITHUB_EXPRESSION = re.compile(r"\$\{\{.*?\}\}", re.DOTALL)
#: A `#` comment to end of line. A flag that survives this is one bazel is passed.
_SHELL_COMMENT = re.compile(r"(?m)(?:^|\s)#.*$")
_FLAG = re.compile(r"--[A-Za-z0-9_]+(?:=\S+)?")
#: `--config X`, the space-separated spelling `_FLAG` cannot pair up.
_SPACED_CONFIG = re.compile(r"--config\s+(\S+)")
#: One rc stanza: `build:ci --remote_cache=…`. `common` and `build` both reach a
#: `bazel test`, so all three command words are read.
_RC_STANZA = re.compile(r"(?m)^[ \t]*(build|common|test)(?::([\w.-]+))?[ \t]+(.*)$")
#: Where the write secret may be named: as a cache step's credential input (the repo action's
#: `write-auth`, the template setup action's `cache-write-auth`), or passed through by name to a
#: reusable workflow.
_WRITE_SECRET_SPOTS = frozenset(
    {("with", "write-auth"), ("with", "cache-write-auth"), ("secrets", WRITE_SECRET)}
)
#: The conventions template's setup action. Its `trusted` cache mode presents the write credential
#: on every event that carries it, which is `write-auth` under another name; it owns the credential's
#: lifetime itself, since a composite action has no `post:` hook and the template's jobs add no cleanup.
TEMPLATE_SETUP = "ocx-sh/conventions/.github/actions/setup"


# ---------------------------------------------------------------------------
# Readers
# ---------------------------------------------------------------------------


def _scalars(node: object, path: tuple[str, ...] = ()) -> Iterator[tuple[tuple[str, ...], str]]:
    """Every string leaf of a parsed document, with the key path that reaches it.

    A credential can be bound at workflow, job or step `env:`, or passed as an
    action's `with:` input; a sweep keyed on one placement misses the others.
    """
    if isinstance(node, dict):
        for key, value in node.items():
            yield from _scalars(value, (*path, str(key)))
    elif isinstance(node, list):
        for index, value in enumerate(node):
            yield from _scalars(value, (*path, str(index)))
    elif isinstance(node, str):
        yield path, node


def _flags(text: str) -> list[str]:
    """The `--flags` bazel is actually passed by `text`.

    A `#` comment (`# --keep_going is NOT set` contains the token) and a `${{ … }}`
    span (a flag GitHub may or may not expand is not one the lane always carries) are
    removed first; each is a way a match reads its own negation.
    """
    return _FLAG.findall(_SHELL_COMMENT.sub("", _GITHUB_EXPRESSION.sub(" ", text)))


def _flag_names(flags: Iterable[str]) -> set[str]:
    """Flag names without the dashes, the `=value`, or the `no` negation.

    `--noremote_local_fallback` and `--remote_local_fallback=true` are one flag under
    BZL-CACHE-26. The bare form is yielded beside the stripped one, so a real flag
    beginning `no` is never renamed into something else.
    """
    names: set[str] = set()
    for flag in flags:
        name = flag.lstrip("-").split("=", 1)[0]
        names.add(name)
        if name.startswith("no"):
            names.add(name[2:])
    return names


def _rc_flags(rc_texts: Iterable[str], configs: Iterable[str]) -> list[str]:
    """`build` / `common` / `test` flags from rc text that the lane's configs activate.

    Config activation is a fixpoint because one stanza may name another config.
    `import` / `try-import` are not followed: they pull in the gitignored
    `.bazelrc.user`, outside the tracked set, so a flag reachable only through one is
    reported absent, a loud red rather than a silent pass.
    """
    stanzas = [
        (config, _flags(body)) for text in rc_texts for _, config, body in _RC_STANZA.findall(text)
    ]
    active = set(configs)
    while True:
        selected = [
            flag for config, flags in stanzas if not config or config in active for flag in flags
        ]
        grown = active | {flag.split("=", 1)[1] for flag in selected if flag.startswith("--config=")}
        if grown == active:
            return selected
        active = grown


def _tracked_rc_files() -> list[Path]:
    """Every tracked file whose name carries `.bazelrc` — BZL-CACHE-04's own needle."""
    listed = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files"],
        capture_output=True,
        encoding="utf-8",
        check=True,
    ).stdout.splitlines()
    return [ROOT / entry for entry in listed if entry and ".bazelrc" in PurePosixPath(entry).name]


def _rc_texts() -> list[str]:
    return [path.read_text(encoding="utf-8") for path in _tracked_rc_files()]


def _documents(workflows: Iterable[Path] | None = None) -> list[tuple[str, dict]]:
    return [(workflow.name, _load(workflow)) for workflow in (workflows or WORKFLOWS)]


def _jobs(documents: list[tuple[str, dict]]) -> Iterator[tuple[str, dict, dict]]:
    """`(<workflow>:<job>, workflow, job)` for every job of every document."""
    for name, document in documents:
        for job_name, job in (document.get("jobs") or {}).items():
            yield f"{name}:{job_name}", document, job


def _is_lane_step(step: dict) -> bool:
    run = _SHELL_COMMENT.sub("", str(step.get("run", "")))
    return bool(_BAZEL.search(run)) or any(needle in run for needle in _LANE_NEEDLES)


def _lane_jobs(documents: list[tuple[str, dict]]) -> dict[str, tuple[dict, list[int]]]:
    """`{<workflow>:<job>: (job, indices of its lane steps)}` for every Bazel lane."""
    lanes = {}
    for where, _, job in _jobs(documents):
        indices = [i for i, step in enumerate(job.get("steps") or []) if _is_lane_step(step)]
        if indices:
            lanes[where] = (job, indices)
    return lanes


def _lane_steps(documents: list[tuple[str, dict]]) -> list[tuple[str, dict]]:
    return [
        (where, step)
        for where, (job, indices) in _lane_jobs(documents).items()
        for step in (job["steps"][i] for i in indices)
    ]


def _effective_flags(step: dict, rc_texts: Iterable[str]) -> list[str]:
    """The step's own flags plus the rc flags its `--config`s activate."""
    run = str(step.get("run", ""))
    command_line = _flags(run)
    configs = {flag.split("=", 1)[1] for flag in command_line if flag.startswith("--config=")}
    configs |= set(_SPACED_CONFIG.findall(_SHELL_COMMENT.sub("", run)))
    return [*command_line, *_rc_flags(rc_texts, configs)]


def _truthy_input(step: dict, name: str) -> bool:
    """A `with:` input YAML may have parsed as a bool, a string, or an expression."""
    return str((step.get("with") or {}).get(name)).strip().lower() == "true"


def _is_rc_step(step: dict) -> bool:
    return RC_ACTION in str(step.get("uses", ""))


def _rc_writes(step: dict) -> bool:
    return _is_rc_step(step) and not _truthy_input(step, "cleanup")


def _is_template_cache_step(step: dict) -> bool:
    """The template setup action in the mode that writes the cache rc. The same action called with only
    `report:` (the telemetry push) or in another mode writes nothing and is no cache step."""
    return (
        str(step.get("uses", "")).startswith(f"{TEMPLATE_SETUP}@")
        and str((step.get("with") or {}).get("bazel-cache", "")) == "trusted"
    )


def _cache_writer_auth(step: dict) -> str | None:
    """The credential a step hands a cache-rc writer, `None` when it writes no rc.

    The writer is `bazel-cache-rc` (`write-auth`) or the template setup action in `trusted` mode
    (`cache-write-auth`).
    """
    if _is_template_cache_step(step):
        return str((step.get("with") or {}).get("cache-write-auth", ""))
    return str((step.get("with") or {}).get("write-auth", "")) if _rc_writes(step) else None


def _cleanup_gap(steps: list[dict]) -> str | None:
    """Why the rc a job writes outlives it, `None` when an `if: always()` cleanup follows.

    No `post:` hook exists for a composite action, so deletion is the caller's step. It
    must follow the last step that uses the rc (an earlier one leaves later steps
    uncached) and run under `always()`, because the job a leftover credential matters
    on is the cancelled one.
    """
    last = max((i for i, s in enumerate(steps) if _is_lane_step(s) or _rc_writes(s)), default=-1)
    cleanups = [i for i, s in enumerate(steps) if _is_rc_step(s) and _truthy_input(s, "cleanup")]
    if not cleanups:
        return "no `cleanup: true` step"
    if not any(i > last and "always()" in str(steps[i].get("if", "")) for i in cleanups):
        return f"no `cleanup: true` step under `if: always()` after step {last}"
    return None


def _local_callee(job: dict) -> str | None:
    """The file name of the repo workflow a job calls with `uses: ./.github/workflows/<name>`."""
    uses = str(job.get("uses", ""))
    return uses.rsplit("/", 1)[1] if uses.startswith("./.github/workflows/") else None


def _declared_secrets(document: dict) -> dict:
    """The secrets a workflow declares under `on.workflow_call.secrets`."""
    triggers = document.get("on", document.get(True))
    called = triggers.get("workflow_call") if isinstance(triggers, dict) else None
    return (called or {}).get("secrets") or {}


def _holds_cache_step(by_name: dict[str, dict], name: str, seen: tuple[str, ...] = ()) -> bool:
    """Whether a workflow writes the cache rc itself or calls one that does.

    "Holds" is transitive: a workflow calling one that does is a path the secret must
    travel through too.
    """
    if name in seen or name not in by_name:
        return False
    for job in (by_name[name].get("jobs") or {}).values():
        if any(_cache_writer_auth(step) is not None for step in job.get("steps") or []):
            return True
        callee = _local_callee(job)
        if callee and _holds_cache_step(by_name, callee, (*seen, name)):
            return True
    return False


def _cache_workflow_callers(documents: list[tuple[str, dict]]) -> list[tuple[str, str]]:
    """`(<caller>:<job>, <callee>)` for every job calling a workflow that holds a cache step."""
    by_name = dict(documents)
    return [
        (where, callee)
        for where, _, job in _jobs(documents)
        if (callee := _local_callee(job)) and _holds_cache_step(by_name, callee)
    ]


def _call_edges(documents: list[tuple[str, dict]]) -> set[tuple[str, str]]:
    """`(caller file, callee file)` for every `uses: ./.github/workflows/<name>` job."""
    return {(where.split(":")[0], callee) for where, _, job in _jobs(documents) if (callee := _local_callee(job))}


# ---------------------------------------------------------------------------
# Sweeps — one function per property, each returning the findings it made
# ---------------------------------------------------------------------------


def _write_credential_findings(documents: list[tuple[str, dict]]) -> list[str]:
    """Every cache step passes the write secret exactly, and nothing reads a read one.

    "Exactly" means no event predicate, no fallback and no step-level `if:`: a gate would
    withhold the cache from lanes that should share it, and an `|| ''` is the shape a gate
    takes. The write secret is named only as a credential input or a by-name pass-through.
    """
    findings = []
    for where, _, job in _jobs(documents):
        for index, step in enumerate(job.get("steps") or []):
            auth = _cache_writer_auth(step)
            if auth is not None and " ".join(auth.split()) != WRITE_AUTH:
                findings.append(f"{where}[{index}] passes {auth.strip()!r}, not `{WRITE_AUTH}`")
            if auth is not None and "if" in step:
                findings.append(f"{where}[{index}] gates the cache credential with `if: {step['if']}`")
    for name, document in documents:
        for path, value in _scalars(document):
            if "read-auth" in path or "BAZEL_CACHE_READ_AUTH" in value:
                findings.append(f"{name}:{'.'.join(path)} names the retired read credential")
            if _WRITE_SECRET_REF.search(value) and path[-2:] not in _WRITE_SECRET_SPOTS:
                findings.append(f"{name}:{'.'.join(path)} binds the write secret outside `write-auth`")
    return findings


def _upload_flag_findings(documents: list[tuple[str, dict]]) -> list[str]:
    """No step passes `--remote_upload_local_results`, with any value.

    The rc file the action writes carries the grant, so a workflow that spells the flag
    either duplicates it or overrides it on one lane.
    """
    return [
        f"{name}:{'.'.join(path)} passes `{match}`"
        for name, document in documents
        for path, value in _scalars(document)
        for match in _UPLOAD_FLAG.findall(_SHELL_COMMENT.sub("", value) if path[-1] == "run" else value)
    ]


def _lanes_without_cache_step(
    documents: list[tuple[str, dict]], excluded: Iterable[str] = ()
) -> list[str]:
    """Every Bazel lane writes the cache before its first lane step and cleans up after.

    The cleanup half is the caller's duty for `bazel-cache-rc` only; the template's setup action
    in `trusted` mode has no cleanup to add (see `TEMPLATE_SETUP`).
    """
    findings = []
    for where, (job, lane) in _lane_jobs(documents).items():
        if where in excluded:
            continue
        steps = job.get("steps") or []
        writers = [i for i, step in enumerate(steps) if _cache_writer_auth(step) is not None]
        if not writers or writers[0] > lane[0]:
            findings.append(f"{where}: no cache write step before its first lane step ({lane[0]})")
        elif _is_rc_step(steps[writers[0]]) and (gap := _cleanup_gap(steps)):
            findings.append(f"{where}: {gap}")
    return findings


def _callers_without_the_secret(documents: list[tuple[str, dict]]) -> list[str]:
    """A caller passes, by name, each secret the workflow it calls requires, and the cache secret.

    A required secret left out fails the run at startup; the cache secret is optional
    but leaving it out runs the lane uncached and green, so it is owed whenever the
    callee declares it. Other optional secrets may be withheld on purpose (a dev publish
    announces nothing). `secrets: inherit` is a zizmor finding, so it is refused outright
    (the generated workflow aside). The called workflow must declare the cache secret or
    GitHub withholds it; both ends are checked, and the secret must be passed exactly
    (`WRITE_AUTH`), never gated. A call to a file that does not exist is a finding too.
    """
    by_name = dict(documents)
    findings = []
    for where, _, job in _jobs(documents):
        callee = _local_callee(job)
        caller = where.split(":")[0]
        if callee is None or caller in GENERATED_WORKFLOWS:
            continue
        if callee not in by_name:
            findings.append(f"{where} calls {callee}, which does not exist")
            continue
        passed = job.get("secrets")
        if passed == "inherit":
            findings.append(f"{where} calls {callee} with `secrets: inherit`")
            continue
        passed = passed if isinstance(passed, dict) else {}
        findings += [
            f"{where} calls {callee} without passing the secret {secret}"
            for secret, spec in _declared_secrets(by_name[callee]).items()
            if secret not in passed and (secret == WRITE_SECRET or (spec or {}).get("required") is True)
        ]
        if WRITE_SECRET in passed and " ".join(str(passed[WRITE_SECRET]).split()) != WRITE_AUTH:
            findings.append(f"{where} passes {WRITE_SECRET} as {str(passed[WRITE_SECRET]).strip()!r}, not `{WRITE_AUTH}`")
    called = {callee for _, _, job in _jobs(documents) if (callee := _local_callee(job))}
    for name in sorted(((called | set(REPO_OWNED_REUSABLE)) & set(by_name)) - TEMPLATE_OWNED_WORKFLOWS):
        if _holds_cache_step(by_name, name) and WRITE_SECRET not in _declared_secrets(by_name[name]):
            findings.append(f"{name} holds a cache step but declares no `workflow_call` secret {WRITE_SECRET}")
    return sorted(set(findings))


def _rc_writers_without_cleanup(documents: list[tuple[str, dict]]) -> list[str]:
    """The caller's half of the credential cleanup: a job that writes the rc through the action also deletes it."""
    return [
        f"{where}: {gap}"
        for where, _, job in _jobs(documents)
        if any(_rc_writes(step) for step in job.get("steps") or [])
        and (gap := _cleanup_gap(job["steps"]))
    ]


def _lanes_with_an_outage_policy_flag(
    documents: list[tuple[str, dict]], rc_texts: Iterable[str] = ()
) -> list[str]:
    """BZL-CACHE-26: no lane may make a cache outage fatal, or pretend to."""
    rc_texts = list(rc_texts)
    return [
        f"{where}: `{flag}`"
        for where, step in _lane_steps(documents)
        for flag in _effective_flags(step, rc_texts)
        if _flag_names([flag]) & OUTAGE_POLICY_FLAGS
    ]


def _lanes_without_remote_cache_reach(
    documents: list[tuple[str, dict]], rc_texts: Iterable[str] = ()
) -> list[str]:
    """The structural half of the remote-hit check: a lane must actually point at the shared cache.

    A lane with no `--remote_cache` is green with no cache reach at all, silently. The
    host is pinned too, and an explicit `https://` is required, because both remote
    flags default to `grpcs` on a scheme-less URI and would dial a port nothing serves.
    """
    rc_texts = list(rc_texts)
    findings = []
    for where, step in _lane_steps(documents):
        endpoints = [
            flag.split("=", 1)[1].strip("\"'")
            for flag in _effective_flags(step, rc_texts)
            if flag.startswith("--remote_cache=")
        ]
        if not endpoints:
            findings.append(f"{where}: no `--remote_cache=` in its effective flags")
        wrong = [e for e in endpoints if urlsplit(e).hostname != CACHE_HOST]
        if wrong:
            findings.append(f"{where}: `--remote_cache` points at {wrong}, not https://{CACHE_HOST}")
    return findings


def _runner_temp_uploads(documents: list[tuple[str, dict]]) -> list[str]:
    """ADR ruling 4b line 4: no upload step may take a path under `$RUNNER_TEMP`.

    That directory holds the rc file and the credential helper, and an uploaded
    artifact is a download for anyone who can read the run.
    """
    return [
        f"{where} step `{step.get('uses')}` path={value.strip()!r}"
        for where, _, job in _jobs(documents)
        for step in (job.get("steps") or [])
        if "upload" in str(step.get("uses", ""))
        for _, value in _scalars(step.get("with") or {})
        if _RUNNER_TEMP.search(value)
    ]


def _interpolated_credentials(documents: list[tuple[str, dict]]) -> list[str]:
    """ADR ruling 4b line 1: a cache secret reaches a `run:` body through `env:` only.

    GitHub materialises every step script to a file under `_temp/`, so the
    `${{ secrets.X }}` spelling writes the credential to a second on-disk location.
    """
    return [
        f"{name}:{'.'.join(path)} interpolates {sorted(set(_CACHE_SECRET.findall(value)))}"
        for name, document in documents
        for path, value in _scalars(document)
        if path and path[-1] == "run" and _CACHE_SECRET.search(value)
    ]


_PACKAGE_FETCH = re.compile(r"\bpackage\s+(?:env|pull|install|exec)\s+([^\n]*)")


def _unpinned_package_fetches(documents: list[tuple[str, dict]]) -> list[str]:
    """A job holding the cache credential fetches each ocx package by digest.

    Whatever such a job fetches runs beside the rc and the helper the credential lives in, so
    a tag that moves must not be able to change it.
    """
    findings = []
    for where, _, job in _jobs(documents):
        steps = job.get("steps") or []
        if not any(_cache_writer_auth(step) is not None for step in steps):
            continue
        for index, step in enumerate(steps):
            run = _SHELL_COMMENT.sub("", str(step.get("run", ""))).replace("\\\n", " ")
            for fetch in _PACKAGE_FETCH.finditer(run):
                for token in fetch[1].split():
                    if token == "--":
                        break
                    if not token.startswith("-") and "@sha256:" not in token:
                        findings.append(f"{where}[{index}] fetches {token!r} without a digest")
    return findings


def _rc_files_configuring_a_credential_helper(rc_files: Iterable[Path]) -> list[str]:
    """BZL-CACHE-04 (MUST): `--credential_helper` never from a shipped rc file.

    Bazel spawns the helper with the full client environment, before the sandbox, with
    no check on where the flag came from: a read-only `bazel query //...` on a fresh
    clone runs it (bazel#30439, closed as intended). The helper belongs in
    `$RUNNER_TEMP/bazel-cache.rc`, written by the action.
    """
    return [
        f"{path}:{number}: {line.strip()}"
        for path in rc_files
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1)
        if "credential_helper" in _SHELL_COMMENT.sub("", line)
    ]


# ---------------------------------------------------------------------------
# Reader red states
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("run", "expected"),
    [
        pytest.param("bazel test //... --keep_going", ["--keep_going"], id="passed"),
        pytest.param("bazel test //...  # --keep_going is NOT set", [], id="in-a-comment"),
        pytest.param(
            "bazel test //... ${{ github.ref == 'refs/heads/main' && '' || '--keep_going' }}",
            [],
            id="in-an-expression",
        ),
        pytest.param(
            f"bazel test //... \\\n  --remote_cache=https://{CACHE_HOST} \\\n  --keep_going",
            [f"--remote_cache=https://{CACHE_HOST}", "--keep_going"],
            id="line-continued",
        ),
    ],
)
def test_the_flag_reader_reads_past_a_comment_and_an_expression(
    run: str, expected: list[str]
) -> None:
    """The substring trap, pinned: `"--keep_going" in run` is true for three of these four."""
    assert _flags(run) == expected


@pytest.mark.parametrize(
    ("flags", "expected"),
    [
        pytest.param(["--remote_local_fallback"], {"remote_local_fallback"}, id="bare"),
        pytest.param(["--noremote_local_fallback"], {"remote_local_fallback"}, id="negated"),
        pytest.param(["--remote_local_fallback=true"], {"remote_local_fallback"}, id="valued"),
        pytest.param([f"--remote_cache=https://{CACHE_HOST}"], set(), id="unrelated"),
    ],
)
def test_the_flag_name_reader_finds_the_outage_flag(flags: list[str], expected: set[str]) -> None:
    assert _flag_names(flags) & OUTAGE_POLICY_FLAGS == expected


@pytest.mark.parametrize(
    ("configs", "expected"),
    [
        pytest.param([], ["--keep_going"], id="unconditional-only"),
        pytest.param(["ci"], ["--keep_going", f"--remote_cache=https://{CACHE_HOST}"], id="active"),
        pytest.param(
            ["dev"],
            ["--keep_going", "--config=ci", f"--remote_cache=https://{CACHE_HOST}"],
            id="chained",
        ),
    ],
)
def test_the_rc_reader_follows_an_active_config(configs: list[str], expected: list[str]) -> None:
    """A flag under `build:ci` is not in effect until the lane passes `--config=ci`; a
    `build:dev` stanza naming `--config=ci` pulls it in transitively. The commented
    stanza is never in effect, and the `query:` one never reaches a `bazel test`."""
    rc = "\n".join(
        [
            "build --keep_going",
            "# build --remote_cache=https://commented.example",
            f"build:ci --remote_cache=https://{CACHE_HOST}",
            "build:dev --config=ci",
            "query:ci --verbose_failures",
        ]
    )
    assert sorted(_rc_flags([rc], configs)) == sorted(expected)


def test_the_credential_helper_sweep_reads_an_rc_file(tmp_path: Path) -> None:
    """BZL-CACHE-04's sweep, both halves, on rc files this test writes."""
    clean = tmp_path / ".bazelrc"
    clean.write_text(
        f"build --remote_cache=https://{CACHE_HOST}\n# build --credential_helper=x\n",
        encoding="utf-8",
    )
    assert not _rc_files_configuring_a_credential_helper([clean])

    shipped = tmp_path / ".bazelrc.ci"
    shipped.write_text(
        f"build --credential_helper={CACHE_HOST}=%workspace%/tools/helper\n", encoding="utf-8"
    )
    findings = _rc_files_configuring_a_credential_helper([shipped])
    assert findings and "credential_helper" in findings[0]


# ---------------------------------------------------------------------------
# Every sweep's red and green, shown on every run
#
# The corpus is a lane workflow and its caller; each violation
# is the same corpus built with one property changed, rather than patched with
# `str.replace` afterwards, so no needle can drift and leave the compliant tree
# standing in for a mutation.
# ---------------------------------------------------------------------------

_RC_STEP = "      - uses: ./.github/actions/bazel-cache-rc\n"
_CLEANUP = f"{_RC_STEP}        if: always()\n        with:\n          cleanup: 'true'\n"
_UPLOAD = "      - uses: actions/upload-artifact@v4\n        with:\n          path: target/x.xml\n"
_FIXTURE_RC = f"build --remote_cache=https://{CACHE_HOST}\n"


def _write(auth: str = WRITE_AUTH, extra: str = "") -> str:
    return f"{_RC_STEP}        with:\n          write-auth: {auth}\n{extra}"


def _run(command: str = "task bazel:test:unit") -> str:
    return f"      - run: {command}\n"


def _caller(secrets: str = f"    secrets:\n      {WRITE_SECRET}: {WRITE_AUTH}\n") -> str:
    return (
        "name: caller\non:\n  workflow_dispatch: {}\njobs:\n  deep:\n"
        f"    uses: ./.github/workflows/lane.yml\n{secrets}"
    )


def _corpus(
    *steps: str,
    caller: str | None = None,
    declared: bool = True,
    also_declares: tuple[str, bool] | None = None,
) -> list[tuple[str, dict]]:
    steps = steps or (_write(), _run(), _UPLOAD, _CLEANUP)
    extra = f"      {also_declares[0]}:\n        required: {str(also_declares[1]).lower()}\n" if also_declares else ""
    declaration = f"  workflow_call:\n    secrets:\n      {WRITE_SECRET}:\n        required: false\n{extra}"
    lane = (
        f"name: lane\non:\n{declaration if declared else '  workflow_call: {}\n'}"
        "jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n"
        f"{''.join(steps)}"
    )
    return [("lane.yml", yaml.safe_load(lane)), ("caller.yml", yaml.safe_load(caller or _caller()))]


def _template_write(auth: str = WRITE_AUTH, mode: str = "trusted", extra: str = "") -> str:
    return (
        f"      - uses: {TEMPLATE_SETUP}@9a68818180c78cedda0999ffbc384b5a9e75543a # v0.3.0\n"
        f"        with:\n          bazel-cache: {mode}\n          cache-write-auth: {auth}\n{extra}"
    )


_SWEEPS = {
    "write-credential": _write_credential_findings,
    "no-upload-flag": _upload_flag_findings,
    "lane-cache-step": _lanes_without_cache_step,
    "caller-secret": _callers_without_the_secret,
    "rc-writer-cleanup": _rc_writers_without_cleanup,
    "outage-policy": lambda docs: _lanes_with_an_outage_policy_flag(docs, [_FIXTURE_RC]),
    "remote-cache-reach": lambda docs: _lanes_without_remote_cache_reach(docs, [_FIXTURE_RC]),
    "runner-temp-upload": _runner_temp_uploads,
    "interpolated-credential": _interpolated_credentials,
    "unpinned-package": _unpinned_package_fetches,
}

_EVENT_GATE = "${{ github.ref == 'refs/heads/main' && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}"
_VIOLATIONS = [
    pytest.param("write-credential", _corpus(_write(auth=_EVENT_GATE), _run(), _CLEANUP), id="a-event-predicate"),
    pytest.param("write-credential", _corpus(_write(auth="''"), _run(), _CLEANUP), id="a-empty-credential"),
    pytest.param(
        "write-credential",
        _corpus(_write(extra="        if: github.ref == 'refs/heads/main'\n"), _run(), _CLEANUP),
        id="a-step-level-if",
    ),
    pytest.param(
        "write-credential",
        _corpus(_write(extra="          read-auth: ${{ secrets.BAZEL_CACHE_READ_AUTH }}\n"), _run(), _CLEANUP),
        id="a-read-credential-beside-it",
    ),
    pytest.param(
        "write-credential",
        _corpus(_write(), f"{_run()}        env:\n          T: {WRITE_AUTH}\n", _CLEANUP),
        id="a-write-secret-bound-in-env",
    ),
    pytest.param("no-upload-flag", _corpus(_write(), _run("task bazel:test:unit -- --remote_upload_local_results=true"), _CLEANUP), id="b-run-body"),
    pytest.param("no-upload-flag", _corpus(_write(), _run("task bazel:test:unit -- --remote_upload_local_results=false"), _CLEANUP), id="b-run-body-false"),
    pytest.param(
        "no-upload-flag",
        _corpus(_write(), f"{_UPLOAD}          args: --noremote_upload_local_results\n", _CLEANUP),
        id="b-with-input",
    ),
    pytest.param("lane-cache-step", _corpus(_run(), _CLEANUP), id="c-no-write-step"),
    pytest.param("lane-cache-step", _corpus(_run(), _write(), _CLEANUP), id="c-write-step-after-the-lane"),
    pytest.param("lane-cache-step", _corpus(_run("task cross:build TARGET=x")), id="c-lane-found-by-task-name"),
    pytest.param("lane-cache-step", _corpus(_write(), _run()), id="c-no-cleanup"),
    pytest.param("caller-secret", _corpus(caller=_caller(secrets="    secrets: inherit\n")), id="d-inherit"),
    pytest.param("caller-secret", _corpus(caller=_caller(secrets="")), id="d-no-secrets"),
    pytest.param(
        "caller-secret",
        _corpus(caller=_caller(secrets=f"    secrets:\n      {WRITE_SECRET}: ${{{{ secrets.OTHER }}}}\n")),
        id="d-another-secret-passed",
    ),
    pytest.param("caller-secret", _corpus(declared=False), id="d-callee-declares-nothing"),
    pytest.param("caller-secret", _corpus(also_declares=("OTHER_SECRET", True)), id="d-required-secret-not-passed"),
    pytest.param(
        "caller-secret",
        [("caller.yml", yaml.safe_load(_caller().replace("lane.yml", "absent.yml")))],
        id="d-callee-does-not-exist",
    ),
    pytest.param("rc-writer-cleanup", _corpus(_write(), _run(), _CLEANUP.replace("always()", "success()")), id="cleanup-not-under-always"),
    pytest.param("rc-writer-cleanup", _corpus(_write(), _CLEANUP, _run()), id="cleanup-before-the-last-lane-step"),
    pytest.param("rc-writer-cleanup", _corpus(_write(), _run()), id="cleanup-absent"),
    pytest.param("outage-policy", _corpus(_write(), _run("task bazel:test:unit -- --remote_local_fallback=true"), _CLEANUP), id="outage-fallback-flag"),
    pytest.param("outage-policy", _corpus(_write(), _run("task bazel:test:unit -- --experimental_remote_require_cached"), _CLEANUP), id="outage-made-fatal"),
    pytest.param("remote-cache-reach", _corpus(_write(), _run("bazel test //... --remote_cache=https://elsewhere.example"), _CLEANUP), id="reach-wrong-host"),
    pytest.param("remote-cache-reach", _corpus(_write(), _run(f"bazel test //... --remote_cache={CACHE_HOST}"), _CLEANUP), id="reach-scheme-less"),
    pytest.param(
        "runner-temp-upload",
        _corpus(_write(), _run(), "      - uses: actions/upload-artifact@v4\n        with:\n          path: ${{ runner.temp }}/bep.json\n", _CLEANUP),
        id="runner-temp-upload",
    ),
    pytest.param(
        "interpolated-credential",
        _corpus(_write(), _run(f"echo '{WRITE_AUTH}' >> .bazelrc.user"), _CLEANUP),
        id="interpolated-credential",
    ),
    pytest.param(
        "unpinned-package",
        _corpus(_write(), _run('eval "$(ocx --remote package env nushell/nushell:0.116.1 --shell=bash)"'), _CLEANUP),
        id="unpinned-package-by-tag",
    ),
    pytest.param(
        "unpinned-package",
        _corpus(_write(), f'      - run: |\n          ocx package env a/b:1@sha256:{"0" * 64} \\\n            c/d:2 --shell=bash\n', _CLEANUP),
        id="unpinned-package-after-a-continuation",
    ),
]


@pytest.mark.parametrize("sweep", sorted(_SWEEPS))
def test_every_sweep_passes_the_compliant_corpus(sweep: str) -> None:
    """The green half. A compliant corpus some sweep reports is a sweep whose red below
    proves nothing."""
    corpus = _corpus()
    assert _lane_jobs(corpus) and _cache_workflow_callers(corpus), (
        "the corpus fixture has no lane or no caller — every sweep reading it reads nothing"
    )
    findings = _SWEEPS[sweep](corpus)
    assert not findings, f"`{sweep}` reports the compliant corpus: {findings}"


@pytest.mark.parametrize(("sweep", "corpus"), _VIOLATIONS)
def test_every_sweep_reds_its_violating_corpus(sweep: str, corpus: list[tuple[str, dict]]) -> None:
    """The red half, and the proof that the mutation landed: a corpus equal to the
    compliant one would leave the sweep correctly finding nothing."""
    assert corpus != _corpus(), f"the `{sweep}` violating corpus mutates nothing"
    assert _SWEEPS[sweep](corpus), f"`{sweep}` found nothing in a corpus built to violate it"


@pytest.mark.parametrize(
    ("steps", "sweep", "findings"),
    [
        pytest.param((_template_write(), _run()), "lane-cache-step", False, id="trusted-step-before-the-lane-needs-no-cleanup"),
        pytest.param((_template_write(), _run()), "write-credential", False, id="trusted-step-passing-the-secret-by-name"),
        pytest.param((_template_write(auth=_EVENT_GATE), _run()), "write-credential", True, id="trusted-step-with-an-event-predicate"),
        pytest.param(
            (_template_write(extra="        if: github.event_name == 'push'\n"), _run()),
            "write-credential",
            True,
            id="trusted-step-with-a-step-level-if",
        ),
        pytest.param((_run(), _template_write()), "lane-cache-step", True, id="trusted-step-after-the-lane"),
        pytest.param((_template_write(mode="read"), _run()), "lane-cache-step", True, id="read-mode-writes-no-rc"),
        pytest.param((_write(), _run()), "lane-cache-step", True, id="the-repo-action-still-owes-its-cleanup"),
    ],
)
def test_the_template_setup_step_is_a_cache_writer_in_trusted_mode_only(
    steps: tuple[str, ...], sweep: str, findings: bool
) -> None:
    """Red and green for the template's `bazel-cache: trusted` step as a cache writer."""
    assert bool(_SWEEPS[sweep](_corpus(*steps))) is findings


def test_a_pinned_package_fetch_is_green() -> None:
    run = _run(f'eval "$(ocx --remote package env a/b:1@sha256:{"0" * 64} --shell=bash)"')
    assert not _unpinned_package_fetches(_corpus(_write(), run, _CLEANUP))


def test_a_named_exclusion_skips_its_lane() -> None:
    corpus = _corpus(_run(), _CLEANUP)
    assert _lanes_without_cache_step(corpus)
    assert not _lanes_without_cache_step(corpus, {"lane.yml:build": "no Bazel"})


# ---------------------------------------------------------------------------
# The live assertions
# ---------------------------------------------------------------------------


def test_every_cache_step_passes_the_write_secret_and_nothing_else() -> None:
    """Floored on its reader: a tree with no cache step is not a clean one."""
    documents = _documents()
    assert any(
        _cache_writer_auth(step) is not None
        for _, _, job in _jobs(documents)
        for step in job.get("steps") or []
    ), f"no workflow job writes a cache rc through `{RC_ACTION}` — the reader found nothing"
    findings = _write_credential_findings(documents)
    assert not findings, (
        f"cache steps that do not pass `{WRITE_AUTH}` exactly, or workflows naming the retired "
        f"read credential: {findings}. Every same-repo lane holding the secret writes the "
        f"cache; a fork PR holds no secrets, so the empty binding is the fork lane and needs "
        f"no predicate"
    )


def test_no_step_passes_the_upload_flag() -> None:
    """The rc the action writes carries `--remote_upload_local_results=true`."""
    findings = _upload_flag_findings(_documents())
    assert not findings, (
        f"steps passing the upload flag: {findings}. Each task passes the action's rc with "
        f"`--bazelrc` when it exists; a second spelling in a workflow overrides it on one lane"
    )


def test_no_job_holding_the_cache_credential_fetches_an_unpinned_package() -> None:
    """Floored on its reader: at least one credentialed job fetches a package."""
    documents = _documents()
    fetches = [
        fetch
        for _, _, job in _jobs(documents)
        if any(_cache_writer_auth(step) is not None for step in job.get("steps") or [])
        for step in job.get("steps") or []
        for fetch in _PACKAGE_FETCH.finditer(str(step.get("run", "")))
    ]
    assert fetches, "no credentialed job runs `ocx package env|pull|install|exec` — the reader found nothing"
    findings = _unpinned_package_fetches(documents)
    assert not findings, (
        f"{findings}. The job also holds the Bazel cache write credential; pin each package to "
        f"`<tag>@sha256:<index digest>` (see the comment in basic-repo.yml)"
    )


def test_the_lane_reader_is_not_asleep() -> None:
    """The floor under every lane assertion, so it fails when the reader stops matching."""
    lanes = _lane_jobs(_documents())
    assert len(lanes) >= BAZEL_LANE_FLOOR, (
        f"found {len(lanes)} Bazel lane jobs, expected at least {BAZEL_LANE_FLOOR}: "
        f"{sorted(lanes)} — `_LANE_NEEDLES` no longer match how the workflows spell a lane"
    )


def test_every_lane_exclusion_names_a_lane() -> None:
    stale = sorted(set(LANE_EXCLUSIONS) - set(_lane_jobs(_documents())))
    assert not stale, f"exclusions for jobs that are no longer detected as lanes: {stale}"


def test_every_bazel_lane_has_the_cache_step() -> None:
    """Without the step a lane compiles cold and the cache never gains an entry."""
    findings = _lanes_without_cache_step(_documents(), LANE_EXCLUSIONS)
    assert not findings, (
        f"Bazel lanes without a cache write step before their first lane step, or without "
        f"an `if: always()` cleanup after it: {findings}. A lane that provably reaches no "
        f"Bazel belongs in `LANE_EXCLUSIONS` with its reason; the credential is not "
        f"granted to a job that cannot use it"
    )


def test_every_caller_of_a_reusable_workflow_passes_its_declared_secrets_by_name() -> None:
    """Secrets do not cross `workflow_call` unless declared and passed.

    Every job calling `./.github/workflows/<name>.yml` passes by name each secret that
    file requires, and the cache secret; no workflow inherits. A repo-owned reusable
    workflow that holds a cache step declares `BAZEL_CACHE_WRITE_AUTH`; the two
    template-owned workflows are the baseline check's to hold on the declare side.
    """
    documents = _documents()
    edges = _call_edges(documents)
    assert len(edges) >= len(REQUIRED_CALL_EDGES), (
        f"found {len(edges)} reusable-workflow call edges, expected at least "
        f"{len(REQUIRED_CALL_EDGES)}: {sorted(edges)} — the reader stopped matching `uses:`"
    )
    missing = sorted(REQUIRED_CALL_EDGES - edges)
    assert not missing, (
        f"call edges the tier shape needs and the workflows lack (caller, callee): {missing}. "
        f"The template's `repo` jobs call `basic-repo.yml` and `deep-repo.yml`; those call "
        f"`acceptance.yml` and `cross.yml`"
    )
    findings = _callers_without_the_secret(documents)
    assert not findings, (
        f"{findings}. Without both ends the called workflow's cache step sees an empty "
        f"secret and runs uncached, green at full cost"
    )


def test_an_optional_secret_may_be_left_unpassed() -> None:
    """The green control for the required-secret finding: `required: false` is a choice."""
    assert not _callers_without_the_secret(_corpus(also_declares=("OPTIONAL_SECRET", False)))


def test_the_generated_workflow_exemption_is_still_needed() -> None:
    """An exemption that outlives its reason hides a hand-written `secrets: inherit`."""
    for name in GENERATED_WORKFLOWS:
        jobs = (_load(WORKFLOW_DIR / name).get("jobs") or {}).values()
        assert any(job.get("secrets") == "inherit" for job in jobs), (
            f"{name} no longer passes `secrets: inherit` — drop it from GENERATED_WORKFLOWS "
            f"so the sweep covers it"
        )


def test_a_template_owned_callee_is_exempt_from_the_declare_half_only() -> None:
    """The exemption skips `verify-deep.yml` as a callee that declares nothing, and nothing more."""
    lane = _corpus(declared=False)[0][1]
    exempt = [
        ("verify-deep.yml", lane),
        ("caller.yml", yaml.safe_load(_caller().replace("lane.yml", "verify-deep.yml"))),
    ]
    assert not _callers_without_the_secret(exempt)
    assert _callers_without_the_secret(_corpus(declared=False)), (
        "the same callee under a repo-owned name must still be a finding"
    )
    inherits = [("verify-deep.yml", lane), ("caller.yml", yaml.safe_load(_caller(secrets="    secrets: inherit\n").replace("lane.yml", "verify-deep.yml")))]
    assert _callers_without_the_secret(inherits), "an exempt callee does not exempt its caller from `inherit`"


def test_every_write_credential_caller_deletes_it_in_an_always_step() -> None:
    """The caller's half of the credential cleanup: `using: composite` has no `post:` hook, so the deletion is a
    second `uses:` of the action under `if: always()`, after the last step needing the rc."""
    findings = _rc_writers_without_cleanup(_documents())
    assert not findings, (
        f"jobs that write the cache rc and do not delete it: {findings}. "
        f"`$RUNNER_TEMP/bazel-cache.rc` and its helper survive the job unless the caller runs "
        f"`{RC_ACTION}` again with `cleanup: 'true'` under `if: always()`; `success()` is not "
        f"enough, because the job a leftover credential matters on is the cancelled one"
    )


def test_no_bazel_lane_makes_a_cache_outage_fatal() -> None:
    """An unreachable cache host is a warning and exit 0, by construction.

    With no `--remote_executor` an outage is already non-fatal on every Bazel version
    measured. What can go wrong is a lane citing a fallback flag as though it governed
    this (BZL-CACHE-26), or requiring a cache hit outright.
    """
    findings = _lanes_with_an_outage_policy_flag(_documents(), _rc_texts())
    assert not findings, (
        f"lanes citing a flag that does not govern a cache-only outage, or that makes one "
        f"fatal: {findings}. Measured on 8.7.0 and 9.2.0, all four fallback-flag combinations "
        f"give `WARNING: Remote Cache: Connection refused`, every action local, exit 0"
    )


def test_a_bazel_lane_reaches_the_remote_cache() -> None:
    """The structural half of the remote-hit check — a lane pointing nowhere is the silent zero-hit lane.

    The **hit count** is not derivable from configuration; what counts as a hit is
    `remote_cache_hits`, and `test_s016_is_not_satisfiable_by_a_disk_cache_hit` is its
    red and green.
    """
    documents = _documents()
    assert _lane_steps(documents), "no lane step found — the reader stopped matching"
    findings = _lanes_without_remote_cache_reach(documents, _rc_texts())
    assert not findings, (
        f"Bazel lanes with no reach to the shared cache: {findings}. A lane that reads no "
        f"cache is green forever at full cost; a scheme-less URI is worse than none, since "
        f"both remote flags default to `grpcs` and would dial a port nothing serves"
    )


@pytest.mark.parametrize(
    "rc_file", _tracked_rc_files(), ids=lambda path: path.relative_to(ROOT).as_posix()
)
def test_no_tracked_rc_file_configures_a_credential_helper(rc_file: Path) -> None:
    """BZL-CACHE-04 (MUST) / ADR ruling 4a — the helper is never in a shipped rc file.

    Parametrized over the tracked set rather than looped: an empty set is a visible
    skip, a loop would report the same green as a clean tree.
    """
    findings = _rc_files_configuring_a_credential_helper([rc_file])
    assert not findings, (
        f"a tracked rc file configures `--credential_helper`: {findings}. Bazel spawns that "
        f"program with the full client environment, before the sandbox, on a read-only "
        f"`bazel query //...` from a fresh clone (bazel#30439). The helper belongs in "
        f"`$RUNNER_TEMP/bazel-cache.rc`, written by `{RC_ACTION}`"
    )


def test_no_upload_step_takes_a_runner_temp_path() -> None:
    """ADR ruling 4b line 4. Live over the whole workflow corpus, not only Bazel lanes.

    Floored on its reader: no upload step found means the sweep stopped reading.
    """
    documents = _documents()
    assert any(
        "upload" in str(step.get("uses", ""))
        for _, _, job in _jobs(documents)
        for step in job.get("steps") or []
    ), "no workflow step uses an upload action — the reader found nothing"
    findings = _runner_temp_uploads(documents)
    assert not findings, (
        f"upload steps taking a path under `$RUNNER_TEMP`: {findings}. That directory holds "
        f"the rc file and the credential helper, whose whole job is to print the credential"
    )


def test_no_run_body_interpolates_a_cache_credential() -> None:
    """ADR ruling 4b line 1 — cache secrets reach a `run:` body through `env:` only.

    Floored on its reader: the corpus must reference `secrets.` somewhere for a clean
    `run:` sweep to mean anything.
    """
    documents = _documents()
    assert any(
        "secrets." in value for _, document in documents for _, value in _scalars(document)
    ), "no workflow references `secrets.` at all — the reader drifted"
    findings = _interpolated_credentials(documents)
    assert not findings, (
        f"`run:` bodies interpolating a cache credential: {findings}. GitHub materialises "
        f"every step script to a file under `_temp/`, so the `${{{{ secrets.X }}}}` spelling "
        f"writes the credential to a second on-disk location. Pass it as a step `env:` entry"
    )


# ---------------------------------------------------------------------------
# S-016's evidence predicate — what a remote cache hit is, and what it is not
# ---------------------------------------------------------------------------

#: `executionInfo.strategy` on a cached `TestResult` is the name of the runner
#: that served it, and the runner names the tier. Bazel's spellings, not ours.
REMOTE_CACHE_RUNNER = "remote cache hit"
DISK_CACHE_RUNNER = "disk cache hit"

#: Four real bazel 9.2.0 invocations of one workspace, appended into one
#: stream. Verbatim lines, so each invocation's `started.optionsDescription`
#: still records the flags that produced it — which is how the runs below are
#: *selected* rather than assumed. `scripts/bep_to_otlp.py` reads the same file.
CACHE_STATES_BEP = ROOT / "test" / "fixtures" / "bep" / "cache_states.json"

_REMOTE_CACHE_FLAG = re.compile(r"--remote_cache=(\S*)")


def _cache_state_runs() -> list[tuple[str, list[dict]]]:
    """`(optionsDescription, [testResult payload, …])` per invocation in the BEP.

    Split on `started` exactly as Bazel appends: one `--build_event_json_file`
    can hold several streams, and a reader keyed on "the" invocation would read
    one of the four.
    """
    runs: list[tuple[str, list[dict]]] = []
    for line in CACHE_STATES_BEP.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        event = json.loads(line)
        identity = event.get("id") or {}
        if "started" in identity:
            runs.append((str((event.get("started") or {}).get("optionsDescription", "")), []))
        elif "testResult" in identity and runs:
            runs[-1][1].append(event.get("testResult") or {})
    return runs


def _remote_cache_flag(options: str) -> str:
    """The `--remote_cache=` value this invocation actually ran with, unquoted."""
    match = _REMOTE_CACHE_FLAG.search(options)
    return match.group(1).strip("\"'") if match else ""


def remote_cache_hits(payloads: list[dict]) -> list[int]:
    """S-016's predicate: the indices served by the **remote** cache.

    Keyed on `executionInfo.strategy`, and on nothing else. The two fields whose
    names answer this question cannot carry it, both measured on 9.2.0 over the
    runs in `CACHE_STATES_BEP`:

    * `executionInfo.cachedRemotely` is **true for a `--disk_cache` hit** — the
      disk cache is classified as a remote tier. `.bazelrc` puts `--disk_cache`
      on *every* invocation in this repository, so a lane asserting "nonzero
      remote cache hits" through that field asserts it with the remote cache
      switched off, and its red half ("remove the credential → 0 hits") is
      unreachable.
    * `cachedLocally` is true **only** for a live bazel server's in-memory
      action cache, and false for every disk hit — the CI-shaped warm state. It
      reports nothing cached on a run where everything was.
    """
    return [
        index
        for index, payload in enumerate(payloads)
        if (payload.get("executionInfo") or {}).get("strategy") == REMOTE_CACHE_RUNNER
    ]


def cached_remotely_hits(payloads: list[dict]) -> list[int]:
    """The wrong predicate, kept as a named control and used nowhere else.

    This is the spelling the plan's own S-016 row is phrased in, and the one
    anyone writes first. `test_s016_is_not_satisfiable_by_a_disk_cache_hit`
    shows it answering the same on a run served entirely by the disk cache as
    on a run served entirely by the remote one.
    """
    return [
        index
        for index, payload in enumerate(payloads)
        if (payload.get("executionInfo") or {}).get("cachedRemotely") is True
    ]


def test_s016_is_not_satisfiable_by_a_disk_cache_hit() -> None:
    """S-016, restated — and the false green it replaces, shown on the same bytes.

    The plan's S-016 reads *"warm cache → nonzero remote cache hits; red half: remove the
    credential → 0 hits"*. Its subject is the value proposition of the whole remote cache,
    and WP-30 measured that as phrased it is satisfiable with the remote cache off.

    Neither run below is selected by position or by name. Each is selected by the
    `--remote_cache=` value **it recorded in its own `optionsDescription`**, so the
    surprising half — a run with no remote cache reporting `cachedRemotely: true` on every
    target — is evidence rather than an assertion about Bazel.

    This is S-016's predicate, not S-016's live half: a lane whose BEP can be read is
    WP-23/WP-24's, and WP-30 returned NO-GO, so no such lane lands. What is fixed here is
    that the predicate is now correct *when* one does, and cannot be written the wrong way
    without this test going red.
    """
    runs = _cache_state_runs()
    assert len(runs) == 4, f"{CACHE_STATES_BEP} must hold 4 invocations, got {len(runs)}"

    no_remote = [
        payloads
        for options, payloads in runs
        if payloads and not _remote_cache_flag(options) and cached_remotely_hits(payloads)
    ]
    assert len(no_remote) == 1, (
        f"expected exactly one run that ran with `--remote_cache=` empty and still reports "
        f"`cachedRemotely` on its targets, got {len(no_remote)} — the fixture no longer "
        f"carries the disk-cache-hit state S-016's red half has to survive"
    )
    disk = no_remote[0]

    # The red half, and it is the whole point: with the remote cache switched off,
    # S-016's restated predicate finds nothing, while the plan's finds everything.
    assert remote_cache_hits(disk) == [], (
        f"{len(remote_cache_hits(disk))} of {len(disk)} targets read as remote cache hits on "
        f"a run whose own optionsDescription records `--remote_cache=''`. S-016's red half "
        f"is then unreachable and the value proposition is unasserted"
    )
    assert cached_remotely_hits(disk) == list(range(len(disk))), (
        f"the control must call all {len(disk)} of them remote hits, or it is not the "
        f"control S-016 was phrased in"
    )

    served = [
        payloads
        for options, payloads in runs
        if payloads and _remote_cache_flag(options) and remote_cache_hits(payloads)
    ]
    assert len(served) == 1, f"expected exactly one remote-served run, got {len(served)}"
    remote = served[0]

    # The green half, on a run the remote cache really did serve.
    assert remote_cache_hits(remote) == list(range(len(remote))), (
        f"all {len(remote)} targets of the remote-served run must read as remote cache hits"
    )
    # And the two runs are the same answer to the control: that is the defect, pinned.
    assert cached_remotely_hits(disk) == cached_remotely_hits(remote) and remote_cache_hits(
        disk
    ) != remote_cache_hits(remote), (
        "the control must be unable to tell the disk-served run from the remote-served one "
        "while the restated predicate separates them — otherwise this test is not showing "
        "the false green it exists to close"
    )


# ---------------------------------------------------------------------------
# Release provenance scan (plan_test_speed_tiers.md C-005, S-013, S-024; ADR
# adr_test_speed_tiers.md C-PROV wiring). A `__testing` build carries fixed
# placeholder provenance; these pin the wiring that keeps one from being
# published. Each is a property of a workflow file, so each is structural.
# ---------------------------------------------------------------------------

WORKFLOW_DIR = ROOT / ".github" / "workflows"
SCAN_WORKFLOW_USES = "./.github/workflows/scan-binaries.yml"
PROVENANCE_CHECK = "scripts/release_provenance_check.py"


def _scan_callers(workflow: Path) -> list[str]:
    """Names of the jobs in `workflow` that call the reusable scan."""
    jobs = _load(workflow).get("jobs") or {}
    return [name for name, job in jobs.items() if (job or {}).get("uses") == SCAN_WORKFLOW_USES]


def _needs(job: dict) -> list[str]:
    needs = job.get("needs") or []
    return [needs] if isinstance(needs, str) else list(needs)


def _run_steps(job: dict) -> list[tuple[int, str]]:
    return [(index, step.get("run") or "") for index, step in enumerate(job.get("steps") or [])]


def _scan_input_default(name: str) -> object:
    inputs = _on(WORKFLOW_DIR / "scan-binaries.yml")["workflow_call"]["inputs"]
    return inputs[name].get("default")


def test_every_scan_marker_is_a_placeholder_the_testing_build_plants() -> None:
    """The scan's markers and `build.rs`'s placeholder table are two spellings of one fact;
    rename a placeholder and the matching marker would silently stop matching."""
    import importlib.util

    spec = importlib.util.spec_from_file_location("release_provenance_check", ROOT / PROVENANCE_CHECK)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    build_rs = (ROOT / "crates" / "ocx_cli" / "build.rs").read_text(encoding="utf-8")
    table = re.search(r"const TESTING_PLACEHOLDERS: &\[\(&str, &str\)\] = &\[(.*?)\];", build_rs, re.S)
    assert table, "crates/ocx_cli/build.rs no longer defines TESTING_PLACEHOLDERS — the reader found nothing"
    values = re.findall(r'\("[A-Z_]+",\s*"([^"]*)"\)', table.group(1))
    assert len(values) >= 12, f"read {len(values)} placeholder rows from build.rs, expected the 12 of ADR C-PROV"
    assert len(module.MARKERS) == 3, module.MARKERS
    for marker in module.MARKERS:
        assert any(marker.decode() in value for value in values), (
            f"scan marker {marker!r} is in no TESTING_PLACEHOLDERS value {values} — it can never match a test build"
        )


def test_release_host_needs_the_provenance_scan() -> None:
    """`host` (the GitHub Release, then every post-announce publish) waits for the scan,
    reads its result, and the scan waits for the binaries it scans (P-4)."""
    jobs = _load(WORKFLOW_DIR / "release.yml")["jobs"]
    callers = _scan_callers(WORKFLOW_DIR / "release.yml")
    assert len(callers) == 1, (
        f"release.yml has {len(callers)} jobs calling {SCAN_WORKFLOW_USES} ({callers}); expected exactly "
        f"one, rendered from `global-artifacts-jobs` in dist-workspace.toml"
    )
    scan = callers[0]
    assert "build-local-artifacts" in _needs(jobs[scan]), (
        f"release.yml `{scan}` needs {_needs(jobs[scan])}: without `build-local-artifacts` it runs "
        f"beside the build and scans no binary (that is the `local-artifacts-jobs` render)"
    )
    assert scan in _needs(jobs["host"]), f"release.yml `host` needs {_needs(jobs['host'])}, not `{scan}`"
    clause = f"(needs.{scan}.result == 'skipped' || needs.{scan}.result == 'success')"
    assert clause in str(jobs["host"].get("if", "")), (
        f"release.yml `host.if` does not read `{scan}`'s result, so an `always()` host "
        f"would publish after a failed scan: {jobs['host'].get('if')}"
    )


def test_the_scan_workflow_cannot_pass_by_skipping() -> None:
    """`host` accepts a `skipped` scan, so the scan must never be able to skip or soften itself."""
    document = _load(WORKFLOW_DIR / "scan-binaries.yml")
    assert document.get("permissions") == {"contents": "read"}, document.get("permissions")
    runs = []
    for name, job in document["jobs"].items():
        assert "if" not in job, f"scan-binaries.yml `{name}` has an `if:` — a skipped scan reads as a pass to `host`"
        assert "continue-on-error" not in job, f"scan-binaries.yml `{name}` has `continue-on-error`"
        for step in job.get("steps") or []:
            assert "if" not in step, f"scan-binaries.yml `{name}` step {step.get('name')!r} has an `if:`"
            assert "continue-on-error" not in step, f"scan-binaries.yml `{name}` step {step.get('name')!r} may fail silently"
            assert "upload-artifact" not in str(step.get("uses", "")), (
                f"scan-binaries.yml `{name}` uploads an artifact; `host` publishes every `artifacts-*` it downloads"
            )
            runs.append(step.get("run") or "")
    body = "\n".join(runs)
    assert f"{PROVENANCE_CHECK} --scan" in body and '--min-files "$MIN_FILES"' in body, body
    assert f"{PROVENANCE_CHECK} --exec" in body, body
    assert f"{PROVENANCE_CHECK} --link" in body and '--min-files "$LINK_MIN_FILES"' in body, body


def test_the_release_scan_floor_is_one_binary_per_dist_target() -> None:
    """cargo-dist passes the scan only `plan`, so the input defaults ARE the release's floor."""
    import tomllib

    targets = tomllib.loads((ROOT / "dist-workspace.toml").read_text(encoding="utf-8"))["dist"]["targets"]
    assert _scan_input_default("min-files") == len(targets), (
        f"scan-binaries.yml `min-files` defaults to {_scan_input_default('min-files')}, but "
        f"dist-workspace.toml builds {len(targets)} targets — a missing binary would pass the floor"
    )
    assert _scan_input_default("extract") is True
    assert _scan_input_default("artifact-pattern") == "artifacts-build-local-*"


def test_the_release_link_floor_is_one_binary_per_linux_dist_target() -> None:
    """The link check runs on cargo-dist archives only, so its reader floor is the release's Linux target count."""
    import tomllib

    targets = tomllib.loads((ROOT / "dist-workspace.toml").read_text(encoding="utf-8"))["dist"]["targets"]
    linux = [target for target in targets if "-unknown-linux-" in target]
    assert _scan_input_default("link-min-files") == len(linux), (
        f"scan-binaries.yml `link-min-files` defaults to {_scan_input_default('link-min-files')}, but "
        f"dist-workspace.toml builds {len(linux)} Linux targets {linux} — a missing one would pass the floor"
    )


def test_deploy_dev_publish_needs_the_provenance_scan() -> None:
    """S-024: builds → `scan-binaries` → publish; a marker in any staged binary skips the publish."""
    jobs = _load(WORKFLOW_DIR / "deploy-dev.yml")["jobs"]
    callers = _scan_callers(WORKFLOW_DIR / "deploy-dev.yml")
    assert callers, f"deploy-dev.yml has no job calling {SCAN_WORKFLOW_USES}"
    assert any(caller in _needs(jobs["publish"]) for caller in callers), (
        f"deploy-dev.yml `publish` needs {_needs(jobs['publish'])}, none of which is a scan job ({callers})"
    )
    builds = sorted(name for name in jobs if name.startswith("build-"))
    targets = json.loads(jobs["publish"]["with"]["targets"])
    for caller in callers:
        assert set(builds) <= set(_needs(jobs[caller])), f"`{caller}` needs {_needs(jobs[caller])}, not every {builds}"
        with_ = jobs[caller].get("with") or {}
        assert with_.get("min-files") == len(targets), (
            f"`{caller}` floors the scan at {with_.get('min-files')} binaries; `publish` pushes {len(targets)} targets"
        )
        assert with_.get("extract") is False, "deploy-dev uploads raw binaries, not archives"


def test_oci_publish_execs_the_provenance_check_before_publishing() -> None:
    """The native binary that drives the push must itself report release provenance."""
    steps = _run_steps(_load(WORKFLOW_DIR / "oci-publish.yml")["jobs"]["publish"])
    execs = [index for index, run in steps if f"{PROVENANCE_CHECK} --exec" in run]
    pushes = [index for index, run in steps if "ocx package push" in run]
    assert execs, "oci-publish.yml never runs `release_provenance_check.py --exec`"
    assert pushes and min(execs) < min(pushes), f"--exec at step {execs}, push at step {pushes}"


def test_oci_publish_bare_download_is_the_scanned_set() -> None:
    """S2: the bare path publishes only what deploy-dev's scan read. Without a `pattern`,
    `download-artifact` fetches every artifact of the run, so any other upload would be
    staged under `dist/` and pushed without ever having been scanned."""
    steps = _load(WORKFLOW_DIR / "oci-publish.yml")["jobs"]["publish"]["steps"]
    [download] = [step for step in steps if step.get("name") == "Download bare-binary artifacts"]
    jobs = _load(WORKFLOW_DIR / "deploy-dev.yml")["jobs"]
    callers = _scan_callers(WORKFLOW_DIR / "deploy-dev.yml")
    scanned = {(jobs[caller].get("with") or {}).get("artifact-pattern") for caller in callers}
    assert scanned == {"ocx-*"}, f"deploy-dev.yml scans {scanned}"
    assert (download.get("with") or {}).get("pattern") in scanned, (
        f"oci-publish.yml downloads pattern {(download.get('with') or {}).get('pattern')!r}; the scan read {scanned}"
    )


def test_deep_does_not_scan_the_bazel_built_binaries() -> None:
    """The Bazel-built `ocx` carries `__testing` placeholder provenance by design, so a
    provenance `--scan` of it reds; the scan lives in `scan-binaries.yml` over release bytes."""
    text = VERIFY_DEEP.read_text(encoding="utf-8")
    assert f"{PROVENANCE_CHECK} --scan" not in text, (
        "verify-deep.yml scans its binaries, which are the `_TESTING_PROVENANCE` Bazel build — "
        "that scan can only red; release and deploy-dev scan the real release bytes"
    )


# ---------------------------------------------------------------------------
# deploy-docs.yml
# ---------------------------------------------------------------------------

DEPLOY_DOCS = ROOT / ".github" / "workflows" / "deploy-docs.yml"
DEPLOY_ACTION = "ocx-sh/website/.github/actions/deploy"
FULL_SHA = re.compile(r"[0-9a-f]{40}")


def _deploy_steps() -> list[dict]:
    jobs = _load(DEPLOY_DOCS)["jobs"]
    return [
        step
        for job in jobs.values()
        for step in job.get("steps") or []
        if str(step.get("uses", "")).startswith(f"{DEPLOY_ACTION}@")
    ]


def test_deploy_docs_deploys_docs_and_schemas_with_a_pinned_action() -> None:
    """exactly two deploy steps, `/docs/` and `/schemas/`, the action pinned by full SHA."""
    steps = _deploy_steps()
    paths = sorted((step.get("with") or {}).get("path") for step in steps)
    assert paths == ["/docs/", "/schemas/"], f"deploy-docs.yml deploys {paths}"
    pairs = {(step["with"]["path"], step["with"]["dist"]) for step in steps}
    assert pairs == {("/docs/", "dist/docs"), ("/schemas/", "dist/schemas")}, f"deploy-docs.yml dist -> path pairs: {pairs}"
    for step in steps:
        ref = step["uses"].split("@", 1)[1].split()[0]
        assert FULL_SHA.fullmatch(ref), f"{DEPLOY_ACTION} is pinned by {ref!r}, not a 40-hex SHA"
        assert (step.get("with") or {}).get("storage-key") == "${{ secrets.BUNNY_STORAGE_KEY }}"


def test_deploy_docs_runs_in_the_ocx_sh_environment_under_one_concurrency_group() -> None:
    """the deploy job sits in environment `ocx.sh`; whole runs serialise on `ocx-sh-deploy`."""
    document = _load(DEPLOY_DOCS)
    jobs = document["jobs"]
    deploy_steps = _deploy_steps()
    [deploy] = [job for job in jobs.values() if any(step in deploy_steps for step in job["steps"])]
    environment = deploy.get("environment")
    name = environment.get("name") if isinstance(environment, dict) else environment
    assert name == "ocx.sh", f"deploy environment is {name!r}"
    concurrency = document.get("concurrency") or {}
    assert concurrency.get("group") == "ocx-sh-deploy", f"workflow concurrency is {concurrency}"
    assert concurrency.get("cancel-in-progress") is False, f"workflow concurrency is {concurrency}"
    assert "concurrency" not in deploy, "concurrency belongs at workflow level, not on the deploy job"


def test_deploy_docs_checks_the_built_site_before_uploading_it() -> None:
    """the build job runs `task website:check` and the doc-script drift gate before the artifact upload."""
    steps = _load(DEPLOY_DOCS)["jobs"]["build"]["steps"]
    runs = [str(step.get("run", "")).strip() for step in steps]
    uploads = [i for i, step in enumerate(steps) if str(step.get("uses", "")).startswith("actions/upload-artifact@")]
    assert len(uploads) == 1, f"build job has {len(uploads)} upload steps"
    for command in ("task website:build", "task website:check", "task test:doc-scripts:drift"):
        assert command in runs, f"build job never runs `{command}`"
    assert runs.index("task website:build") < runs.index("task website:check") < uploads[0]
    assert runs.index("task test:doc-scripts:drift") < uploads[0]


def test_acceptance_binaries_downloaded_for_a_test_run_are_made_executable() -> None:
    """An artifact round trip drops every executable bit: each job that downloads into
    `test/bin` on a runner that has them runs `chmod -R +x test/bin`, or a missing one reds
    deep in the suite instead of at the step that lost it."""
    checked = []
    for workflow in (DEEP_REPO, ACCEPTANCE):
        for name, job in _repo_jobs(workflow).items():
            steps = job.get("steps") or []
            if not any(
                str(s.get("uses", "")).startswith("actions/download-artifact@")
                and (s.get("with") or {}).get("path") == "test/bin"
                for s in steps
            ):
                continue
            checked.append(f"{workflow.name}:{name}")
            runs = "\n".join(str(s.get("run", "")) for s in steps)
            assert "chmod -R +x test/bin" in runs, (
                f"{workflow.name} job {name!r} downloads into test/bin but never `chmod -R +x test/bin`"
            )
    assert "acceptance.yml:shard" in checked, (
        f"read {checked}; the readers did not find the shard job, the one that downloads the test-bin artifact"
    )


def test_deploy_docs_grants_nothing_at_the_top_and_triggers_on_main_and_dispatch() -> None:
    """top-level `permissions: {}`, push to `main` plus `workflow_dispatch`, nothing else."""
    document = _load(DEPLOY_DOCS)
    assert document.get("permissions") == {}, f"top-level permissions is {document.get('permissions')!r}"
    triggers = _on(DEPLOY_DOCS)
    assert isinstance(triggers, dict), f"deploy-docs.yml `on:` is {triggers!r}"
    assert set(triggers) == {"push", "workflow_dispatch"}, f"deploy-docs.yml triggers on {sorted(triggers)}"
    assert triggers["push"]["branches"] == ["main"], f"push trigger is {triggers['push']}"


def test_the_retired_pages_deploy_workflows_are_gone() -> None:
    """the Pages deploy and its release dispatcher are deleted, with the release job that called it."""
    for name in ("deploy-website.yml", "deploy-website-release.yml"):
        assert not (ROOT / ".github" / "workflows" / name).exists(), f"{name} is back"
    jobs = _load(ROOT / ".github" / "workflows" / "release.yml")["jobs"]
    assert "custom-deploy-website-release" not in jobs


# ---------------------------------------------------------------------------
# One `CACHE_RC`, and every Bazel call a task makes carries it
# ---------------------------------------------------------------------------

#: `--bazelrc` is a startup option, so it must sit immediately after the second `bazel`;
#: a call without it reads the shared cache anonymously (a 401) and compiles cold, green.
_TASK_BAZEL_CALL = re.compile(r"ocx exec bazel -- bazel(?:[ \t]+(\S+))?")
#: Invocations found in the tracked taskfiles when this was written. A floor, not
#: a census: a reader that stopped seeing taskfiles passes a negative silently.
_TASK_BAZEL_CALL_FLOOR = 25


def _tracked_taskfiles() -> list[Path]:
    listed = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files", "--", ":(glob)**/taskfile.yml", ":(glob)**/*.taskfile.yml"],
        capture_output=True,
        encoding="utf-8",
        check=True,
    ).stdout.split()
    return [ROOT / name for name in listed]


def test_every_task_bazel_call_passes_the_cache_rc_first() -> None:
    seen = 0
    missing: list[str] = []
    for path in _tracked_taskfiles():
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            if line.lstrip().startswith("#"):
                continue
            for call in _TASK_BAZEL_CALL.finditer(line):
                seen += 1
                if call.group(1) != "{{.CACHE_RC}}":
                    missing.append(f"{path.relative_to(ROOT)}:{number}")
    assert seen >= _TASK_BAZEL_CALL_FLOOR, (
        f"found {seen} `ocx exec bazel -- bazel` calls in the taskfiles, floor {_TASK_BAZEL_CALL_FLOOR} — "
        f"the reader stopped short, and a clean result over it proves nothing"
    )
    assert not missing, (
        "these task calls to bazel do not pass `{{.CACHE_RC}}` as the first startup option, so on CI they "
        "read the shared cache anonymously: " + ", ".join(missing)
    )


def test_cache_rc_is_defined_once_in_the_root_taskfile() -> None:
    defined = [
        f"{path.relative_to(ROOT)}:{number}"
        for path in _tracked_taskfiles()
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1)
        if re.match(r"\s*CACHE_RC:", line)
    ]
    assert len(defined) == 1 and defined[0].startswith("taskfile.yml:"), (
        f"`CACHE_RC:` must be defined exactly once, in the root taskfile.yml; found: {defined}"
    )


def test_bazel_test_accept_never_uploads_verdicts_whatever_rc_a_lane_passes() -> None:
    """Acceptance verdicts stay on the local disk, per `adr_test_speed_tiers.md`.

    A command-line flag outranks the CI rc's `--remote_upload_local_results=true`, so this holds on any lane.
    """
    tasks = yaml.safe_load((ROOT / "taskfiles" / "bazel.taskfile.yml").read_text(encoding="utf-8"))["tasks"]
    bodies = [c.get("cmd", "") for c in tasks["test:accept"]["cmds"] if isinstance(c, dict)]
    invocations = [
        line
        for body in bodies
        for line in body.splitlines()
        if not line.lstrip().startswith("#") and "bazel {{.CACHE_RC}} test " in line
    ]
    assert len(invocations) == 1, f"expected one `bazel test` line in `bazel:test:accept`, found {invocations}"
    assert re.search(r"bazel \{\{\.CACHE_RC\}\} test\b.* --remote_upload_local_results=false\b", invocations[0]), (
        f"`bazel:test:accept` must pass `--remote_upload_local_results=false` after `test`: {invocations[0].strip()}"
    )


# ---------------------------------------------------------------------------
# The repo-owned workflows behind the conventions template's `repo` jobs; a
# missing file or job is a finding that names it.
# ---------------------------------------------------------------------------

BASIC_REPO = WORKFLOW_DIR / "basic-repo.yml"
DEEP_REPO = WORKFLOW_DIR / "deep-repo.yml"
ACCEPTANCE = WORKFLOW_DIR / "acceptance.yml"
CROSS = WORKFLOW_DIR / "cross.yml"
SHELL_ACTIVATION_DEEP = WORKFLOW_DIR / "shell-activation-deep.yml"
REUSABLE_REPO_WORKFLOWS = (BASIC_REPO, DEEP_REPO, ACCEPTANCE, CROSS)

#: Where the cross-build jobs run. "B" keeps all eight in the deep tier only, "A" adds all eight to every
#: pull request, "S" (split) adds only the targets with a native smoke leg to every pull request
#: (`CROSS_BASIC_TARGETS`) and leaves the full matrix to deep. The slowest warm cross job's wall time
#: decides how much basic can carry.
CROSS_VARIANT = "S"
CROSS_BASIC_TARGETS = frozenset({"aarch64-apple-darwin", "x86_64-pc-windows-gnullvm"})
#: The legs a native runner downloads a unit-test bundle for; `deep-repo.yml`'s `unit-native` matrix.
NATIVE_BUNDLE_TARGETS = ("aarch64-apple-darwin", "x86_64-pc-windows-gnullvm")
CROSS_TARGETS = frozenset(
    {
        "aarch64-apple-darwin",
        "aarch64-pc-windows-gnullvm",
        "aarch64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-gnullvm",
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
    }
)
BASIC_REPO_JOBS = frozenset({"binaries", "acceptance", "smoke-acceptance", "conventional-commits", "scripts-tests"})
DEEP_REPO_JOBS = frozenset(
    {
        "cross",
        "unit-native",
        "acceptance-native",
        "satellite-verify",
        "satellite-contract",
        "index-conformance-drift",
        "shell-activation-deep",
    }
)
ACCEPTANCE_INPUTS = {
    "os": {"type": "string"},
    "shard-count": {"type": "number", "default": 4},
    "binary": {"type": "string"},
    "limits-dir": {"type": "string", "default": "test"},
    "release-binary": {"type": "boolean", "default": False},
    "no-docker": {"type": "boolean", "default": False},
    "toolchain-group": {"type": "string", "default": ""},
}
CROSS_WORKFLOW_CALL = "./.github/workflows/cross.yml"


def _repo_workflow(path: Path) -> dict:
    assert path.exists(), f"{path.relative_to(ROOT)}: no such workflow"
    return _load(path)


def _repo_jobs(path: Path) -> dict[str, dict]:
    return {name: job or {} for name, job in (_repo_workflow(path).get("jobs") or {}).items()}


def _repo_job(path: Path, name: str) -> dict:
    jobs = _repo_jobs(path)
    assert name in jobs, f"{path.name}: no job `{name}`; its jobs are {sorted(jobs)}"
    return jobs[name]


def _workflow_call(path: Path) -> dict:
    document = _repo_workflow(path)
    triggers = document.get("on", document.get(True))
    assert isinstance(triggers, dict) and "workflow_call" in triggers, (
        f"{path.name}: not a reusable workflow, `on` is {triggers!r}"
    )
    return triggers["workflow_call"] or {}


def _permission_overreach(document: dict) -> list[str]:
    """Where a workflow or one of its jobs asks for more than `contents: read`.

    The template's caller grants only that, and a called job asking for more fails the
    whole run at startup. `none` asks for nothing, and an absent key asks for nothing.
    """
    scopes = [("workflow", document.get("permissions"))]
    scopes += [(f"job `{name}`", (job or {}).get("permissions")) for name, job in (document.get("jobs") or {}).items()]
    found = []
    for where, granted in scopes:
        if isinstance(granted, str):
            found.append(f"{where}: `{granted}`")
        else:
            found += [
                f"{where}: {scope}: {level}"
                for scope, level in (granted or {}).items()
                if level != "none" and (scope, level) != ("contents", "read")
            ]
    return found


@pytest.mark.parametrize(
    ("permissions", "overreach"),
    [
        pytest.param({}, False, id="nothing"),
        pytest.param({"contents": "read"}, False, id="contents-read"),
        pytest.param({"contents": "read", "checks": "none"}, False, id="none-is-nothing"),
        pytest.param({"contents": "write"}, True, id="contents-write"),
        pytest.param({"contents": "read", "checks": "write"}, True, id="checks-write"),
        pytest.param("read-all", True, id="read-all"),
    ],
)
def test_the_permission_reader_flags_more_than_contents_read(permissions: object, overreach: bool) -> None:
    """The reader's red and green, on a workflow-level and on a job-level grant."""
    assert bool(_permission_overreach({"permissions": permissions, "jobs": {}})) is overreach
    assert bool(_permission_overreach({"jobs": {"j": {"permissions": permissions}}})) is overreach


@pytest.mark.parametrize("workflow", REUSABLE_REPO_WORKFLOWS, ids=lambda path: path.name)
def test_a_repo_reusable_workflow_declares_the_cache_secret_as_optional(workflow: Path) -> None:
    """A fork pull request holds no secrets, so the secret is declared and not required."""
    declared = _declared_secrets(_repo_workflow(workflow))
    _workflow_call(workflow)
    assert WRITE_SECRET in declared, (
        f"{workflow.name}: `on.workflow_call.secrets` declares {sorted(declared)}, not {WRITE_SECRET}"
    )
    assert (declared[WRITE_SECRET] or {}).get("required") is False, (
        f"{workflow.name}: {WRITE_SECRET} must be declared with `required: false`, got {declared[WRITE_SECRET]!r}"
    )


@pytest.mark.parametrize("workflow", (BASIC_REPO, DEEP_REPO), ids=lambda path: path.name)
def test_a_tier_workflow_grants_nothing_at_the_top(workflow: Path) -> None:
    document = _repo_workflow(workflow)
    assert document.get("permissions") == {} and "permissions" in document, (
        f"{workflow.name}: top-level `permissions` is {document.get('permissions')!r}, expected `permissions: {{}}`"
    )


@pytest.mark.parametrize(
    "workflow", (*REUSABLE_REPO_WORKFLOWS, SHELL_ACTIVATION_DEEP), ids=lambda path: path.name
)
def test_no_job_called_through_the_template_asks_for_more_than_contents_read(workflow: Path) -> None:
    findings = _permission_overreach(_repo_workflow(workflow))
    assert not findings, (
        f"{workflow.name} requests more than `contents: read`: {findings}. The template's `repo` job "
        f"grants only that, and a nested job asking for more fails the whole run at startup"
    )


def test_basic_repo_has_the_jobs_the_basic_tier_needs() -> None:
    jobs = set(_repo_jobs(BASIC_REPO))
    assert BASIC_REPO_JOBS <= jobs, (
        f"{BASIC_REPO.name}: no job {sorted(BASIC_REPO_JOBS - jobs)}; its jobs are {sorted(jobs)}"
    )


def test_basic_repo_runs_linux_acceptance_on_a_rebuilt_binary() -> None:
    job = _repo_job(BASIC_REPO, "acceptance")
    assert job.get("uses") == "./.github/workflows/acceptance.yml", (
        f"{BASIC_REPO.name}: job `acceptance` uses {job.get('uses')!r}, expected the reusable acceptance.yml"
    )
    inputs = job.get("with") or {}
    assert inputs.get("binary") == "build", (
        f"{BASIC_REPO.name}: job `acceptance` passes binary={inputs.get('binary')!r}, expected 'build'"
    )
    assert str(inputs.get("os", "")).startswith("ubuntu"), (
        f"{BASIC_REPO.name}: job `acceptance` passes os={inputs.get('os')!r}, expected an ubuntu runner label"
    )


def test_basic_repo_scripts_tests_job_cannot_be_skipped() -> None:
    """A required job skipped by its own `if:` leaves the gate unknown, not green.

    The job always runs and decides inside whether `scripts/**` changed.
    """
    job = _repo_job(BASIC_REPO, "scripts-tests")
    assert "if" not in job, f"{BASIC_REPO.name}: job `scripts-tests` has `if: {job.get('if')}`"


def test_deep_repo_has_the_jobs_the_deep_tier_needs() -> None:
    jobs = set(_repo_jobs(DEEP_REPO))
    assert DEEP_REPO_JOBS <= jobs, (
        f"{DEEP_REPO.name}: no job {sorted(DEEP_REPO_JOBS - jobs)}; its jobs are {sorted(jobs)}"
    )


@pytest.mark.parametrize("name", ["unit-native", "acceptance-native"])
def test_a_deep_native_leg_waits_for_the_cross_job(name: str) -> None:
    """A macOS or Windows leg runs the outputs the cross job built in the same run."""
    needs = _needs(_repo_job(DEEP_REPO, name))
    assert "cross" in needs, f"{DEEP_REPO.name}: job `{name}` needs {needs}, not `cross`"


def test_the_deep_native_acceptance_legs_run_the_full_suite_per_os_without_docker() -> None:
    """macOS and Windows each run the whole suite in four shards against their own limits.

    `no-docker` is the declaration `OCX_TEST_NO_DOCKER=1` hangs on; a leg without it would start
    Docker services on a runner that has none, and the Linux basic tier never passes it.
    """
    job = _repo_job(DEEP_REPO, "acceptance-native")
    assert job.get("uses") == "./.github/workflows/acceptance.yml", (
        f"{DEEP_REPO.name}: job `acceptance-native` uses {job.get('uses')!r}, expected the reusable acceptance.yml"
    )
    inputs = job.get("with") or {}
    assert inputs.get("no-docker") is True, f"{DEEP_REPO.name}: `acceptance-native` passes no-docker={inputs.get('no-docker')!r}"
    assert inputs.get("shard-count") == 4, f"{DEEP_REPO.name}: `acceptance-native` passes shard-count={inputs.get('shard-count')!r}"
    assert inputs.get("binary") == "artifact:test-bin-${{ matrix.job.target }}", (
        f"{DEEP_REPO.name}: `acceptance-native` passes binary={inputs.get('binary')!r}, expected the artifact `cross` uploads"
    )
    assert inputs.get("limits-dir") == "${{ matrix.job.limits }}", f"{DEEP_REPO.name}: limits-dir is {inputs.get('limits-dir')!r}"
    legs = job["strategy"]["matrix"]["job"]
    assert {leg["os"]: leg["limits"] for leg in legs} == {
        "macos-latest": "test/limits/macos",
        "windows-latest": "test/limits/windows",
    }, f"{DEEP_REPO.name}: `acceptance-native` legs are {legs}"
    linux = (_repo_job(BASIC_REPO, "acceptance").get("with") or {})
    assert not linux.get("no-docker"), f"{BASIC_REPO.name}: Linux stays strict, but `acceptance` passes {linux}"


@pytest.mark.parametrize("platform", ["windows", "macos"])
def test_the_native_limits_are_committed(platform: str) -> None:
    """The three files `scripts/accept_floor.py --limits-dir` reads, per native leg."""
    for name in ("SUITE_FLOOR", "SKIP_CEILING", "XFAIL_CEILING"):
        path = ROOT / "test" / "limits" / platform / name
        assert path.is_file(), f"{path.relative_to(ROOT)} is missing"
        assert path.read_text(encoding="utf-8").strip().isdigit(), f"{path.relative_to(ROOT)} is not a plain integer"


def test_the_acceptance_shard_starts_docker_only_when_the_caller_has_it() -> None:
    """Every Docker step is gated on `no-docker`, and the suite's skip switch is bound to the same input."""
    shard = _repo_jobs(ACCEPTANCE)["shard"]
    assert shard["env"].get("OCX_TEST_NO_DOCKER") == "${{ inputs.no-docker && '1' || '' }}", (
        f"{ACCEPTANCE.name}: `shard` binds OCX_TEST_NO_DOCKER to {shard['env'].get('OCX_TEST_NO_DOCKER')!r}"
    )
    docker_steps = [step for step in shard["steps"] if "docker compose" in str(step.get("run", ""))]
    assert len(docker_steps) >= 3, f"read {len(docker_steps)} Docker steps; the reader stopped matching"
    ungated = [step.get("name") for step in docker_steps if "!inputs.no-docker" not in str(step.get("if", ""))]
    assert not ungated, f"{ACCEPTANCE.name}: Docker steps {ungated} run even when the caller declares no-docker"


def test_the_acceptance_shard_binds_the_release_binary_switch_to_its_input() -> None:
    """`release-binary` reaches the suite only through this env variable; a dropped or renamed one is silent."""
    shard = _repo_jobs(ACCEPTANCE)["shard"]
    expected = "${{ inputs.release-binary && '1' || '' }}"
    actual = (shard.get("env") or {}).get("OCX_TEST_RELEASE_BINARY")
    assert actual == expected, f"{ACCEPTANCE.name}: `shard` binds OCX_TEST_RELEASE_BINARY to {actual!r}, expected {expected!r}"


def test_the_acceptance_shard_builds_nothing() -> None:
    """The shard runs on macOS and Windows too: it downloads and executes, and only `binary` builds."""
    offending = [
        step.get("name", step.get("uses", step.get("run")))
        for step in _repo_jobs(ACCEPTANCE)["shard"]["steps"]
        if _BUILD_TOOLING.search(yaml.safe_dump({k: v for k, v in step.items() if k != "name"}))
    ]
    assert not offending, f"{ACCEPTANCE.name}: `shard` has build steps {offending}"


def test_deep_repo_runs_shell_activation_through_its_reusable_workflow() -> None:
    job = _repo_job(DEEP_REPO, "shell-activation-deep")
    assert job.get("uses") == "./.github/workflows/shell-activation-deep.yml", (
        f"{DEEP_REPO.name}: job `shell-activation-deep` uses {job.get('uses')!r}"
    )


def _cross_targets_passed(job: dict) -> frozenset[str]:
    """The `targets` a caller hands `cross.yml`; empty means the whole matrix."""
    return frozenset(str((job.get("with") or {}).get("targets", "")).split())


def test_the_cross_jobs_sit_where_the_recorded_variant_puts_them() -> None:
    """Deep always cross-builds all eight; basic builds none (B), all eight (A) or the native-leg pair (S)."""
    assert CROSS_VARIANT in {"A", "B", "S"}, f"CROSS_VARIANT is {CROSS_VARIANT!r}, expected 'A', 'B' or 'S'"
    deep = _repo_job(DEEP_REPO, "cross")
    assert deep.get("uses") == CROSS_WORKFLOW_CALL, (
        f"{DEEP_REPO.name}: job `cross` uses {deep.get('uses')!r}, expected {CROSS_WORKFLOW_CALL}"
    )
    assert not _cross_targets_passed(deep), f"{DEEP_REPO.name}: `cross` narrows the matrix to {sorted(_cross_targets_passed(deep))}"
    callers = {name: job for name, job in _repo_jobs(BASIC_REPO).items() if job.get("uses") == CROSS_WORKFLOW_CALL}
    if CROSS_VARIANT == "B":
        assert not callers, f"{BASIC_REPO.name}: variant B keeps cross in the deep tier, but {sorted(callers)} call cross.yml"
        return
    assert callers, f"{BASIC_REPO.name}: variant {CROSS_VARIANT} cross-builds on every pull request, but no job calls cross.yml"
    passed = frozenset().union(*(_cross_targets_passed(job) for job in callers.values()))
    expected = CROSS_BASIC_TARGETS if CROSS_VARIANT == "S" else frozenset()
    assert passed == expected, (
        f"{BASIC_REPO.name}: variant {CROSS_VARIANT} passes cross targets {sorted(passed)}, expected {sorted(expected)}"
        f" (empty is the whole matrix)"
    )


def test_every_basic_native_leg_has_its_binaries_cross_built() -> None:
    """The subset basic hands `cross.yml` is exactly the targets its native legs download, no more.

    A native leg whose target is not built downloads nothing and reds late; a built target no leg
    uses is a Bazel build every pull request pays for.
    """
    built = _cross_targets_passed(_repo_job(BASIC_REPO, "cross"))
    for name in ("unit-native", "acceptance-native"):
        job = _repo_job(BASIC_REPO, name)
        assert "cross" in _needs(job), f"{BASIC_REPO.name}: `{name}` needs {_needs(job)}, not `cross`"
        legs = {leg["target"] for leg in job["strategy"]["matrix"]["job"]}
        assert legs == built, f"{BASIC_REPO.name}: `{name}` runs {sorted(legs)}, `cross` builds {sorted(built)}"


def test_the_basic_smoke_acceptance_legs_run_the_smoke_selection_with_their_own_limits() -> None:
    """`marker: smoke` selects the tier; the full suite's floor would red it, so each leg names smoke limits."""
    job = _repo_job(BASIC_REPO, "acceptance-native")
    inputs = job.get("with") or {}
    assert job.get("uses") == "./.github/workflows/acceptance.yml", f"{BASIC_REPO.name}: `acceptance-native` uses {job.get('uses')!r}"
    assert inputs.get("marker") == "smoke", f"{BASIC_REPO.name}: `acceptance-native` passes marker={inputs.get('marker')!r}"
    assert inputs.get("no-docker") is True, f"{BASIC_REPO.name}: `acceptance-native` passes no-docker={inputs.get('no-docker')!r}"
    assert inputs.get("shard-count") == 1, f"{BASIC_REPO.name}: `acceptance-native` passes shard-count={inputs.get('shard-count')!r}"
    assert inputs.get("binary") == "artifact:test-bin-${{ matrix.job.target }}", f"binary is {inputs.get('binary')!r}"
    legs = job["strategy"]["matrix"]["job"]
    assert {leg["os"]: leg["limits"] for leg in legs} == {
        "macos-latest": "test/limits/macos-smoke",
        "windows-latest": "test/limits/windows-smoke",
    }, f"{BASIC_REPO.name}: `acceptance-native` legs are {legs}"
    for leg in legs:
        for name in ("SUITE_FLOOR", "SKIP_CEILING", "XFAIL_CEILING"):
            path = ROOT / leg["limits"] / name
            assert path.read_text(encoding="utf-8").strip().isdigit(), f"{path.relative_to(ROOT)} is missing or not an integer"


def test_the_acceptance_marker_reaches_pytest_and_defaults_to_the_whole_suite() -> None:
    """The input is wired into the shard's pytest call (`PYTEST_ADDOPTS`), and an empty one adds no `-m`."""
    assert _workflow_call(ACCEPTANCE)["inputs"]["marker"]["default"] == ""
    step = next(s for s in _repo_jobs(ACCEPTANCE)["shard"]["steps"] if s.get("name") == "Run Acceptance Tests")
    assert step["env"]["PYTEST_ADDOPTS"] == "${{ inputs.marker != '' && format('-m {0}', inputs.marker) || '' }}", (
        f"PYTEST_ADDOPTS is bound to {step['env'].get('PYTEST_ADDOPTS')!r}"
    )


def _cross_matrix(only: str = "") -> list[dict]:
    """`task cross:matrix`, the matrix `cross.yml`'s plan job hands its legs."""
    done = subprocess.run(
        ["task", "cross:matrix", f"ONLY={only}"], capture_output=True, encoding="utf-8", cwd=ROOT, check=False
    )
    assert done.returncode == 0, done.stderr
    return json.loads(done.stdout)["include"]


def test_the_cross_workflow_builds_exactly_the_supported_targets() -> None:
    """The default run is every `task cross:targets` triple, each flagged for what its native leg downloads."""
    _workflow_call(CROSS)
    legs = _cross_matrix()
    assert {leg["target"] for leg in legs} == CROSS_TARGETS, (
        f"cross.yml builds {sorted(leg['target'] for leg in legs)}, the eight supported triples are {sorted(CROSS_TARGETS)}"
    )
    assert {leg["target"] for leg in legs if leg.get("bundle")} == set(NATIVE_BUNDLE_TARGETS)


def test_a_subset_run_of_the_cross_workflow_builds_only_what_it_was_given() -> None:
    legs = _cross_matrix(" ".join(CROSS_BASIC_TARGETS))
    assert {leg["target"] for leg in legs} == CROSS_BASIC_TARGETS, legs


def test_the_cross_job_takes_its_matrix_from_the_plan_job() -> None:
    """A literal matrix would be a second list beside `task cross:targets`; the plan job derives it."""
    jobs = _repo_jobs(CROSS)
    assert set(jobs) == {"plan", "cross"}, f"cross.yml jobs are {sorted(jobs)}"
    assert jobs["cross"]["needs"] == "plan"
    assert jobs["cross"]["strategy"]["matrix"] == "${{ fromJSON(needs.plan.outputs.matrix) }}"
    runs = "\n".join(str(step.get("run", "")) for step in jobs["plan"]["steps"])
    assert 'task cross:matrix ONLY="$ONLY"' in runs, f"the plan job runs {runs!r}"
    assert jobs["plan"]["steps"][-1]["env"]["ONLY"] == "${{ inputs.targets }}"
    assert _workflow_call(CROSS)["inputs"]["targets"]["default"] == ""


#: `platform_triples = [ ... ]` in MODULE.bazel; the list holds only quoted triples, so the first `]` closes it.
_PLATFORM_TRIPLES = re.compile(r"platform_triples\s*=\s*\[([^\]]*)\]")
#: Enough that a reader which stopped matching cannot pass on a smaller-than-real list.
PLATFORM_TRIPLES_FLOOR = 8


def test_module_bazel_platform_triples_equal_the_cross_targets() -> None:
    """`taskfiles/cross.taskfile.yml` and `subsystem-taskfiles.md` promise the two lists stay equal."""
    block = _PLATFORM_TRIPLES.search((ROOT / "MODULE.bazel").read_text(encoding="utf-8"))
    assert block, "MODULE.bazel: no `platform_triples = [...]` literal; the reader stopped matching"
    triples = re.findall(r'"([^"]+)"', block.group(1))
    assert len(triples) >= PLATFORM_TRIPLES_FLOOR, (
        f"read {len(triples)} platform_triples, expected at least {PLATFORM_TRIPLES_FLOOR}"
    )
    assert len(set(triples)) == len(triples), f"MODULE.bazel: platform_triples repeats an entry: {triples}"
    found = set(triples)
    assert found == CROSS_TARGETS, (
        f"MODULE.bazel platform_triples differ from the cross targets; "
        f"missing {sorted(CROSS_TARGETS - found)}, unexpected {sorted(found - CROSS_TARGETS)}"
    )


def test_shell_activation_deep_is_called_and_never_scheduled() -> None:
    """The weekly run is the deep tier's job now, so a cron here runs it a second time."""
    triggers = _on(SHELL_ACTIVATION_DEEP)
    assert isinstance(triggers, dict) and "workflow_call" in triggers, (
        f"{SHELL_ACTIVATION_DEEP.name}: `on` is {triggers!r}, expected a `workflow_call` trigger"
    )
    assert "schedule" not in triggers, (
        f"{SHELL_ACTIVATION_DEEP.name}: `on` still has a `schedule` trigger ({sorted(triggers)})"
    )


def test_the_acceptance_workflow_takes_the_documented_inputs() -> None:
    inputs = _workflow_call(ACCEPTANCE).get("inputs") or {}
    findings = []
    for name, want in ACCEPTANCE_INPUTS.items():
        if name not in inputs:
            findings.append(f"no input `{name}`")
            continue
        got = inputs[name] or {}
        if got.get("type") != want["type"]:
            findings.append(f"`{name}` has type {got.get('type')!r}, expected {want['type']!r}")
        if "default" in want and (got.get("default") != want["default"] or type(got.get("default")) is not type(want["default"])):
            findings.append(f"`{name}` defaults to {got.get('default')!r}, expected {want['default']!r}")
    description = str((inputs.get("binary") or {}).get("description", ""))
    if "build" not in description or "artifact:" not in description:
        findings.append("`binary` does not document its values `build` and `artifact:<name>`")
    assert not findings, f"{ACCEPTANCE.name} `on.workflow_call.inputs`: {findings}"


def _shard_count_source(document: dict, job: dict, step: dict) -> str | None:
    """What `EXPECT_SHARDS=` is given in a floor step: the input's expression, or `None`.

    The value reaches the task inline (`${{ inputs.shard-count }}`) or through an `env:`
    binding of the step, job or workflow (`$SHARD_COUNT`); a literal is neither.
    """
    found = re.search(r"EXPECT_SHARDS=(\"?\$\{\{[^}]*\}\}\"?|\S+)", str(step.get("run", "")))
    if not found:
        return None
    value = found.group(1).strip("\"'")
    variable = re.fullmatch(r"\$\{?(\w+)\}?", value)
    if variable:
        for env in (step.get("env"), job.get("env"), document.get("env")):
            if env and variable.group(1) in env:
                return str(env[variable.group(1)])
    return value


def test_the_acceptance_workflow_merges_shards_on_the_input_shard_count() -> None:
    document = _repo_workflow(ACCEPTANCE)
    jobs = _repo_jobs(ACCEPTANCE)
    shard_jobs = {name for name, job in jobs.items() if (job.get("strategy") or {}).get("matrix")}
    floor_steps = {
        name: [step for step in job.get("steps") or [] if "accept:floor" in str(step.get("run", ""))]
        for name, job in jobs.items()
    }
    merge_jobs = sorted(name for name, steps in floor_steps.items() if steps)
    assert shard_jobs, f"{ACCEPTANCE.name}: no job has a `strategy.matrix`, so there are no shards"
    assert len(merge_jobs) == 1, f"{ACCEPTANCE.name}: jobs running the suite floor are {merge_jobs}, expected exactly one"
    merge = merge_jobs[0]
    assert set(_needs(jobs[merge])) & shard_jobs, (
        f"{ACCEPTANCE.name}: merge job `{merge}` needs {_needs(jobs[merge])}, none of the shard jobs {sorted(shard_jobs)}"
    )
    sources = [_shard_count_source(document, jobs[merge], step) for step in floor_steps[merge]]
    assert sources and all(source and "inputs.shard-count" in source for source in sources), (
        f"{ACCEPTANCE.name}: the floor step in `{merge}` takes EXPECT_SHARDS from {sources}, "
        f"expected `inputs.shard-count`, never a literal"
    )


# ---------------------------------------------------------------------------
# The workflows around the tiers: Discord, the readiness gate, the write-capable responder
# ---------------------------------------------------------------------------

NOTIFY_DISCORD = WORKFLOW_DIR / "notify-discord.yml"
CLAUDE_TEMPLATE = WORKFLOW_DIR / "claude.yml"
CLAUDE_WRITE = WORKFLOW_DIR / "claude-write.yml"
RELEASE_READINESS = WORKFLOW_DIR / "release-readiness.yml"
_LEVELS = {"none": 0, "read": 1, "write": 2}
def test_every_workflow_the_discord_notifier_watches_is_some_workflows_name() -> None:
    """`workflow_run.workflows` matches the `name:` of the workflow that ran, exactly. A name
    that no file carries is a notification that never fires, with no error anywhere — which is
    how `Basic Verification` and `Verify Deep` outlived the rename to `verify-basic` and
    `verify-deep`."""
    names = {str(_load(workflow).get("name")) for workflow in WORKFLOWS}
    watched = _on(NOTIFY_DISCORD)["workflow_run"]["workflows"]
    # The floor: an empty list is a notifier that watches nothing, and sweeps clean.
    assert {"verify-basic", "verify-deep"} <= set(watched), f"notify-discord.yml watches {watched}"
    missing = sorted(set(watched) - names)
    assert not missing, (
        f"notify-discord.yml watches {missing}, which no workflow's `name:` is; the names are {sorted(names)}"
    )


def _scope_levels(document: dict, job: dict) -> dict[str, int]:
    """The permission each scope of a job is granted: its own `permissions:`, else the workflow's.

    `read-all` / `write-all` are the scope `*`.
    """
    granted = job.get("permissions", document.get("permissions") or {})
    if isinstance(granted, str):
        return {"*": _LEVELS[granted.split("-")[0]]}
    return {scope: _LEVELS[level] for scope, level in (granted or {}).items()}


def _callers_granting_too_little(documents: list[tuple[str, dict]]) -> list[tuple[str, str, str]]:
    """`(caller job, scope, message)` where a job calling a repo workflow grants less than a job of that workflow asks for.

    GitHub checks this when the run starts, statically — a job an `if:` would skip counts — and
    a violation fails the whole run, not the one job. Each edge is checked against the callee's
    own jobs; a callee job that itself calls a workflow is bounded by its own grant, checked at
    that edge.
    """
    by_name = dict(documents)
    findings = []
    for where, document, job in _jobs(documents):
        callee = _local_callee(job)
        if callee is None or callee not in by_name:
            continue
        grant = _scope_levels(document, job)
        for name, called in (by_name[callee].get("jobs") or {}).items():
            for scope, level in _scope_levels(by_name[callee], called or {}).items():
                if level > max(grant.get(scope, 0), grant.get("*", 0)):
                    findings.append((where, scope, f"{where} grants {scope} level {grant.get(scope, 0)} to {callee}, whose job `{name}` asks {level}"))
    return findings


def _permission_edge(grant: dict, asks: dict) -> list[tuple[str, dict]]:
    callee = {"on": {"workflow_call": {}}, "jobs": {"j": {"permissions": asks, "runs-on": "ubuntu-latest"}}}
    caller = {"jobs": {"c": {"uses": "./.github/workflows/callee.yml", "permissions": grant}}}
    return [("caller.yml", caller), ("callee.yml", callee)]


@pytest.mark.parametrize(
    ("grant", "asks", "findings"),
    [
        pytest.param({"contents": "read"}, {"contents": "read"}, False, id="equal"),
        pytest.param({"contents": "read", "issues": "write"}, {"issues": "write"}, False, id="the-union-is-granted"),
        pytest.param({"contents": "read"}, {"issues": "write"}, True, id="a-scope-the-caller-lacks"),
        pytest.param({"issues": "read"}, {"issues": "write"}, True, id="a-lower-level"),
        pytest.param({}, {"contents": "read"}, True, id="nothing-granted"),
    ],
)
def test_the_permission_edge_reader_reds_a_caller_that_grants_too_little(
    grant: dict, asks: dict, findings: bool
) -> None:
    assert bool(_callers_granting_too_little(_permission_edge(grant, asks))) is findings


def test_every_caller_grants_the_union_of_what_the_workflow_it_calls_asks_for() -> None:
    """The release readiness gate calls `acceptance.yml`, `scan-binaries.yml` and
    `shell-activation-deep.yml`, each asking `contents: read`; cargo-dist's `release.yml` grants
    `custom-docker-publish` what `docker-publish.yml`'s `build` job asks for, `artifact-metadata`
    included (once a known gap: the next tag would have failed at startup)."""
    documents = _documents()
    edges = _call_edges(documents)
    for callee in ("acceptance.yml", "scan-binaries.yml", "shell-activation-deep.yml"):
        assert ("release-readiness.yml", callee) in edges, (
            f"release-readiness.yml no longer calls {callee} — the edge this test exists for is gone"
        )
    assert ("release.yml", "docker-publish.yml") in edges, "release.yml no longer calls docker-publish.yml"
    findings = [message for _where, _scope, message in _callers_granting_too_little(documents)]
    assert not findings, (
        f"{findings}. A called workflow's jobs may not ask for more than the job that calls it grants; "
        f"the run fails at startup, before any job, and no job's `if:` helps"
    )


# ---------------------------------------------------------------------------
# Release readiness: the gate a tag needs, on the bytes cargo-dist would publish
# ---------------------------------------------------------------------------

READINESS_GATE = WORKFLOW_DIR / "readiness-gate.yml"
RELEASE_WORKFLOW = WORKFLOW_DIR / "release.yml"
DIST_INSTALLER = re.compile(r"cargo-dist/releases/download/v([0-9.]+)/cargo-dist-installer")


def _dist_config() -> dict:
    import tomllib

    return tomllib.loads((ROOT / "dist-workspace.toml").read_text(encoding="utf-8"))["dist"]


def _readiness_matrix(job: str) -> list[dict]:
    return _load(RELEASE_READINESS)["jobs"][job]["strategy"]["matrix"]["include"]


def test_release_readiness_is_dispatch_only_and_names_a_bypass_in_its_run_name() -> None:
    document = _load(RELEASE_READINESS)
    on = _on(RELEASE_READINESS)
    assert set(on) == {"workflow_dispatch"}, f"release-readiness.yml `on` is {sorted(on)}: only a dispatch may start the gate"
    inputs = on["workflow_dispatch"]["inputs"]
    assert inputs["bypass"]["type"] == "boolean" and inputs["bypass"]["default"] is False, inputs["bypass"]
    assert inputs["reason"]["type"] == "string", inputs["reason"]
    run_name = document["run-name"]
    assert run_name.startswith("readiness ${{ github.sha }}"), run_name
    assert "BYPASS: {0}" in run_name and "inputs.reason" in run_name and "inputs.bypass" in run_name, (
        f"run-name {run_name!r} does not carry ' BYPASS: <reason>' when bypass is set; "
        f"scripts/release_readiness_gate.py names a bypass from it"
    )


def _uploads_without_overwrite(documents: list[tuple[str, dict]]) -> list[str]:
    """`upload-artifact` steps that keep the action's default of refusing a name the run holds.

    A rerun of a job (or of the whole run) uploads its names again; the default turns that into
    a red upload after green tests.
    """
    return [
        f"{where}[{index}] uploads {(step.get('with') or {}).get('name')!r} without `overwrite: true`"
        for where, _, job in _jobs(documents)
        for index, step in enumerate(job.get("steps") or [])
        if str(step.get("uses", "")).startswith("actions/upload-artifact@") and not _truthy_input(step, "overwrite")
    ]


@pytest.mark.parametrize(
    ("overwrite", "findings"),
    [
        pytest.param("", True, id="the-default-refuses-a-held-name"),
        pytest.param("          overwrite: false\n", True, id="false"),
        pytest.param("          overwrite: true\n", False, id="true"),
    ],
)
def test_the_upload_reader_reds_a_name_a_rerun_cannot_replace(overwrite: str, findings: bool) -> None:
    upload = f"      - uses: actions/upload-artifact@v7\n        with:\n          name: x\n{overwrite}"
    assert bool(_uploads_without_overwrite(_corpus(upload))) is findings


def _template_owned(path: Path) -> bool:
    return path.read_text(encoding="utf-8").startswith("# ocx-baseline: template-owned file")


def test_every_upload_replaces_its_own_earlier_copy() -> None:
    """A failed job is rerun in place and a rerun-all repeats every upload; either one fails on the
    second copy of a name unless the upload replaces the first. Exempt: a file whose first line is the
    conventions template's banner (changed upstream, not here) and `release.yml` (cargo-dist writes it
    and `dist generate --check` fails a hand edit)."""
    swept = _documents([path for path in WORKFLOWS if path.name not in GENERATED_WORKFLOWS and not _template_owned(path)])
    for action in sorted((ROOT / ".github" / "actions").glob("*/action.yml")):
        if not _template_owned(action):
            steps = _load(action)["runs"].get("steps") or []
            swept.append((f"actions/{action.parent.name}", {"jobs": {"composite": {"steps": steps}}}))
    uploads = [
        step for _, _, job in _jobs(swept) for step in job.get("steps") or [] if "upload-artifact" in str(step.get("uses", ""))
    ]
    assert len(uploads) >= 18, f"read {len(uploads)} uploads from {len(swept)} files; the repo has more"
    findings = _uploads_without_overwrite(swept)
    assert not findings, f"{findings}. Set `overwrite: true`; a rerun otherwise fails the upload of a name the run already holds"


def test_release_readiness_publishes_nothing() -> None:
    """The dry run must stay a dry run: read-only tokens, no `host`-style step, no secret inherit."""
    document = _load(RELEASE_READINESS)
    assert document.get("permissions") == {}, document.get("permissions")
    text = RELEASE_READINESS.read_text(encoding="utf-8")
    for needle in ("gh release", "git push", "git tag", "docker push", "ocx package push", "secrets: inherit", "dist host"):
        assert needle not in text, f"release-readiness.yml contains `{needle}` - the gate publishes nothing"
    for name, job in document["jobs"].items():
        assert "write" not in str(job.get("permissions") or {}), f"`{name}` asks a write permission"
        for step in job.get("steps") or []:
            uses = str(step.get("uses", ""))
            assert "action-gh-release" not in uses and "login-action" not in uses, f"`{name}` uses {uses}"


def test_release_readiness_builds_every_dist_target_and_tests_six_platforms() -> None:
    targets = _dist_config()["targets"]
    built = sorted(entry["target"] for entry in _readiness_matrix("build"))
    assert built == sorted(targets), f"release-readiness.yml builds {built}, dist-workspace.toml builds {sorted(targets)}"
    expected = {
        "x86_64-unknown-linux-gnu": ("ubuntu-latest", "linux"),
        "aarch64-unknown-linux-gnu": ("ubuntu-24.04-arm", "linux"),
        "aarch64-apple-darwin": ("macos-latest", "macos"),
        "x86_64-apple-darwin": ("macos-15-intel", "macos"),
        "x86_64-pc-windows-msvc": ("windows-latest", "windows"),
        "aarch64-pc-windows-msvc": ("windows-11-arm", "windows"),
    }
    jobs = _load(RELEASE_READINESS)["jobs"]
    acceptance = _readiness_matrix("acceptance")
    assert {entry["target"]: (entry["os"], entry["limits"]) for entry in acceptance} == {
        target: (os_label, f"test/limits/{kind}-release") for target, (os_label, kind) in expected.items()
    }, acceptance
    assert {entry["target"]: entry["os"] for entry in _readiness_matrix("stage")} == {t: v[0] for t, v in expected.items()}
    for entry in acceptance:
        assert entry["no-docker"] is (not entry["os"].startswith("ubuntu")), entry
    call = jobs["acceptance"]
    assert call["uses"] == "./.github/workflows/acceptance.yml", call.get("uses")
    assert call["with"]["release-binary"] is True, "the shards must run with OCX_TEST_RELEASE_BINARY=1"
    assert call["with"]["binary"] == "artifact:readiness-bin-${{ matrix.target }}", call["with"]["binary"]
    assert call["with"]["limits-dir"] == "${{ matrix.limits }}", call["with"]["limits-dir"]
    assert set(_needs(call)) == {"stage", "scan"}, _needs(call)
    assert jobs["scan"]["uses"] == SCAN_WORKFLOW_USES and "build" in _needs(jobs["scan"])


def test_release_builds_windows_arm64_natively_on_the_runner_readiness_tests_it_on() -> None:
    """dist cross-builds (xwin, on ubuntu) every target without a `github-custom-runners` entry, so
    the zip readiness executes natively on `windows-11-arm` would not be the zip `release.yml` ships.
    `release.yml` reads its build runners from `dist plan` at run time, so the config is the source."""
    target = "aarch64-pc-windows-msvc"
    readiness = {entry["target"]: entry["runner"] for entry in _readiness_matrix("build")}[target]
    entry = _dist_config().get("github-custom-runners", {}).get(target)
    assert isinstance(entry, dict), f"dist-workspace.toml has no `github-custom-runners.{target}` table: {entry!r}"
    assert entry.get("runner") == readiness, f"release builds {target} on {entry.get('runner')!r}, readiness on {readiness!r}"
    assert entry.get("host") == target, f"host {entry.get('host')!r}: dist cross-builds unless the runner's host is {target}"


# The `[[tool]]` rows of ocx.lock are the leaves a platform can install; a target maps onto
# the lock's `os/arch` platform keys (`linux/amd64+libc.glibc` is a leaf of `linux/amd64`).
_LOCK_OS = {"apple-darwin": "darwin", "unknown-linux-gnu": "linux", "pc-windows-msvc": "windows"}
_LOCK_ARCH = {"x86_64": "amd64", "aarch64": "arm64"}


def _lock_platform(target: str) -> str:
    arch, _, rest = target.partition("-")
    return f"{_LOCK_OS[rest]}/{_LOCK_ARCH[arch]}"


def _lock_tools() -> list[dict]:
    import tomllib

    return tomllib.loads((ROOT / "ocx.lock").read_text(encoding="utf-8"))["tool"]


def _lock_tools_without_leaf(platform: str) -> dict[str, list[str]]:
    """Group -> tools whose `ocx.lock` row has no leaf for `platform`."""
    missing: dict[str, list[str]] = {}
    for tool in _lock_tools():
        if not any(key.split("+")[0] == platform for key in tool["platforms"]):
            missing.setdefault(tool["group"], []).append(tool["name"])
    return missing


def test_a_readiness_leg_without_every_default_leaf_loads_only_a_group_that_has_them() -> None:
    """Loading the whole project on a platform missing a default tool's leaf exits 78 in setup-ocx."""
    legs = _readiness_matrix("acceptance")
    gaps = {_lock_platform(leg["target"]): _lock_tools_without_leaf(_lock_platform(leg["target"])) for leg in legs}
    assert any(gap.get("default") for gap in gaps.values()), (
        "ocx.lock has a leaf for every default tool on every readiness platform; `toolchain-group` has no reason to exist"
    )
    findings = []
    for leg in legs:
        gap = gaps[_lock_platform(leg["target"])]
        group = leg.get("toolchain-group", "")
        if not gap.get("default"):
            if group:
                findings.append(f"{leg['name']}: every default leaf exists, yet `toolchain-group` is {group!r}")
        elif not group:
            findings.append(f"{leg['name']}: no default leaf for {gap['default']} but `toolchain-group` is empty")
        elif group not in {t["group"] for t in _lock_tools()}:
            findings.append(f"{leg['name']}: `toolchain-group` {group!r} holds no tool in ocx.lock")
        elif gap.get(group):
            findings.append(f"{leg['name']}: group {group!r} has no leaf for {gap[group]} either")
    assert not findings, findings
    with_ = _load(RELEASE_READINESS)["jobs"]["acceptance"]["with"]
    assert with_.get("toolchain-group") == "${{ matrix.toolchain-group }}", with_.get("toolchain-group")


def test_the_acceptance_shard_loads_the_whole_project_unless_given_a_toolchain_group() -> None:
    """Empty `toolchain-group` keeps the action's own project load; a value swaps it for `pull -g` and `env -g`."""
    steps = _repo_job(ACCEPTANCE, "shard")["steps"]
    setup = next(s for s in steps if str(s.get("uses", "")).startswith("ocx-sh/setup-ocx@"))
    assert setup["with"]["project"] == "${{ inputs.toolchain-group == '' && 'ocx.toml' || '' }}", setup["with"]
    group_step = next(s for s in steps if "ocx --project ocx.toml pull" in str(s.get("run", "")))
    assert group_step["if"] == "${{ inputs.toolchain-group != '' }}", group_step.get("if")
    run = group_step["run"]
    assert 'pull -g "$GROUP"' in run and 'env -g "$GROUP" --ci=github' in run, run
    assert group_step["env"] == {"GROUP": "${{ inputs.toolchain-group }}"}, group_step.get("env")
    assert steps.index(setup) < steps.index(group_step) < next(
        i for i, s in enumerate(steps) if s.get("name") == "Run Acceptance Tests"
    )


def test_the_release_gate_installs_the_dist_version_the_config_pins() -> None:
    version = _dist_config()["cargo-dist-version"]
    for workflow in (RELEASE_WORKFLOW, WORKFLOW_DIR / "verify-release-ci.yml", RELEASE_READINESS):
        found = set(DIST_INSTALLER.findall(workflow.read_text(encoding="utf-8")))
        assert found == {version}, f"{workflow.name} installs dist {sorted(found)}, dist-workspace.toml pins {version}"


# The verdict step's script, run for real: each case is a set of `needs` results the gate could meet.
_GO = dict.fromkeys(("PLAN", "BUILD", "SCAN", "STAGE", "ACCEPTANCE", "SHELL_ACTIVATION"), "success")


def _verdict(tmp_path: Path, **overrides: str) -> tuple[int, str]:
    verdict = _load(RELEASE_READINESS)["jobs"]["verdict"]
    (step,) = verdict["steps"]
    summary = tmp_path / "summary.md"
    summary.write_text("", encoding="utf-8")
    env = {**os.environ, "BYPASS": "false", "REASON": "", "ACTOR": "dispatcher", "SHA": "a" * 40, **_GO}
    env.update(overrides)
    env["GITHUB_STEP_SUMMARY"] = str(summary)
    done = subprocess.run(["bash", "-c", step["run"]], env=env, capture_output=True, text=True, encoding="utf-8", check=False)
    return done.returncode, summary.read_text(encoding="utf-8") + done.stdout + done.stderr


def test_the_verdict_job_always_runs_and_waits_for_every_other_job() -> None:
    jobs = _load(RELEASE_READINESS)["jobs"]
    verdict = jobs["verdict"]
    assert "always()" in str(verdict["if"]), verdict["if"]
    assert set(_needs(verdict)) == set(jobs) - {"verdict"}, f"verdict needs {_needs(verdict)}"
    env = verdict["steps"][0]["env"]
    for need in _needs(verdict):
        assert f"needs.{need}.result" in " ".join(map(str, env.values())), f"the verdict never reads `{need}`'s result"


def test_the_verdict_is_green_only_when_every_job_succeeded(tmp_path: Path) -> None:
    code, out = _verdict(tmp_path)
    assert code == 0 and "Verdict: GO" in out, out


@pytest.mark.parametrize("job", sorted(_GO))
@pytest.mark.parametrize("result", ["failure", "cancelled", "skipped"])
def test_the_verdict_goes_red_when_any_job_did_not_succeed(tmp_path: Path, job: str, result: str) -> None:
    """The forced-red proof: one failed, cancelled or skipped job is NO-GO, naming it."""
    code, out = _verdict(tmp_path, **{job: result})
    assert code == 1, out
    assert "NO-GO" in out and f"{result}" in out, out


def test_a_bypass_with_a_reason_is_green_and_writes_the_reason_down(tmp_path: Path) -> None:
    skipped = dict.fromkeys(_GO, "skipped")
    code, out = _verdict(tmp_path, BYPASS="true", REASON="runner outage, hotfix", **skipped)
    assert code == 0, out
    assert "BYPASS" in out and "runner outage, hotfix" in out and "dispatcher" in out, out


@pytest.mark.parametrize("reason", ["", "   ", "\n"])
def test_a_bypass_without_a_reason_is_red(tmp_path: Path, reason: str) -> None:
    code, out = _verdict(tmp_path, BYPASS="true", REASON=reason, **dict.fromkeys(_GO, "skipped"))
    assert code == 1 and "needs a reason" in out, out


def test_every_test_job_of_the_gate_skips_under_bypass() -> None:
    jobs = _load(RELEASE_READINESS)["jobs"]
    for name in ("plan", "build", "stage", "acceptance", "shell-activation-deep"):
        assert "!inputs.bypass" in str(jobs[name].get("if")), f"`{name}` still runs under bypass: {jobs[name].get('if')}"
    assert "if" not in jobs["scan"], "scan skips through `needs: build`; an `if:` of its own is a second switch"


def test_the_readiness_gate_workflow_runs_the_script_on_the_tagged_commit() -> None:
    document = _load(READINESS_GATE)
    on = _on(READINESS_GATE)
    assert set(on) == {"workflow_call", "workflow_dispatch"}, sorted(on)
    assert "plan" in on["workflow_call"]["inputs"], "cargo-dist passes `plan` to every custom job"
    assert on["workflow_dispatch"]["inputs"]["sha"]["type"] == "string"
    assert document["permissions"] == {"contents": "read", "actions": "read"}, document["permissions"]
    (job,) = document["jobs"].values()
    run = " ".join(step.get("run", "") for step in job["steps"])
    assert 'scripts/release_readiness_gate.py --sha "$SHA"' in run, run
    assert job["steps"][-1]["env"]["SHA"] == "${{ inputs.sha || github.sha }}", job["steps"][-1]["env"]
    for step in job["steps"]:
        assert "continue-on-error" not in step and "|| true" not in step.get("run", ""), f"step {step.get('name')} can pass a red gate"
    assert "if" not in job, "a skipped gate reads as a pass to `host`"


def test_release_host_needs_the_readiness_gate() -> None:
    """Red readiness-gate -> `host` never runs -> nothing is released."""
    config = _dist_config()
    assert config["local-artifacts-jobs"] == ["./verify-version", "./readiness-gate"], config.get("local-artifacts-jobs")
    assert "plan-jobs" not in config, (
        f"`plan-jobs` is {config.get('plan-jobs')}: dist makes every job wait on a plan job, and `host` reads their skip as a pass"
    )
    assert config["github-custom-job-permissions"]["readiness-gate"] == {"contents": "read", "actions": "read"}
    jobs = _load(RELEASE_WORKFLOW)["jobs"]
    gate = "custom-readiness-gate"
    assert jobs[gate]["uses"] == "./.github/workflows/readiness-gate.yml", jobs[gate]
    assert jobs[gate]["permissions"] == {"contents": "read", "actions": "read"}, jobs[gate]["permissions"]
    assert gate in _needs(jobs["host"]), f"release.yml `host` needs {_needs(jobs['host'])}, not `{gate}`"
    clause = f"(needs.{gate}.result == 'skipped' || needs.{gate}.result == 'success')"
    assert clause in str(jobs["host"].get("if", "")), f"`host.if` does not read `{gate}`'s result: {jobs['host'].get('if')}"
    assert gate in _needs(jobs["custom-scan-binaries"]), "the scan must not run after a red readiness gate"
    assert _needs(jobs[gate]) == ["plan"], f"`{gate}` needs {_needs(jobs[gate])}: a failed prerequisite would skip it, and `host` accepts a skip"


#: The `if:` dist renders on a `local-artifacts-jobs` entry: it runs for a publishing tag (or an
#: upload-mode pull request) and is skipped otherwise. Any other `if:` on a release gate, `false`
#: included, skips it on the tag it exists to check, and `host` reads that skip as a pass.
PUBLISH_PREDICATE = (
    "${{ needs.plan.outputs.publishing == 'true' || fromJson(needs.plan.outputs.val).ci.github.pr_run_mode == 'upload' }}"
)
RELEASE_GATES = ("custom-verify-version", "custom-readiness-gate")


def _host_clause(need: str) -> str:
    if need == "plan":
        return "needs.plan.result == 'success'"
    return f"(needs.{need}.result == 'skipped' || needs.{need}.result == 'success')"


def _host_condition_findings(jobs: dict) -> list[str]:
    """Needs of `host` whose result `host.if` ignores, and release gates not running on the publish predicate."""
    condition = str(jobs["host"].get("if", ""))
    findings = [
        f"`host.if` does not read `{need}`: {_host_clause(need)!r} is missing"
        for need in _needs(jobs["host"])
        if _host_clause(need) not in condition
    ]
    return findings + [
        f"`{gate}` has `if: {jobs[gate].get('if')!r}`, not the publish predicate"
        for gate in RELEASE_GATES
        if jobs[gate].get("if") != PUBLISH_PREDICATE
    ]


def _drop_verify_version_from_host_if(jobs: dict) -> None:
    clause = f" && {_host_clause('custom-verify-version')}"
    assert clause in jobs["host"]["if"], "the clause to plant the defect by removing is not in host.if"
    jobs["host"]["if"] = jobs["host"]["if"].replace(clause, "")


def _verify_version_never_runs(jobs: dict) -> None:
    assert jobs["custom-verify-version"]["if"] == PUBLISH_PREDICATE
    jobs["custom-verify-version"]["if"] = False  # YAML `if: false` loads as a bool


@pytest.mark.parametrize("mutate", [_drop_verify_version_from_host_if, _verify_version_never_runs], ids=lambda f: f.__name__)
def test_the_release_condition_reader_reds_a_gate_host_no_longer_reads(mutate) -> None:
    jobs = copy.deepcopy(_load(RELEASE_WORKFLOW)["jobs"])
    assert not _host_condition_findings(jobs), "the unmutated copy must be clean for the red below to mean anything"
    mutate(jobs)
    findings = _host_condition_findings(jobs)
    assert findings and all("custom-verify-version" in finding for finding in findings), findings


def test_no_job_upstream_of_host_can_fail_into_a_skip() -> None:
    """`host.if` reads a `skipped` need as a pass, and GitHub skips a job whose own need failed.
    So a job `host` waits on only through another job (a `plan-jobs` entry, which dist makes every
    job wait on) turns its own red into a skipped chain and `host` publishes. Every job upstream of
    `host` must be one `host` reads itself, apart from `plan`, which `host` requires to succeed."""
    jobs = _load(RELEASE_WORKFLOW)["jobs"]
    direct = set(_needs(jobs["host"]))
    assert set(RELEASE_GATES) <= direct, sorted(direct)
    findings = _host_condition_findings(jobs)
    assert not findings, f"{findings}: `host` publishes past a red need, or a gate never runs on the tag it checks"
    upstream: set[str] = set()
    pending = list(direct)
    while pending:
        for need in _needs(jobs[pending.pop()]):
            if need not in upstream:
                upstream.add(need)
                pending.append(need)
    assert "plan" in upstream, f"walked {sorted(upstream)} and never reached `plan`: the reader stopped early"
    hidden = sorted(upstream - direct - {"plan"})
    assert not hidden, (
        f"release.yml jobs {hidden} sit upstream of `host` without being in its `needs`: when one fails, "
        f"its dependents are skipped, `host` accepts a skip, and publishes"
    )


def test_every_input_release_yml_passes_is_declared_by_the_workflow_it_calls() -> None:
    """cargo-dist hands `plan` to every `local-artifacts-jobs` caller; a callee that does not declare
    the input fails the run at startup, before any job, so no tag would build."""
    documents = dict(_documents())
    callers = 0
    for name, job in documents["release.yml"]["jobs"].items():
        callee = _local_callee(job)
        if callee is None:
            continue
        callers += 1
        triggers = documents[callee].get("on", documents[callee].get(True))
        declared = set((triggers["workflow_call"] or {}).get("inputs") or {})
        undeclared = sorted(set(job.get("with") or {}) - declared)
        assert not undeclared, f"release.yml `{name}` passes {undeclared} to {callee}, which declares {sorted(declared)}"
    assert callers >= 6, f"release.yml has {callers} callers of a repo workflow; the reader lost some"


def test_the_docker_publish_grant_covers_what_the_called_workflow_asks() -> None:
    asked = _load(WORKFLOW_DIR / "docker-publish.yml")["jobs"]["build"]["permissions"]
    granted = _load(RELEASE_WORKFLOW)["jobs"]["custom-docker-publish"]["permissions"]
    assert {scope for scope, level in asked.items() if level == "write"} <= {s for s, v in granted.items() if v == "write"}, (
        asked,
        granted,
    )


def test_claude_write_answers_only_what_the_template_responder_does_not() -> None:
    """The template's `claude.yml` answers every `@claude` comment and every pull request; only its
    issue handler holds `contents: write`, and only for an `@claude` comment on an issue that is not
    a pull request. `claude-write.yml` is the other write-capable responder, so it must not answer the
    same event twice: its comment trigger is a different phrase that excludes `@claude`, and its
    `issues` events are ones the template does not subscribe to."""
    template_on = _on(CLAUDE_TEMPLATE)
    template_ifs = " ".join(str(job.get("if", "")) for job in _load(CLAUDE_TEMPLATE)["jobs"].values())
    write = _load(CLAUDE_WRITE)
    condition = str(write["jobs"]["claude"]["if"])
    assert "issues" not in template_on, "the template responder now takes `issues` events; claude-write.yml overlaps it"
    assert "'@claude'" in template_ifs, "the template responder no longer keys on `@claude`; re-derive the disjointness"
    assert "'/claude-write'" in condition and "!contains(github.event.comment.body, '@claude')" in condition, (
        "claude-write.yml answers a comment that holds `@claude`, which the template responder answers too"
    )
    permissions = write["jobs"]["claude"]["permissions"]
    assert permissions["contents"] == "write", f"claude-write.yml permissions are {permissions}"
    template_writers = {
        name: job
        for name, job in _load(CLAUDE_TEMPLATE)["jobs"].items()
        if "write" in str(job.get("permissions", {}).get("contents", ""))
    }
    assert set(template_writers) <= {"issue"}, (
        f"the template jobs {sorted(set(template_writers) - {'issue'})} gained `contents: write`; "
        "only its issue handler may, and claude-write.yml's disjointness rests on that"
    )
    for name, job in template_writers.items():
        gate = str(job.get("if", ""))
        assert "'@claude'" in gate and "!github.event.issue.pull_request" in gate, (
            f"the template `{name}` job holds `contents: write` without keying on an `@claude` issue comment; "
            "re-derive its disjointness from claude-write.yml"
        )
    checkout = next(s for s in write["jobs"]["claude"]["steps"] if str(s.get("uses", "")).startswith("actions/checkout@"))
    assert checkout["with"]["persist-credentials"] is False, (
        "claude-write.yml's checkout keeps its token; the action replaces it before pushing, so keep none"
    )
