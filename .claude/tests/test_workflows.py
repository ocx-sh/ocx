"""Structural tests for `.github/workflows/*.yml` (plan_crate_split_workspace.md C-016, C-023).

Two shapes the crate split can break without any workflow going red:

- a `paths:` / `paths-ignore:` entry whose last file moved. The workflow then
  never fires, and a workflow that never fires is a green nobody can tell
  from a pass. Every entry must match at least one tracked file.
- `verify-deep.yml`'s trigger shape, which is what makes it a manual gate:
  NO `pull_request` trigger and no job guarding on a `github.event.pull_request`
  context (a deep run is ≈113 runner-minutes against basic's ≈18, so it is
  opt-in per branch), `workflow_dispatch` — now the only way a human runs it —
  and `push` to `main` (the one full run over the tree a rebase-only merge
  actually landed, DX-16).
- the merge-queue shape of BOTH tiers — `merge_group` and a
  `cancel-in-progress` that leaves queue runs alone — since either workflow's
  checks can be marked required.
- the shell zoo's checkout and its task graph, which are one contract: that
  job omits `submodules:` on purpose, so a task it runs must not reach
  `:website:schema:default` and through it a cargo build of a workspace whose
  `external/` patch sources are not on disk.
- the tier partition (D5): `verify-basic.yml` is what a pull request pays
  for, so it runs on Linux only, and the Windows and macOS unit legs are
  `verify-deep.yml`'s `build` matrix. A leg re-added to basic is a cost every
  push pays twice; a leg dropped from deep's matrix is a `cfg(windows)` /
  `cfg(target_os = "macos")` arm nothing compiles anywhere.

- the Bazel remote-cache lane policy (plan_pr637_round2.md C-003). Every same-repo
  lane holding the secret reads and writes the shared cache: each `bazel-cache-rc`
  step passes `BAZEL_CACHE_WRITE_AUTH` by name with no event predicate, every Bazel
  lane has such a step before its first lane step and an `if: always()` cleanup
  after, no workflow spells the upload flag, and a caller of a reusable workflow
  holding a cache step passes the secret by name. Every one of those assertions is
  structural; none dials the cache.

- S-016's evidence predicate: which `TestResult` counts as a **remote** cache hit.
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

import json
import os
import re
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
# The merge-queue shape belongs to both tiers, not to deep alone.
TIERS = (VERIFY_BASIC, VERIFY_DEEP)
# Runner-label prefixes: `ubuntu-latest`, `macos-14`, `windows-2025` all
# belong to their family, so a version pin never reads as a lost OS.
OS_FAMILIES = ("ubuntu", "macos", "windows")

# No deep job may condition on a pull request any more: the workflow does not
# fire on one, so such a guard is either dead text or evidence the trigger came
# back. The whole `github.event.pull_request` context is the needle, so the
# former draft guard and any other shape of it read the same.
PULL_REQUEST_CONTEXT = "github.event.pull_request"
# Both checks below this point are negative — "`pull_request` is not among the
# triggers", "no job's `if:` names that context" — and a negative over a
# collection passes identically when the collection is empty, mis-keyed, or
# read by a loader that returned nothing. The reader self-test that used to
# floor this file went with the draft-guard parser it proved, so these two sets
# are the floor instead: each check asserts what it *saw* before asserting what
# it did not. They are exact rather than a count, so the failure names the
# drift. Adding a trigger or a job to the deep tier reds one of them, which is
# the review it deserves.
DEEP_TRIGGERS = frozenset({"workflow_dispatch", "workflow_call", "push", "merge_group", "schedule"})
DEEP_JOBS = frozenset(
    {
        "index-conformance-drift",
        "build",
        "cross",
        "unit-native",
        "acceptance",
        "acceptance-merge",
        "acceptance-native",
        "test-results",
        "satellite-verify",
        "satellite-contract",
    }
)
# A cancelled merge-queue run counts as a failed check and drops the entry
# from the queue, so queue runs are never cancelled; `main` keeps its carve-out.
MERGE_QUEUE_SAFE_CANCEL = (
    "${{ !startsWith(github.ref, 'refs/heads/gh-readonly-queue/') && github.ref != 'refs/heads/main' }}"
)
# Exact, not "contains the schedule clause": any wider `if:` can keep that
# clause and still admit no dispatch, which leaves the cross-build unreachable.
DEEP_BUILD_IF = "github.event_name != 'schedule' || inputs.full"


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
# C-016: verify-deep.yml is a manual gate that survives the merge queue
# ---------------------------------------------------------------------------


def test_deep_does_not_run_on_pull_requests() -> None:
    """The deep tier is opt-in, not per-PR.

    A deep run is ≈113 runner-minutes against `verify-basic.yml`'s ≈18, most of
    it Windows and macOS minutes, and every pull request paid it on every push.
    Re-adding the trigger is a real decision about cost and about what the
    basic tier is for — so it reds here rather than arriving in a diff nobody
    reads. The coverage this gives up is named in subsystem-ci.md.
    """
    on = _on(VERIFY_DEEP)
    assert "pull_request" not in on, (
        f"verify-deep.yml `on` has {sorted(on)}, including `pull_request` — the deep tier "
        f"is opt-in per branch (`gh workflow run verify-deep.yml --ref <branch>`), and a "
        f"pull request pays for the basic tier only. If this is deliberate, this assertion "
        f"and subsystem-ci.md's 'Verification tiers' table change together"
    )
    # The floor: the assertion above is a negative, and an `on:` block this
    # reader got back empty or under a key it does not know would satisfy it
    # while proving nothing.
    assert set(on) == DEEP_TRIGGERS, (
        f"verify-deep.yml `on` is {sorted(on)}, expected exactly {sorted(DEEP_TRIGGERS)} — "
        f"a trigger gained or lost changes when the deep tier runs, and the `pull_request` "
        f"assertion above is only evidence if this reader saw the real `on:` block"
    )


def test_deep_is_dispatched_by_hand() -> None:
    """`workflow_dispatch` is now the load-bearing trigger: with `pull_request`
    gone it is the only way a branch gets Windows, macOS or full-acceptance
    coverage before it lands. Lose it and the deep tier runs nowhere but on
    `main`, after the merge."""
    on = _on(VERIFY_DEEP)
    assert "workflow_dispatch" in on, (
        f"verify-deep.yml `on` has {sorted(on)} and no `workflow_dispatch` — the deep tier "
        f"no longer runs per pull request, so this is the only way a human runs it on a "
        f"branch; without it nothing deep happens until the merge lands on `main`"
    )


def test_deep_runs_on_every_push_to_main() -> None:
    """Merges are rebase-only and there is no queue (DX-16): a PR verified at
    its own head lands on a `main` that moved since, so the push is the one
    full run over the tree that actually landed."""
    on = _on(VERIFY_DEEP)
    assert "push" in on, (
        f"verify-deep.yml `on` has {sorted(on)} and no `push` — nothing full-suite runs on "
        f"`main` after a rebase-only merge (DX-16)"
    )
    branches = (on["push"] or {}).get("branches") or []
    assert "main" in branches, f"verify-deep.yml `on.push.branches` is {branches}, missing `main`"


@pytest.mark.parametrize("workflow", TIERS, ids=lambda workflow: workflow.name)
def test_each_tier_triggers_in_the_merge_queue(workflow: Path) -> None:
    """Both tiers, because either one can be marked required: a required check
    that never runs on the queued merge commit blocks the queue forever, and
    basic's `merge_group: {}` exists for exactly that."""
    on = _on(workflow)
    assert "merge_group" in on, (
        f"{workflow.name} `on` has {sorted(on)} and no `merge_group` — a required check that "
        f"never runs on the queued merge commit blocks the queue forever"
    )


def test_no_deep_job_guards_on_a_pull_request() -> None:
    """The other half of the trigger removal, and the half a diff hides.

    Every job used to carry `github.event_name != 'pull_request' ||
    github.event.pull_request.draft == false`. With no `pull_request` trigger
    that term is always true — dead text that reads like a live gate, and the
    shape someone restoring the trigger would find already in place. The needle
    is the whole `github.event.pull_request` context rather than that one
    expression, so a rewritten guard reds too.
    """
    jobs = _load(VERIFY_DEEP)["jobs"]
    # The floor, before the negative below: a job set this reader got back
    # empty — or six jobs it read past because the loader returned them under
    # some other shape — sweeps clean and says nothing.
    assert set(jobs) == DEEP_JOBS, (
        f"verify-deep.yml jobs are {sorted(jobs)}, expected exactly {sorted(DEEP_JOBS)} — a "
        f"job added here must be checked for a pull-request guard by the assertion below, "
        f"and one removed means this sweep now reads less than it claims to"
    )
    guarded = {
        name: job["if"] for name, job in jobs.items() if PULL_REQUEST_CONTEXT in str(job.get("if", ""))
    }
    assert not guarded, (
        f"verify-deep.yml jobs whose `if:` reads `{PULL_REQUEST_CONTEXT}`: {guarded}. The "
        f"workflow has no `pull_request` trigger, so that context is never populated: the "
        f"term is dead text if the trigger is still gone, and a re-added trigger belongs "
        f"in `test_deep_does_not_run_on_pull_requests` above, not here"
    )


@pytest.mark.parametrize("workflow", TIERS, ids=lambda workflow: workflow.name)
def test_each_tier_cancel_in_progress_spares_the_merge_queue(workflow: Path) -> None:
    value = _load(workflow)["concurrency"]["cancel-in-progress"]
    assert value == MERGE_QUEUE_SAFE_CANCEL, (
        f"{workflow.name} `concurrency.cancel-in-progress` is `{value}`, expected "
        f"`{MERGE_QUEUE_SAFE_CANCEL}` — a cancelled merge-queue run is a failed check and "
        f"drops the entry from the queue"
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


def test_the_claude_structural_tests_run_in_ci() -> None:
    """This file included. No workflow invoked `task claude:tests`, so every
    shape asserted here — the D5 partition below among them — held on a
    developer's machine and nowhere else: a leg deleted from deep's matrix
    was green in CI.

    Presence is half of it: a step whose failure is tolerated or skipped is
    the same green as no step. One carrier must run unconditionally and be
    able to fail its job; further conditional carriers are free.
    """
    carriers = [
        (workflow.name, name, _disarmed(job, step))
        for workflow in WORKFLOWS
        for name, job in (_load(workflow).get("jobs") or {}).items()
        for step in (job.get("steps") or [])
        if "task claude:tests" in str(step.get("run", ""))
    ]
    assert carriers, (
        "no workflow step runs `task claude:tests` — the structural tests of `.claude/` "
        "are then a gate nothing in CI executes, and this assertion is the one that says so"
    )
    assert any(not reasons for _, _, reasons in carriers), (
        f"every workflow step running `task claude:tests` is disarmed: "
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
    {".claude/artifacts/plan_crate_split_workspace.md: verify-basic.yml smoke-windows"}
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
# D5: basic runs on Linux only; deep's native jobs own macOS and Windows
# ---------------------------------------------------------------------------


def test_basic_runs_on_linux_only() -> None:
    """Strict on purpose: a list or a `${{ matrix.* }}` expression cannot be
    read as Linux here, so only a literal `ubuntu-*` label passes."""
    jobs = _load(VERIFY_BASIC)["jobs"]
    assert jobs, "verify-basic.yml has no jobs"
    off_linux = {
        name: job.get("runs-on")
        for name, job in jobs.items()
        if not str(job.get("runs-on", "")).startswith("ubuntu-")
    }
    assert not off_linux, (
        f"verify-basic.yml jobs not on a literal `ubuntu-*` runner: {off_linux}. Basic is the "
        f"tier a pull request pays for (D5); Windows and macOS unit coverage is "
        f"verify-deep.yml's `build` matrix, so a leg here runs the same suite twice on every "
        f"push to `main`"
    )


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
NATIVE_DEEP_JOBS = frozenset({"unit-native", "acceptance-native"})


def test_deep_build_job_is_the_one_linux_compile() -> None:
    """The `build` job's `if:` admits a dispatch, it runs on Linux, and the
    Linux unit run is skipped on a push to `main`, where `verify-basic.yml`'s
    `smoke` job already ran the identical `bazel:test:unit`."""
    build = _load(VERIFY_DEEP)["jobs"]["build"]
    assert build.get("if") == DEEP_BUILD_IF, (
        f"verify-deep.yml `build.if` is `{build.get('if')}`, expected `{DEEP_BUILD_IF}` — any "
        f"other shape can keep the schedule clause and still admit no dispatch, which leaves "
        f"a branch with no cross-build and so no Windows/macOS coverage at all (basic has "
        f"none, D5, and no pull request fires this workflow)"
    )
    assert build.get("runs-on") == "ubuntu-latest", (
        f"verify-deep.yml `build.runs-on` is `{build.get('runs-on')}` — every binary in this "
        f"workflow is compiled on one Linux host"
    )
    [unit] = [step for step in build["steps"] if str(step.get("run", "")).strip() == "task bazel:test:unit"]
    assert unit.get("if") == "github.event_name != 'push'", (
        f"verify-deep.yml `build` unit step has `if: {unit.get('if')}` — on a push to `main` "
        f"`verify-basic.yml` runs the same `bazel:test:unit` on the same tree, so the step must "
        f"be skipped there and run on every other trigger"
    )


def test_deep_cross_matrix_is_every_supported_target() -> None:
    """One `cross` leg per `task cross:targets` triple: a missing leg is a
    platform whose binary the run never builds, and nothing else would red."""
    vars_ = _load(ROOT / "taskfiles" / "cross.taskfile.yml")["vars"]
    targets = vars_["TARGETS"].split()
    assert len(targets) >= 8, f"read {targets} from cross.taskfile.yml TARGETS"
    matrix = _load(VERIFY_DEEP)["jobs"]["cross"]["strategy"]["matrix"]
    assert sorted(matrix["target"]) == sorted(targets), (
        f"verify-deep.yml `cross` matrix is {sorted(matrix['target'])}, "
        f"`task cross:targets` is {sorted(targets)}"
    )


def test_deep_non_linux_jobs_only_download_and_execute() -> None:
    """Every non-ubuntu job of verify-deep contains no build tooling.

    All compilation happens on the Linux `build` and `cross` jobs; a macOS or Windows
    runner that grows a `cargo`, `bazel` or Rust-toolchain step is paying
    10x-minute rates for a second compile and reintroducing the toolchain
    drift the cross-build removed.
    """
    jobs = _load(VERIFY_DEEP)["jobs"]
    native = {
        name: job
        for name, job in jobs.items()
        if any(not label.startswith("ubuntu-") for label in _runner_labels(job))
    }
    # The floor, before the negative below: a reader that found no native job
    # sweeps clean and says nothing.
    assert set(native) == NATIVE_DEEP_JOBS, (
        f"verify-deep.yml non-Linux jobs are {sorted(native)}, expected exactly "
        f"{sorted(NATIVE_DEEP_JOBS)}"
    )
    for name, job in native.items():
        labels = _runner_labels(job)
        missing = [family for family in ("macos", "windows") if not any(label.startswith(f"{family}-") for label in labels)]
        assert not missing, f"verify-deep.yml `{name}` runs on {labels}, no {missing} entry"
        assert "exclude" not in job["strategy"]["matrix"], f"verify-deep.yml `{name}` matrix has an `exclude`"
        offending = [
            step.get("name", step.get("uses", step.get("run")))
            for step in job["steps"]
            if _BUILD_TOOLING.search(yaml.safe_dump({k: v for k, v in step.items() if k != "name"}))
        ]
        assert not offending, (
            f"verify-deep.yml `{name}` runs on a non-Linux runner and has build steps {offending} — "
            f"non-Linux jobs download what `cross` cross-compiled and execute it, nothing more"
        )


def test_deep_linux_acceptance_is_four_shards_feeding_a_merge_job() -> None:
    """Sharding is only a coverage claim if the floor is judged over the merge."""
    jobs = _load(VERIFY_DEEP)["jobs"]
    acceptance, merge = jobs["acceptance"], jobs["acceptance-merge"]
    assert acceptance["runs-on"] == "ubuntu-latest"
    assert acceptance["strategy"]["matrix"]["shard"] == [1, 2, 3, 4], (
        f"verify-deep.yml `acceptance` shards are {acceptance['strategy']['matrix'].get('shard')}, "
        f"expected [1, 2, 3, 4]"
    )
    runs = [str(step.get("run", "")) for step in acceptance["steps"]]
    assert "task test:accept:native SHARD=${{ matrix.shard }}/4" in runs, (
        f"verify-deep.yml `acceptance` never runs its shard: {runs}"
    )
    needs = merge["needs"]
    assert (needs if isinstance(needs, list) else [needs]) == ["acceptance"], (
        f"verify-deep.yml `acceptance-merge` needs {needs}, expected `acceptance`"
    )
    assert "!cancelled()" in str(merge.get("if", "")), (
        "verify-deep.yml `acceptance-merge` has no `!cancelled()` — a red shard would skip the merge "
        "and the floor would never be judged over what did run"
    )
    assert any(str(step.get("run", "")).startswith("task test:accept:floor ") for step in merge["steps"]), (
        "verify-deep.yml `acceptance-merge` never runs `task test:accept:floor`"
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
_FLOOR_PARSER_GATE = "scripts/bazel_test_floor.py --bep"
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
    """
    proved: list[str] = []
    for name, document in _documents():
        for job_name, job in (document.get("jobs") or {}).items():
            runs = {str(step.get("run", "")).strip() for step in (job.get("steps") or [])}
            if _DURATION_PROOF in runs:
                proved.append(f"{name}:{job_name}")
            if "task rust:test:duration" not in runs:
                continue
            assert _DURATION_PROOF in runs, (
                f"{name}:{job_name} runs `task rust:test:duration` and never "
                f"`{_DURATION_PROOF}` — the budget parser is then proved nowhere the budget runs"
            )
    assert proved, (
        f"no workflow runs `{_DURATION_PROOF}` — the budget parser's "
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


# ---------------------------------------------------------------------------
# CI runs every step of `.verify:build-test` (B5R-4)
# ---------------------------------------------------------------------------

#: Phase steps a workflow other than `verify-basic.yml` runs, and the spelling
#: that file uses. Every row is asserted live below: an exemption whose target
#: is gone forbids nothing while reading as coverage, so a task dropped from
#: the phase, or a workflow that stopped running one, reds here.
_BUILD_TEST_ELSEWHERE = {
    "rust:license:check": ("verify-licenses.yml", "task rust:license:check"),
    "rust:license:deps": ("verify-licenses.yml", "task rust:license:deps"),
    "rust:license:notice:check": ("verify-licenses.yml", "task rust:license:notice:check"),
    # The deep workflow runs the acceptance suite. It once spelled this
    # `task test`, the SERIAL pytest entry point, then the Bazel lane (171
    # cached `sh_test` targets), and is now four pytest shards over the
    # cross-built binaries (`task test:accept:native`, floored by a merge job).
    # The local gate still runs the Bazel lane. `verify-basic.yml` is
    # deliberately not the home for it — that workflow is the fast PR gate and
    # the acceptance suite is the deep one's long half.
    "bazel:test:accept": ("verify-deep.yml", "task test:accept:native"),
    # The testcases the Bazel `rust_test` targets skip. The deep workflow's
    # macOS and Windows legs run the cross-built unit-test bundle, which is
    # expected to carry these (the old legs ran the whole workspace under
    # nextest) — where the uncovered ocx_util `pub` and the spawn outside
    # launch that added this step to `task verify` went red.
    "rust:test:bazel-skipped": ("verify-deep.yml", "task cross:test:run"),
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
    inside a longer task's name. A step tolerated by `continue-on-error` is
    not coverage — that green cannot be told from no step at all — unless it
    carries an `id` a later step of the same job reads, which is section 3 of
    subsystem-ci.md's deliberate shape: `verify-basic.yml`'s lint steps never
    block the test results and the job's last step exits 1 on their outcomes.

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
            if not re.match(rf"{re.escape(command)}(?![\w:-])", str(step.get("run", "")).strip()):
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

    `verify-basic.yml` re-lists `.verify:build-test`'s steps by hand, and the
    since-deleted `rust:test:doc` (the doctests now run as `rust_doc_test`
    targets inside `bazel:test:unit`) was added to the phase and not to the
    file: it appeared in none of the workflows, in no spelling, so CI ran zero
    doctests while every local `task verify` ran them. That is DX-5/C-008's
    pattern — a gate outside the full gate is a gate nobody runs — and a
    hand-kept second list is how it recurs, so the two are held together here
    instead.
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
        f"`.verify:build-test` runs `{task}` and verify-basic.yml does not — CI would skip it "
        f"entirely. Add the step, or add a `_BUILD_TEST_ELSEWHERE` row naming the workflow that "
        f"runs it (the row is asserted live)"
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
# Bazel remote-cache lane policy (plan_pr637_round2.md C-003)
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
#: stopped matching, which a clean tree cannot be told from.
BAZEL_LANE_FLOOR = 11
#: Detected lanes that get no cache credential (least privilege), with the reason each
#: reaches no Bazel. A row whose job is no longer a detected lane is a finding.
LANE_EXCLUSIONS = {
    "verify-basic.yml:workflow-lint": "actionlint and the .claude pytest suite; no Bazel call",
    "verify-basic.yml:smoke-acceptance": "pytest on the ocx-binary artifact; CI sets SKIP_BUILD, "
    "so test:.build-binaries is skipped",
    "verify-deep.yml:unit-native": "macOS/Windows runner executing the prebuilt cross bundle",
    "verify-deep.yml:acceptance": "test:accept:native runs pytest on the prebuilt test-bin artifact",
    "verify-deep.yml:acceptance-native": "macOS/Windows runner, SKIP_BUILD, prebuilt test-bin",
    "verify-deep.yml:satellite-verify": "cargo builds of ocx-mirror; no Bazel",
    "verify-deep.yml:satellite-contract": "cargo builds plus pytest; no Bazel",
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
#: Where the write secret may be named: as a cache step's credential input, or passed
#: through by name to a reusable workflow.
_WRITE_SECRET_SPOTS = frozenset({("with", "write-auth"), ("secrets", WRITE_SECRET)})


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


def _cache_writer_auth(step: dict) -> str | None:
    """The credential a step hands the `bazel-cache-rc` action, `None` when it writes no rc."""
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


def _cache_workflow_callers(documents: list[tuple[str, dict]]) -> list[tuple[str, str]]:
    """`(<caller>:<job>, <callee>)` for every job calling a workflow that holds a cache step.

    "Holds" is transitive: a workflow calling one that does is a path the secret must
    travel through too.
    """
    by_name = dict(documents)

    def callee_of(job: dict) -> str | None:
        uses = str(job.get("uses", ""))
        return uses.rsplit("/", 1)[1] if uses.startswith("./.github/workflows/") else None

    def holds(name: str, seen: tuple[str, ...] = ()) -> bool:
        if name in seen or name not in by_name:
            return False
        for job in (by_name[name].get("jobs") or {}).values():
            if any(_cache_writer_auth(step) is not None for step in job.get("steps") or []):
                return True
            callee = callee_of(job)
            if callee and holds(callee, (*seen, name)):
                return True
        return False

    return [
        (where, callee)
        for where, _, job in _jobs(documents)
        if (callee := callee_of(job)) and holds(callee)
    ]


# ---------------------------------------------------------------------------
# Sweeps — one function per property, each returning the findings it made
# ---------------------------------------------------------------------------


def _write_credential_findings(documents: list[tuple[str, dict]]) -> list[str]:
    """C-003(a): every cache step passes the write secret exactly, and nothing reads a read one.

    "Exactly" means no event predicate and no fallback: a gate would withhold the cache
    from lanes that should share it, and an `|| ''` is the shape a gate takes. The write
    secret is named only as a credential input or a by-name pass-through.
    """
    findings = []
    for where, _, job in _jobs(documents):
        for index, step in enumerate(job.get("steps") or []):
            auth = _cache_writer_auth(step)
            if auth is not None and " ".join(auth.split()) != WRITE_AUTH:
                findings.append(f"{where}[{index}] passes {auth.strip()!r}, not `{WRITE_AUTH}`")
    for name, document in documents:
        for path, value in _scalars(document):
            if "read-auth" in path or "BAZEL_CACHE_READ_AUTH" in value:
                findings.append(f"{name}:{'.'.join(path)} names the retired read credential")
            if _WRITE_SECRET_REF.search(value) and path[-2:] not in _WRITE_SECRET_SPOTS:
                findings.append(f"{name}:{'.'.join(path)} binds the write secret outside `write-auth`")
    return findings


def _upload_flag_findings(documents: list[tuple[str, dict]]) -> list[str]:
    """C-003(b): no step passes `--remote_upload_local_results`, with any value.

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
    """C-003(c): every Bazel lane writes the cache before its first lane step and cleans up after."""
    findings = []
    for where, (job, lane) in _lane_jobs(documents).items():
        if where in excluded:
            continue
        steps = job.get("steps") or []
        writers = [i for i, step in enumerate(steps) if _cache_writer_auth(step) is not None]
        if not writers or writers[0] > lane[0]:
            findings.append(f"{where}: no cache write step before its first lane step ({lane[0]})")
        elif gap := _cleanup_gap(steps):
            findings.append(f"{where}: {gap}")
    return findings


def _callers_without_the_secret(documents: list[tuple[str, dict]]) -> list[str]:
    """C-003(d): a caller of a workflow holding a cache step passes the write secret by name.

    `secrets: inherit` is a zizmor finding, and the called workflow must declare the
    secret or GitHub withholds it; both ends are checked.
    """
    by_name = dict(documents)
    findings = []
    for where, callee in _cache_workflow_callers(documents):
        passed = by_name[where.split(":")[0]]["jobs"][where.split(":")[1]].get("secrets")
        if not isinstance(passed, dict) or " ".join(str(passed.get(WRITE_SECRET, "")).split()) != WRITE_AUTH:
            findings.append(f"{where} calls {callee} without `secrets: {WRITE_SECRET}: {WRITE_AUTH}`")
        triggers = by_name[callee].get("on", by_name[callee].get(True))
        declared = ((triggers or {}).get("workflow_call") or {}).get("secrets") or {}
        if WRITE_SECRET not in declared:
            findings.append(f"{callee} holds a cache step but declares no `workflow_call` secret {WRITE_SECRET}")
    return sorted(set(findings))


def _rc_writers_without_cleanup(documents: list[tuple[str, dict]]) -> list[str]:
    """C-015's caller half: a job that writes the rc through the action also deletes it."""
    return [
        f"{where}: {gap}"
        for where, _, job in _jobs(documents)
        if any(_rc_writes(step) for step in job.get("steps") or [])
        and (gap := _cleanup_gap(job["steps"]))
    ]


def _lanes_with_an_outage_policy_flag(
    documents: list[tuple[str, dict]], rc_texts: Iterable[str] = ()
) -> list[str]:
    """S-008 / BZL-CACHE-26: no lane may make a cache outage fatal, or pretend to."""
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
    """S-016's structural half: a lane must actually point at the shared cache.

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
    *steps: str, caller: str | None = None, declared: bool = True
) -> list[tuple[str, dict]]:
    steps = steps or (_write(), _run(), _UPLOAD, _CLEANUP)
    declaration = f"  workflow_call:\n    secrets:\n      {WRITE_SECRET}:\n        required: false\n"
    lane = (
        f"name: lane\non:\n{declaration if declared else '  workflow_call: {}\n'}"
        "jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n"
        f"{''.join(steps)}"
    )
    return [("lane.yml", yaml.safe_load(lane)), ("caller.yml", yaml.safe_load(caller or _caller()))]


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
}

_EVENT_GATE = "${{ github.ref == 'refs/heads/main' && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}"
_VIOLATIONS = [
    pytest.param("write-credential", _corpus(_write(auth=_EVENT_GATE), _run(), _CLEANUP), id="a-event-predicate"),
    pytest.param("write-credential", _corpus(_write(auth="''"), _run(), _CLEANUP), id="a-empty-credential"),
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


def test_a_named_exclusion_skips_its_lane() -> None:
    corpus = _corpus(_run(), _CLEANUP)
    assert _lanes_without_cache_step(corpus)
    assert not _lanes_without_cache_step(corpus, {"lane.yml:build": "no Bazel"})


# ---------------------------------------------------------------------------
# The live assertions
# ---------------------------------------------------------------------------


def test_every_cache_step_passes_the_write_secret_and_nothing_else() -> None:
    """C-003(a). Floored on its reader: a tree with no cache step is not a clean one."""
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
    """C-003(b). The rc the action writes carries `--remote_upload_local_results=true`."""
    findings = _upload_flag_findings(_documents())
    assert not findings, (
        f"steps passing the upload flag: {findings}. Each task passes the action's rc with "
        f"`--bazelrc` when it exists; a second spelling in a workflow overrides it on one lane"
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
    """C-003(c). Without the step a lane compiles cold and the cache never gains an entry."""
    findings = _lanes_without_cache_step(_documents(), LANE_EXCLUSIONS)
    assert not findings, (
        f"Bazel lanes without a cache write step before their first lane step, or without "
        f"an `if: always()` cleanup after it: {findings}. A lane that provably reaches no "
        f"Bazel belongs in `LANE_EXCLUSIONS` with its reason; the credential is not "
        f"granted to a job that cannot use it"
    )


def test_every_caller_of_a_cache_workflow_passes_the_write_secret_by_name() -> None:
    """C-003(d). Secrets do not cross `workflow_call` unless declared and passed."""
    documents = _documents()
    assert _cache_workflow_callers(documents), (
        "no job calls a reusable workflow holding a cache step — the reader found nothing"
    )
    findings = _callers_without_the_secret(documents)
    assert not findings, (
        f"{findings}. Without both ends the called workflow's cache step sees an empty "
        f"secret and runs uncached, green at full cost"
    )


def test_every_write_credential_caller_deletes_it_in_an_always_step() -> None:
    """C-015's caller half: `using: composite` has no `post:` hook, so the deletion is a
    second `uses:` of the action under `if: always()`, after the last step needing the rc."""
    findings = _rc_writers_without_cleanup(_documents())
    assert not findings, (
        f"jobs that write the cache rc and do not delete it: {findings}. "
        f"`$RUNNER_TEMP/bazel-cache.rc` and its helper survive the job unless the caller runs "
        f"`{RC_ACTION}` again with `cleanup: 'true'` under `if: always()`; `success()` is not "
        f"enough, because the job a leftover credential matters on is the cancelled one"
    )


def test_no_bazel_lane_makes_a_cache_outage_fatal() -> None:
    """S-008 — an unreachable cache host is a warning and exit 0, by construction.

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
    """S-016's structural half — a lane pointing nowhere is the silent zero-hit lane.

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


def test_verify_deep_grants_no_write_at_the_top_and_keeps_test_bin_executable() -> None:
    """write scopes live on the publishing job only; every test-bin download is `chmod -R`'d."""
    document = _load(VERIFY_DEEP)
    top = document.get("permissions") or {}
    assert "write" not in top.values(), f"verify-deep.yml grants write at the top: {top!r}"
    assert document["jobs"]["test-results"]["permissions"].get("checks") == "write"
    for name, job in document["jobs"].items():
        steps = job.get("steps") or []
        if any(
            str(s.get("uses", "")).startswith("actions/download-artifact@")
            and "test-bin-" in str(s.get("with", {}).get("name", ""))
            for s in steps
        ):
            runs = "\n".join(str(s.get("run", "")) for s in steps)
            assert "chmod -R +x test/bin" in runs or job["runs-on"] == "windows-latest", (
                f"verify-deep.yml job {name!r} downloads test-bin but never `chmod -R +x test/bin`"
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

#: `--bazelrc` is a startup option, so it must sit immediately after the second
#: `bazel`. A call without it reads the shared cache anonymously (a 401) and
#: compiles cold, green — the shape that left `cross:build` uncached.
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
    """`adr_test_speed_tiers.md` D6: acceptance verdicts stay on the local disk.

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
