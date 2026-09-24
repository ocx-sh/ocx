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

- the stage-2 Bazel remote-cache credential boundary (plan_bazel_build_adoption.md
  C-015/C-016/C-017/C-029, S-005 … S-008, S-016). Whether the write credential is
  gated on a trusted event is the BZL-CACHE-01 control, and it is a property of the
  workflow file: a same-repo pull request gets the full `secrets` context, so nothing
  but that predicate keeps a cache-write token off a lane an untrusted contributor can
  trigger. See the section comment above `CACHE_HOST` for why every one of those
  assertions is structural and none of them dials the cache.

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

import base64
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
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
        "cross-compile",
        "acceptance-tests",
        "test-results",
        "satellite-verify",
    }
)
# A cancelled merge-queue run counts as a failed check and drops the entry
# from the queue, so queue runs are never cancelled; `main` keeps its carve-out.
MERGE_QUEUE_SAFE_CANCEL = (
    "${{ !startsWith(github.ref, 'refs/heads/gh-readonly-queue/') && github.ref != 'refs/heads/main' }}"
)
# Exact, not "contains the schedule clause": any wider `if:` can keep that
# clause and still admit no dispatch, which leaves the 3-OS matrix unreachable.
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
# D5: basic runs on Linux only; deep's `build` matrix owns the three OSes
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


def test_deep_build_matrix_covers_the_three_oses() -> None:
    """The matrix, and everything between a dispatch and it: the `if:` that
    admits one, the `runs-on` that reads the matrix, and no `exclude` carving
    an OS back out."""
    build = _load(VERIFY_DEEP)["jobs"]["build"]
    assert build.get("if") == DEEP_BUILD_IF, (
        f"verify-deep.yml `build.if` is `{build.get('if')}`, expected `{DEEP_BUILD_IF}` — any "
        f"other shape can keep the schedule clause and still admit no dispatch, which leaves "
        f"a branch with zero Windows/macOS coverage (basic has none, D5, and no pull request "
        f"fires this workflow)"
    )
    assert build.get("runs-on") == "${{ matrix.job.os }}", (
        f"verify-deep.yml `build.runs-on` is `{build.get('runs-on')}`, not "
        f"`${{{{ matrix.job.os }}}}` — the matrix below decides nothing unless the job runs on "
        f"its `os`"
    )
    matrix = build["strategy"]["matrix"]
    assert "exclude" not in matrix, (
        f"verify-deep.yml `build` matrix has `exclude: {matrix['exclude']}` — an excluded OS "
        f"is one the `job` list below still names and nothing runs"
    )
    runners = [str(entry.get("os", "")) for entry in matrix["job"]]
    missing = [
        family for family in OS_FAMILIES if not any(os.startswith(f"{family}-") for os in runners)
    ]
    assert not missing, (
        f"verify-deep.yml `build` matrix runs on {runners}, no {missing} entry — basic has no "
        f"such leg (D5), so this is the only gate that compiles and runs the `cfg` arms for "
        f"that OS on a pull request"
    )


# ---------------------------------------------------------------------------
# WP-42 R16: a parser's gate and its self-test travel together
# ---------------------------------------------------------------------------

#: The parser gate live in CI today, and the invocation that proves it, as two
#: needles into **one task's recipe**. Both are substrings of the same
#: `scripts/bazel_test_floor.py` command, so they are matched with the argument
#: that distinguishes them attached — `--bep` versus `--self-test` — never on
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
_FLOOR_PARSER_PROOF = "scripts/bazel_test_floor.py --self-test"


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


def test_the_duration_budget_is_proved_where_it_runs() -> None:
    """`rust:test:duration` is a shell parser over a run it does not own, so a job
    that runs it without `rust:test:duration:self-test` is green whether or not the
    parser still matches anything.

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
            if "task rust:test:duration:self-test" in runs:
                proved.append(f"{name}:{job_name}")
            if "task rust:test:duration" not in runs:
                continue
            assert "task rust:test:duration:self-test" in runs, (
                f"{name}:{job_name} runs `task rust:test:duration` and never "
                f"`task rust:test:duration:self-test` — the budget parser is then "
                f"proved nowhere the budget runs"
            )
    assert proved, (
        "no workflow runs `task rust:test:duration:self-test` — the budget parser's "
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
    """
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
    # `task test`, the SERIAL pytest entry point (21:41 for 3819 tests on an
    # idle four-core runner), then `task test:parallel`, and now the Bazel
    # lane: 172 `sh_test` targets with their results cached, run concurrently
    # behind the runner's host locks. `verify-basic.yml` is
    # deliberately not the home for it — that workflow is the fast PR gate and
    # the acceptance suite is the deep one's long half.
    "bazel:test:accept": ("verify-deep.yml", "task bazel:test:accept"),
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

    `verify-basic.yml` re-lists `.verify:build-test`'s steps by hand, and
    `rust:test:doc` was added to the phase and not to the file: it appeared in
    none of the workflows, in no spelling, so CI ran zero doctests while every
    local `task verify` ran them. That is DX-5/C-008's pattern — a gate
    outside the full gate is a gate nobody runs — and a hand-kept second list
    is how it recurs, so the two are held together here instead.
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
    `test:build` from there runs `cargo run -p ocx_schema` against absent
    `external/` patch sources and reds the job on a tree that is fine — which
    is how both legs failed on `main`.

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
            f"test:{', test:'.join(needs_cargo)}, which reach `{SCHEMA_TASK}` and so run cargo "
            f"against the `external/` patch sources that checkout did not fetch"
        )


# ---------------------------------------------------------------------------
# WP-21: the stage-2 remote-cache credential boundary
#
# plan_bazel_build_adoption.md C-015, C-016, C-017, C-029; S-005 … S-008, S-016.
# adr_bazel_build_adoption.md § Cache staging rulings 4a/4b/5/5b and § A2.
# bazel-quality/caching.md BZL-CACHE-01 (the control), -03, -04, -26.
#
# **C-029 governs the shape of everything below.** The remote cache realm is
# owner-gated: `bazel-cache.ocx.sh` answers 401 to every method today, the reader
# realm exists only on an unpushed `server-hetzner1` branch, and the two GitHub
# secrets do not exist. So every assertion here is a property of the *configuration*,
# read from workflow YAML and tracked rc files, and passes with both
# `BAZEL_CACHE_READ_AUTH` and `BAZEL_CACHE_WRITE_AUTH` unset and no route to the
# cache host. Exactly one test dials the network, only when its secret is exported,
# and it names the owner action in its skip message.
#
# **What is live today and what is not.** At the time these were written no workflow
# step ran `bazel` — WP-23 (`verify-basic.yml`) and WP-24 (`verify-deep.yml`) land
# the lane swap. A live assertion over zero lanes is the unchecked green this plan
# exists to avoid, so each lane assertion skips with its *observed* cause, and its
# red state lives permanently in `_VIOLATIONS` below: every sweep runs against a
# compliant fixture (must find nothing) and against a fixture built to violate it
# (must find something), on every invocation, today. Two sweeps — `$RUNNER_TEMP`
# uploads and interpolated credentials — are already live over the whole workflow
# corpus and floored on their own readers.
#
# **The secret's two spellings.** ADR ruling 5's snippet writes `BAZEL_CACHE_WRITE`;
# ruling 5b and plan C-029 write `BAZEL_CACHE_WRITE_AUTH`. Both are live in the
# corpus, so the needles match the common prefix and a rename is caught rather than
# silently unmatched.
# ---------------------------------------------------------------------------

#: The one host this repository's lanes may point `--remote_cache` at.
CACHE_HOST = "bazel-cache.ocx.sh"
CACHE_ENDPOINT = f"https://{CACHE_HOST}/"
#: The read credential's variable. It is exported only where it is meant to be used,
#: so its presence is this suite's opt-in to touching the network at all.
READ_CREDENTIAL = "BAZEL_CACHE_READ_AUTH"
#: The cache host sits behind Cloudflare, which rejects the default `Python-urllib/*`
#: signature with **403 and body `error code: 1010`** before the request reaches nginx.
#: Measured 2026-09-21: `Python-urllib` -> 403 from the edge; `bazel/9.2.0`, `curl/8.5.0`
#: and a browser string all -> `401 WWW-Authenticate: Basic realm="bazel cache"` from the
#: origin. Probing with the default agent would therefore red forever on a bot filter
#: rather than on the auth realm the assertion is about — a gate measuring a neighbouring
#: property. This exact string was measured reaching the origin.
PROBE_USER_AGENT = "bazel/9.2.0 (ocx wp-21 structural probe)"

#: The trusted event, as two terms rather than one string: `(A && B) && secrets.X`
#: and `A && B && secrets.X` are the same gate, and a reader accepting only the first
#: reports a compliant lane as a finding.
TRUSTED_EVENT_TERMS = ("github.event_name == 'push'", "github.ref == 'refs/heads/main'")
#: The two triggers a credential binding can tell apart, because ruling 5's gate tells
#: apart exactly these. `merge_group` is a `pull-request` here rather than a third case:
#: it is not a push to `main`, so every expression in this file answers it the way it
#: answers a pull request. A **fork** PR is not a case either — GitHub empties every
#: `secrets.*` there, so its source count is zero by construction and the anonymous
#: `--disk_cache` fallback is the documented behaviour, not a finding.
TRIGGERS = ("push-to-main", "pull-request")
#: BZL-CACHE-01's verification column. Matched as a whole token, never as a substring:
#: `# --remote_upload_local_results=false is NOT set` contains it too.
UPLOAD_OFF = "--remote_upload_local_results=false"
#: The grant it is the negation of, in the one spelling this repository uses. The
#: committed `.bazelrc` sets `UPLOAD_OFF` unconditionally and is read on every
#: invocation, so the grant is a **command-line build option** — measured on the pinned
#: 9.2.0 by reading the build event stream's canonical command line, which carries
#: `=true` and no `=false` when the flag is passed and `=false` when it is not. No rc
#: file changes, and the lane that carries it appends it after the command word:
#: `--remote_upload_local_results` is a build option, unlike `--bazelrc`.
UPLOAD_ON = "--remote_upload_local_results=true"
#: ADR ruling 5's expression with the grant as its payload — the credential's gate and
#: the grant's gate are one shape, read by one function, so neither can be loosened
#: without the other's assertions noticing. Written out literally rather than composed
#: from `TRUSTED_EVENT_TERMS`, for `_GATED`'s reason: a fixture built from the constants
#: the reader is checked against moves with them and can never red.
_UPLOAD_GRANT = (
    "${{ (github.event_name == 'push' && github.ref == 'refs/heads/main')"
    f" && '{UPLOAD_ON}' || '' }}}}"
)

#: Flags whose *presence* is the finding (BZL-CACHE-26, measured on 8.7.0 and 9.2.0:
#: with no `--remote_executor` there is nothing to fall back from, so all four
#: combinations produce `WARNING: Remote Cache: Connection refused`, every action
#: local, exit 0). `--remote_require_cached` is the opposite defect and the same
#: finding — it is the one flag that does turn an outage into a failed build.
OUTAGE_POLICY_FLAGS = frozenset(
    {
        "remote_local_fallback",
        "incompatible_remote_local_fallback_for_remote_cache",
        "remote_require_cached",
        "experimental_remote_require_cached",
    }
)

_WRITE_SECRET = re.compile(r"secrets\.(BAZEL_CACHE_WRITE\w*)")
_CACHE_SECRET = re.compile(r"secrets\.(BAZEL_CACHE_\w*)")
# `(?<!\.)` keeps a *filename* from reading as an invocation: `\b` treats the dot
# in `crates/ocx_shim/BUILD.bazel` as a boundary, so a `git log` pathspec naming a
# BUILD file classified `build-windows-shims.yml:build` -- a job that runs no bazel
# at all -- as a cache lane owing a credential. `.bazelrc` and `--bazelrc` were
# never matches (no boundary after `bazel`), and `bazel-out` still is. Measured:
# the narrowing drops exactly that one job and leaves the other eight lanes.
_BAZEL = re.compile(r"(?<!\.)\bbazel(?:isk)?\b", re.IGNORECASE)
_RUNNER_TEMP = re.compile(r"RUNNER_TEMP|runner\.temp", re.IGNORECASE)
#: A condition that varies with the *trigger*. C-017 requires `UPLOAD_OFF` to hold on
#: every trigger, `push` to `main` included; a platform condition
#: (`matrix.job.os == 'ubuntu-latest'`) is scheduling, not a trigger, and passes.
_TRIGGER_CONDITION = re.compile(r"github\.(?:event_name|event\.|ref\b|ref_name\b)")
#: A `${{ … }}` span. A flag inside one is conditional by construction.
_GITHUB_EXPRESSION = re.compile(r"\$\{\{.*?\}\}", re.DOTALL)
#: Every spelling that **enables** upload, and none that disables it. A bare
#: `--remote_upload_local_results` is `true` on Bazel's boolean parser, so the value is
#: optional here; the trailing lookahead is what keeps `=false` and `=0` out, and
#: `--noremote_upload_local_results` never matches because the literal `--remote_` does
#: not appear in it. A needle that caught the negation would report the default-safe
#: flag as the grant it is the opposite of.
_UPLOAD_ON = re.compile(r"--remote_upload_local_results(?:=(?:true|yes|1))?(?![=\w-])")
#: A `#` comment to end of line. A flag that survives this is one bazel is passed.
_SHELL_COMMENT = re.compile(r"(?m)(?:^|\s)#.*$")
_FLAG = re.compile(r"--[A-Za-z0-9_]+(?:=\S+)?")
#: `--config X`, the space-separated spelling `_FLAG` cannot pair up.
_SPACED_CONFIG = re.compile(r"--config\s+(\S+)")
#: One rc stanza: `build:ci --remote_cache=…`. `common` and `build` both reach a
#: `bazel test`, so all three command words are read.
_RC_STANZA = re.compile(r"(?m)^[ \t]*(build|common|test)(?::([\w.-]+))?[ \t]+(.*)$")
#: A `--remote_header` carrying an `Authorization`, matched case-insensitively because
#: HTTP header names are: Bazel passes the name through verbatim, so `Authorization=`
#: and `authorization=` arrive as one header. Matched with the shell quote optional,
#: since an rc-bound value is quoted and a command-line one usually is not.
_AUTHORIZATION_HEADER = re.compile(r"--remote_header=[\"']?authorization=", re.IGNORECASE)


# ---------------------------------------------------------------------------
# Readers — each with its own red state, since a reader that stopped matching
# leaves every sweep below green over an empty list
# ---------------------------------------------------------------------------


def _scalars(node: object, path: tuple[str, ...] = ()) -> Iterator[tuple[tuple[str, ...], str]]:
    """Every string leaf of a parsed document, with the key path that reaches it.

    A credential can be bound at workflow, job or step `env:`, or passed as a
    composite action's `with:` input; a sweep keyed on one of those placements misses
    the other three.
    """
    if isinstance(node, dict):
        for key, value in node.items():
            yield from _scalars(value, (*path, str(key)))
    elif isinstance(node, list):
        for index, value in enumerate(node):
            yield from _scalars(value, (*path, str(index)))
    elif isinstance(node, str):
        yield path, node


def _unwrap_parens(term: str) -> str:
    """`term` with its enclosing parentheses removed — only when there are any.

    `(a) && (b)` starts with `(` and ends with `)` and is **not** parenthesised: the
    first `(` is closed by the third character, not the last. Stripping there yields
    `a) && (b`, which reads as one opaque term and silently loses the `&&`.
    """
    term = term.strip()
    while term.startswith("(") and term.endswith(")"):
        depth = 0
        for index, char in enumerate(term):
            depth += (char == "(") - (char == ")")
            if depth == 0 and index < len(term) - 1:
                return term
        term = term[1:-1].strip()
    return term


def _split_top_level(expression: str, operator: str) -> list[str]:
    """`expression` split on `operator` at paren depth 0 and outside string literals.

    GitHub writes a literal `'` as `''`, so toggling on every `'` returns to the same
    state and no escape handling is needed.
    """
    parts: list[str] = []
    depth = start = index = 0
    quoted = False
    while index < len(expression):
        char = expression[index]
        if char == "'":
            quoted = not quoted
        elif quoted:
            pass
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
        elif depth == 0 and expression.startswith(operator, index):
            parts.append(expression[start:index])
            index = start = index + len(operator)
            continue
        index += 1
    parts.append(expression[start:])
    return [part.strip() for part in parts]


def _strip_expression(value: str) -> str:
    """One `${{ … }}` wrapper removed and whitespace collapsed.

    PyYAML folds a multi-line plain scalar into one line with newlines as spaces, but
    a block scalar keeps them; collapsing makes the two spellings of one expression
    compare equal.
    """
    collapsed = re.sub(r"\s+", " ", str(value)).strip()
    if collapsed.startswith("${{") and collapsed.endswith("}}"):
        return collapsed[3:-2].strip()
    return collapsed


def _conjuncts(expression: str) -> frozenset[str] | None:
    """Every term ANDed into `expression`, or `None` when some term can be absent.

    `None` on any reachable `||`, at any depth: in `(A || B) && secrets.X` the
    credential is released when `B` alone holds, so the set of terms that must be true
    is not what the expression's shape suggests. Refusing is the point — a reader
    returning `{A, B, secrets.X}` there would call an open gate closed.
    """
    expression = _unwrap_parens(_strip_expression(expression))
    if len(_split_top_level(expression, "||")) > 1:
        return None
    terms = _split_top_level(expression, "&&")
    if len(terms) == 1:
        return frozenset({expression})
    collected: set[str] = set()
    for term in terms:
        nested = _conjuncts(term)
        if nested is None:
            return None
        collected |= nested
    return frozenset(collected)


def _trusted_event_conjuncts(expression: str) -> frozenset[str] | None:
    """The terms `expression` requires, when it releases its payload on a push to `main`.

    The shape ADR ruling 5 names, and nothing looser:
    `${{ <condition> && <payload> || '' }}`, where `<condition>` carries both
    trusted-event terms as conjuncts with no reachable `||`, and the other branch is
    the empty string. `None` for anything else.

    The empty-string branch is half the rule and the easy half to drop. BZL-CACHE-01
    is "absent from that lane's environment, **not merely unused**": any other
    fallback leaves the value bound on an untrusted lane, and `UPLOAD_OFF` is then
    the only thing between a pull request and the shared Action Cache — which the rule
    calls its verification column, not its control.

    The payload is left to the caller to identify, because the gate now guards two of
    them: the write **credential** and the `--remote_upload_local_results=true` that
    makes it do anything. One shape reader, so the two can never drift into meaning
    different things by the same name.
    """
    branches = _split_top_level(_strip_expression(expression), "||")
    if len(branches) != 2:
        return None
    condition, fallback = branches
    if _unwrap_parens(fallback) not in ("''", '""'):
        return None
    terms = _conjuncts(condition)
    if terms is None or not all(term in terms for term in TRUSTED_EVENT_TERMS):
        return None
    return terms


def _gated_on_the_trusted_event(value: str) -> bool:
    """Whether `value` yields a **credential** only on a push to `main`."""
    terms = _trusted_event_conjuncts(value)
    return terms is not None and any(term.startswith("secrets.") for term in terms)


def _negated_trusted_event_conjuncts(expression: str) -> frozenset[str] | None:
    """The terms `expression` requires, when it releases its payload on **every other** trigger.

    `_trusted_event_conjuncts`' mirror, and it has to exist rather than be inferred:
    DX-81 gives the `main` push the write credential *alone*, so the read binding is
    the same gate negated — `${{ !(<condition>) && <payload> || '' }}` — and a reader
    that knows only the positive shape calls that binding unreadable.

    Exactly one negated conjunct, and it must be ruling 5's condition whole. Two
    negations, or a negation of something else, are a different gate wearing this one's
    punctuation, and `None` is the honest answer for both.
    """
    branches = _split_top_level(_strip_expression(expression), "||")
    if len(branches) != 2 or _unwrap_parens(branches[1]) not in ("''", '""'):
        return None
    terms = _conjuncts(branches[0])
    if terms is None:
        return None
    negations = [term for term in terms if term.startswith("!")]
    if len(negations) != 1:
        return None
    inner = _conjuncts(_unwrap_parens(negations[0][1:]))
    if inner is None or not all(term in inner for term in TRUSTED_EVENT_TERMS):
        return None
    return terms


def _credential_triggers(value: str) -> frozenset[str] | None:
    """Which of `TRIGGERS` leave `value` holding a credential. `None` when unreadable.

    Three shapes are read and nothing else is guessed at:

    * a bare `${{ secrets.BAZEL_CACHE_* }}` — every trigger;
    * ruling 5's gate — the `main` push alone;
    * ruling 5's gate negated — every trigger but the `main` push.

    `None` for anything else, and that is a *finding* at the caller rather than an
    empty set. A credential binding this file cannot evaluate is one whose per-trigger
    count nothing has established, which is the state DX-81 exists to end; reading it
    as "no credential here" would turn an unknown gate into a silent green.

    An empty set for a scalar naming no cache secret, which is most of them.
    """
    if not _CACHE_SECRET.search(value):
        return frozenset()
    terms = _conjuncts(_unwrap_parens(_strip_expression(value)))
    if terms is not None and all(term.startswith("secrets.") for term in terms):
        return frozenset(TRIGGERS)
    for reader, triggers in (
        (_trusted_event_conjuncts, frozenset({"push-to-main"})),
        (_negated_trusted_event_conjuncts, frozenset({"pull-request"})),
    ):
        gate = reader(value)
        if gate is not None and any(term.startswith("secrets.") for term in gate):
            return triggers
    return None


def _workflow_triggers(document: dict) -> list[str]:
    """The `TRIGGERS` this workflow can actually fire on.

    PyYAML parses a bare `on:` as the boolean `True`, which is why the key is looked up
    both ways. A workflow declaring neither is evaluated on both anyway: an expression
    that cannot be reached is not a reason to check nothing, and every workflow in this
    corpus declares both.
    """
    triggers = document.get("on", document.get(True)) or {}
    keys = set(triggers) if isinstance(triggers, dict | list) else {triggers}
    found = ["push-to-main"] if "push" in keys else []
    if keys & {"pull_request", "pull_request_target", "merge_group"}:
        found.append("pull-request")
    return found or list(TRIGGERS)


def _flags(text: str) -> list[str]:
    """The `--flags` bazel is actually passed by `text`.

    Two spellings are removed first, and each is a way a green here matches its own
    negation: a `#` comment (`# --remote_upload_local_results=false is NOT set`
    contains the token) and a `${{ … }}` span (a flag GitHub may or may not expand is
    not a flag the lane carries on every trigger).
    """
    return _FLAG.findall(_SHELL_COMMENT.sub("", _GITHUB_EXPRESSION.sub(" ", text)))


def _flag_names(flags: Iterable[str]) -> set[str]:
    """Flag names without the leading dashes, the `=value`, or the `no` negation.

    `--noremote_local_fallback` and `--remote_local_fallback=true` are the same flag
    under BZL-CACHE-26, whose finding is that either is *cited at all*. The bare form
    is yielded alongside the stripped one rather than instead of it, so a real flag
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

    The tracked `.bazelrc` is where a default-safe `UPLOAD_OFF` belongs (ADR ruling 5
    speaks of every non-write lane carrying it *in its effective flags*), so a sweep
    reading only the workflow step would report a compliant tree as a finding. Config
    activation is a fixpoint because one stanza may name another config.

    `import` / `try-import` directives are deliberately not followed: what they pull in
    is `.bazelrc.user`, which is gitignored and therefore outside the tracked set this
    reads. A flag reachable only through one is reported absent — a loud red, not a
    silent pass.
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


def _bazel_lanes(documents: list[tuple[str, dict]]) -> list[tuple[str, dict, dict, dict]]:
    """Every `(where, workflow, job, step)` whose `run:` invokes bazel.

    Deliberately broad — a bare `bazel`, a `bazelisk`, or a `task bazel:…` dispatch all
    match. A step that merely mentions bazel in a comment does not: the comment is
    stripped first, so the needle reads the command rather than the prose beside it.
    """
    return [
        (f"{name}:{job_name}", document, job, step)
        for name, document in documents
        for job_name, job in (document.get("jobs") or {}).items()
        for step in (job.get("steps") or [])
        if _BAZEL.search(_SHELL_COMMENT.sub("", str(step.get("run", ""))))
    ]


def _holds_the_write_credential(document: dict, job: dict) -> bool:
    """Whether the write secret is named anywhere in this job's scope."""
    return any(
        _WRITE_SECRET.search(value)
        for source in (document.get("env") or {}, job)
        for _, value in _scalars(source)
    )


def _upload_grants(value: str) -> tuple[list[str], list[str]]:
    """`(gated, ungated)` citations of the upload grant in one scalar.

    **`_flags` is the wrong reader here, and the difference is the whole point.** It
    drops a `${{ … }}` span, because a flag GitHub may or may not expand is not a flag
    the lane carries on every trigger — which is exactly the shape the grant must take.
    A sweep built on `_flags` would therefore see no grant at all and be green over a
    lane uploading from a pull request. Comments are still stripped: prose naming the
    flag is prose.

    A citation is *gated* only when the `${{ … }}` span enclosing it is ADR ruling 5's
    expression and the flag itself is that expression's payload. Everything else —
    outside any span, inside a span with a looser condition, inside a span whose
    payload is something else — is ungated, and the whole span is consumed so nothing
    is counted twice.
    """
    text = _SHELL_COMMENT.sub("", value)
    gated: list[str] = []
    for span in _GITHUB_EXPRESSION.findall(text):
        citations = _UPLOAD_ON.findall(span)
        if not citations:
            continue
        terms = _trusted_event_conjuncts(span)
        if terms is not None and any(
            _UPLOAD_ON.fullmatch(term.strip("'\"")) for term in terms
        ):
            gated.extend(citations)
            text = text.replace(span, " ", 1)
    return gated, _UPLOAD_ON.findall(text)


def _step_upload_grants(step: dict) -> tuple[list[str], list[str]]:
    """`_upload_grants` over every scalar of a step — `run:`, `env:` and `with:` alike.

    The grant can be written into the command, bound to a variable the command expands,
    or handed to a composite action; a reader keyed on `run:` alone misses two of three.
    """
    found = [_upload_grants(value) for _, value in _scalars(step)]
    return [c for gated, _ in found for c in gated], [c for _, up in found for c in up]


def _effective_flags(step: dict, rc_texts: Iterable[str]) -> list[str]:
    """The step's own flags plus the rc flags its `--config`s activate."""
    run = str(step.get("run", ""))
    command_line = _flags(run)
    configs = {flag.split("=", 1)[1] for flag in command_line if flag.startswith("--config=")}
    configs |= set(_SPACED_CONFIG.findall(_SHELL_COMMENT.sub("", run)))
    return [*command_line, *_rc_flags(rc_texts, configs)]


# ---------------------------------------------------------------------------
# Sweeps — one function per property, each returning the findings it made. Every
# one runs against a compliant fixture and a violating fixture below, so both
# halves of its red/green are shown on every invocation.
# ---------------------------------------------------------------------------


def _ungated_write_credentials(documents: list[tuple[str, dict]]) -> list[str]:
    """S-005, the BZL-CACHE-01 **control**: the write credential on a trusted event only.

    A2 says this is the control in as many words. The write-isolation test (run the PR
    lane *with* the credential and confirm zero `PUT`s) is defence in depth and
    explicitly not this — it constructs the very state BZL-CACHE-01 forbids, so its
    green cannot discharge the rule. Nothing here may be relaxed on the grounds that
    `UPLOAD_OFF` would have caught it.

    S-005's second half is narrower than "no rc file on a PR lane", and the narrowing
    is deliberate: since the read credential landed, a same-repo PR *does* write an rc
    file — one holding a `build --remote_header=…` line and nothing else. What no lane
    but the `main` push may hold is the **write** credential and the
    `--credential_helper` that carries it, and that is a consequence of this sweep
    rather than a separate check: the composite action emits the helper line only when
    `write-auth` is non-empty (C-015), and a gated binding is the empty string
    everywhere else. The rc file's own deletion is `_rc_writers_without_cleanup`, which
    counts either credential.
    """
    return [
        f"{name}:{'.'.join(path)} = {value.strip()}"
        for name, document in documents
        for path, value in _scalars(document)
        if _WRITE_SECRET.search(value) and not _gated_on_the_trusted_event(value)
    ]


def _lanes_not_disabling_upload(
    documents: list[tuple[str, dict]], rc_texts: Iterable[str] = ()
) -> list[str]:
    """C-017 / BZL-CACHE-01's verification: `UPLOAD_OFF` on every non-write bazel lane.

    Two exemptions, both principled. A lane holding the write credential is the lane
    allowed to upload, and on it the flag's other half comes from the CI-only rc file
    (ADR ruling 4a) that lives outside the workspace and this sweep cannot read —
    `_ungated_write_credentials` is what governs that lane. A lane with no
    `--remote_cache` reaches no remote cache to upload to.

    Everything else carries it **unconditionally**: not inside a `${{ … }}` (`_flags`
    drops those) and not under a job or step `if:` that varies with the trigger, since
    C-017 requires it on `push` to `main` as much as on a pull request.
    """
    rc_texts = list(rc_texts)
    findings = []
    for where, document, job, step in _bazel_lanes(documents):
        if _holds_the_write_credential(document, job):
            continue
        flags = _effective_flags(step, rc_texts)
        if not any(flag.startswith("--remote_cache=") for flag in flags):
            continue
        if UPLOAD_OFF not in flags:
            findings.append(f"{where}: effective flags carry no `{UPLOAD_OFF}`")
            continue
        # A job or step `if:` can only make the flag conditional when the flag
        # comes from the step's own `run:`. An unconditional tracked rc stanza
        # holds on every trigger by construction, and a job `if:` is then a
        # *scheduling* condition over a lane that already carries the flag —
        # the same distinction the platform/trigger split above draws, one
        # level up. Reading it the other way reported `verify-deep.yml:build`,
        # whose draft-PR guard decides whether the lane runs at all and nothing
        # about what flags it runs with.
        from_the_run = UPLOAD_OFF in _flags(str(step.get("run", "")))
        conditions = [
            str(node["if"])
            for node in (job, step)
            if from_the_run and _TRIGGER_CONDITION.search(str(node.get("if", "")))
        ]
        if conditions:
            findings.append(f"{where}: `{UPLOAD_OFF}` is under a trigger condition {conditions}")
    return findings


def _write_lane_jobs(documents: list[tuple[str, dict]]) -> list[str]:
    """Every `<workflow>:<job>` that names the write secret anywhere in its scope."""
    return sorted(
        f"{name}:{job_name}"
        for name, document in documents
        for job_name, job in (document.get("jobs") or {}).items()
        if _holds_the_write_credential(document, job)
    )


def _extra_write_lanes(documents: list[tuple[str, dict]]) -> list[str]:
    """The "only" half of the ruling: **one** job in the corpus may hold the credential.

    A trusted-event gate makes a second holder invisible to
    `_ungated_write_credentials`, and `_lanes_not_disabling_upload` *exempts* whatever
    holds the credential — so a job quietly acquiring `write-auth` would widen the write
    surface and narrow the sweep that governs it in the same edit. Naming the surplus
    rather than the set, because the finding is the second one, not the first.
    """
    return _write_lane_jobs(documents)[1:]


def _lanes_enabling_upload_off_the_write_lane(documents: list[tuple[str, dict]]) -> list[str]:
    """The grant's containment: `UPLOAD_ON` on the `main` push and nowhere else.

    `_lanes_not_disabling_upload` cannot carry this and is not asked to. It reads
    `_effective_flags`, which drops `${{ … }}` spans, so a gated grant is invisible to
    it by construction; and it exempts the write lane wholesale, which is the one lane
    this governs. Two findings, because they are two defects:

    * a lane citing the grant while holding no write credential — an anonymous or
      read-credentialled `PUT`, and on a pull request the state BZL-CACHE-01 forbids;
    * a grant that is not gated on the trusted event. The credential is released only
      on the `main` push, but an *unconditional* grant still makes every other trigger
      attempt uploads it cannot authenticate — failed `PUT`s reported as warnings,
      which is a lane that looks like it is populating the cache and is not.
    """
    findings = []
    for where, document, job, step in _bazel_lanes(documents):
        gated, ungated = _step_upload_grants(step)
        if not (gated or ungated):
            continue
        if not _holds_the_write_credential(document, job):
            findings.append(
                f"{where}: cites {sorted(set(gated + ungated))} and its job holds no write "
                f"credential"
            )
            continue
        if ungated:
            findings.append(f"{where}: cites {sorted(set(ungated))} outside a trusted-event gate")
    return findings


def _write_lanes_uploading_nothing(documents: list[tuple[str, dict]]) -> list[str]:
    """The silent no-op the grant exists to prevent: the credential without the flag.

    The committed `.bazelrc` sets `UPLOAD_OFF` unconditionally and is read on every
    invocation, so a job holding the write credential and citing no override **uploads
    nothing** — a green lane at full cost, with a live write credential written to the
    runner and deleted again having done no work. Nothing else in this file reports it:
    every other sweep here is satisfied by a lane that cannot upload.

    Keyed on the job, because `$RUNNER_TEMP` is job-scoped and any of its bazel steps
    may be the one that uploads.
    """
    uploading = {
        where
        for where, _, _, step in _bazel_lanes(documents)
        if any(_step_upload_grants(step))
    }
    return [
        f"{where} holds the write credential and no bazel step of it cites `{UPLOAD_ON}`"
        for where in _write_lane_jobs(documents)
        if where not in uploading
    ]


def _credential_sources(
    document: dict, job: dict, step: dict, trigger: str
) -> tuple[list[str], list[str]]:
    """`(sources, unreadable)` reaching this bazel step on `trigger`.

    Three routes, because there are three ways a credential arrives at one invocation
    and a counter keyed on one of them is blind to the other two:

    * an **rc-writer input** — `read-auth` becomes a `build --remote_header=…` line and
      `write-auth` a `build --credential_helper=…` line, and Bazel applies both from
      the `--bazelrc=` the lane passes. Keyed on the job, which is the scope
      `$RUNNER_TEMP` and the runner live in; the `cleanup: true` call is not a writer.
    * a **`--remote_header=…authorization=`** anywhere in the step, `run:`, `env:` or
      `with:`. Counted on every trigger even inside a `${{ … }}`: a header GitHub may
      or may not expand is still a second source on the trigger where it does, and
      over-counting a conditional one reds rather than passes.
    * an **`env:` binding of a cache secret** visible to the step — step, job, then
      workflow. Nothing in Bazel reads such a variable by itself, and that is not the
      point: a lane that exports one is a lane wiring it into the invocation somehow,
      and DX-81's count is of credentials presented, not of flags proven effective.
    """
    sources: list[str] = []
    unreadable: list[str] = []

    def classify(where: str, value: str) -> None:
        triggers = _credential_triggers(value)
        if triggers is None:
            unreadable.append(where)
        elif trigger in triggers:
            sources.append(where)

    action = _RC_WRITER.parent.as_posix()
    for writer in job.get("steps") or []:
        inputs = writer.get("with") or {}
        if action not in str(writer.get("uses", "")) or _truthy_input(writer, "cleanup"):
            continue
        for name in ("read-auth", "write-auth"):
            if str(inputs.get(name, "")).strip():
                classify(f"`{name}` -> {action}", str(inputs[name]))

    for path, value in _scalars(step):
        sources += [
            f"`{header}` in step {'.'.join(path)}"
            for header in _AUTHORIZATION_HEADER.findall(_SHELL_COMMENT.sub("", value))
        ]

    for scope, node in (("step", step), ("job", job), ("workflow", document)):
        for path, value in _scalars(node.get("env") or {}):
            classify(f"{scope} env `{'.'.join(path)}`", value)

    return sources, unreadable


def _lanes_not_carrying_exactly_one_credential_source(
    documents: list[tuple[str, dict]], rc_texts: Iterable[str] = ()
) -> list[str]:
    """DX-81, the ruling: **one credential source per lane, per trigger, by construction.**

    Measured against a local sink with both credential routes live, one cache `PUT`
    carried *three* `Authorization` headers — the helper's, a `~/.bazelrc`
    `--remote_header`, and a command-line one. Bazel `add`s a header per configured
    source rather than replacing it, so which credential authenticates a request is
    decided by the origin's parsing of a list it was never meant to receive. A design
    that depends on which duplicate an origin keeps is the defect, whichever one it
    keeps, so the count is what this asserts and the origin's behaviour is not recorded
    anywhere in this repository.

    **Two** is the finding DX-81 names and **zero** is the finding it replaces: before
    the read credential landed, every CI request to the shared cache was an anonymous
    401 and the unit lane compiled cold on every run, while `--remote_cache` sat in the
    tracked `.bazelrc` and every flag assertion in this file was green. A flag naming a
    host is not reach. `!= 1` is therefore one predicate over both defects rather than
    two sweeps that could disagree.

    Scoped to the lanes that actually dial the shared cache, from their effective flags
    — a step reaching no cache has no request to authenticate, and
    `_lanes_without_remote_cache_reach` is what forbids *that*.

    This is the structural half of the ruling and it is exact about what it covers: the
    sources a workflow file spells. The ones it cannot see — a runner's `~/.bazelrc`, a
    system rc, an `import`ed `.bazelrc.user` — are refused at runtime by `write_rc.py`,
    which walks that chain before writing anything. Neither half covers the other's.
    """
    rc_texts = list(rc_texts)
    findings = []
    for where, document, job, step in _bazel_lanes(documents):
        flags = _effective_flags(step, rc_texts)
        if not any(flag.startswith(f"--remote_cache=https://{CACHE_HOST}") for flag in flags):
            continue
        for trigger in _workflow_triggers(document):
            sources, unreadable = _credential_sources(document, job, step, trigger)
            if unreadable:
                findings.append(
                    f"{where} on a {trigger}: credential bindings this file cannot "
                    f"evaluate: {sorted(set(unreadable))}"
                )
            elif len(sources) != 1:
                findings.append(
                    f"{where} on a {trigger}: {len(sources)} credential sources "
                    f"{sorted(set(sources))}, expected exactly 1"
                )
    return findings


def _lanes_with_an_outage_policy_flag(
    documents: list[tuple[str, dict]], rc_texts: Iterable[str] = ()
) -> list[str]:
    """S-008 / BZL-CACHE-26: no lane may make a cache outage fatal, or pretend to.

    A cache-only deployment has no `--remote_executor`, so there is nothing to fall
    back *from*: measured on 8.7.0 and 9.2.0, all four fallback-flag combinations gave
    `WARNING: Remote Cache: Connection refused`, every action local, exit 0. Citing
    either fallback flag is therefore dead configuration presented as an outage policy,
    and its **presence** is the finding. `--remote_require_cached` is the one flag that
    genuinely converts an outage into a red build, which S-008 forbids from the other
    side; both are the same finding here.
    """
    rc_texts = list(rc_texts)
    return [
        f"{where}: `{flag}`"
        for where, _, _, step in _bazel_lanes(documents)
        for flag in _effective_flags(step, rc_texts)
        if _flag_names([flag]) & OUTAGE_POLICY_FLAGS
    ]


def _lanes_without_remote_cache_reach(
    documents: list[tuple[str, dict]], rc_texts: Iterable[str] = ()
) -> list[str]:
    """S-016's structural half: a bazel lane must actually point somewhere.

    "Zero hits → the lane is green with no cache reach at all, indefinitely and
    silently" is the failure S-016 exists to catch, and a lane carrying no
    `--remote_cache` is that failure in its loudest, cheapest-to-detect form. The host
    is pinned too, and the scheme must be an explicit `https://`, because both remote
    flags default to `grpcs` on a scheme-less URI and would dial a port nothing serves.

    The *hit count* is not establishable from configuration. What counts as a hit is,
    and `remote_cache_hits` is where that lives.
    """
    rc_texts = list(rc_texts)
    findings = []
    for where, _, _, step in _bazel_lanes(documents):
        endpoints = [
            flag.split("=", 1)[1].strip("\"'")
            for flag in _effective_flags(step, rc_texts)
            if flag.startswith("--remote_cache=")
        ]
        if not endpoints:
            findings.append(f"{where}: no `--remote_cache=` in its effective flags")
            continue
        wrong = [endpoint for endpoint in endpoints if urlsplit(endpoint).hostname != CACHE_HOST]
        if wrong:
            findings.append(
                f"{where}: `--remote_cache` points at {wrong}, not https://{CACHE_HOST}"
            )
    return findings


def _runner_temp_uploads(documents: list[tuple[str, dict]]) -> list[str]:
    """ADR ruling 4b line 4: no upload step may take a path under `$RUNNER_TEMP`.

    `$RUNNER_TEMP` is where the CI-only rc file and the credential helper live, and
    `verify-basic.yml`'s `smoke` job — the job that will hold the write credential —
    already runs `actions/upload-artifact`. An artifact is a download for anyone who
    can read the run.
    """
    return [
        f"{name}:{job_name} step `{step.get('uses')}` path={value.strip()!r}"
        for name, document in documents
        for job_name, job in (document.get("jobs") or {}).items()
        for step in (job.get("steps") or [])
        if "upload" in str(step.get("uses", ""))
        for _, value in _scalars(step.get("with") or {})
        if _RUNNER_TEMP.search(value)
    ]


def _interpolated_credentials(documents: list[tuple[str, dict]]) -> list[str]:
    """ADR ruling 4b line 1: a cache secret reaches a `run:` body through `env:` only.

    GitHub materialises every step script to a file under `/home/runner/work/_temp/`
    before running it, so the `${{ secrets.X }}` spelling writes the credential to a
    second on-disk location and through the expression-expansion path. `env:` passes it
    as a process environment variable and never renders it into the script text.
    """
    return [
        f"{name}:{'.'.join(path)} interpolates {sorted(set(_CACHE_SECRET.findall(value)))}"
        for name, document in documents
        for path, value in _scalars(document)
        if path and path[-1] == "run" and _CACHE_SECRET.search(value)
    ]


def _rc_writers_without_cleanup(documents: list[tuple[str, dict]]) -> list[str]:
    """C-015's second half: a job that writes the credential also deletes it.

    A composite action has no `post:` hook — the runner's action schema gives
    `post`/`post-if` to `node-runs` and `container-runs` only — so the deletion is
    an **obligation on the caller**, stated in prose in `action.yml`'s `cleanup`
    input and, until this sweep, enforced by nothing. DX-34 split the clause that
    way on purpose ("the step body is WP-22's, the `if: always()` caller is
    WP-23/WP-24's") and then WP-24 was aborted, which left the caller half with no
    owner and no check.

    Keyed on the *job*, because that is the scope `$RUNNER_TEMP` and the runner
    live in. Three things must hold together, and each is a separate finding so a
    half-wired caller names its own gap:

    * a second step of the same job `uses:` the action with `cleanup` truthy;
    * that step carries an `if:` mentioning `always()` — a cancelled or failed job
      is exactly the one an unconditional cleanup would skip;
    * the write step is not itself the cleanup step.

    The write step is recognised by a **non-blank** `read-auth` or `write-auth`,
    not by their mere presence: with both blank `write_rc.py` returns before
    touching the filesystem, so nothing needs cleaning up and treating that as a
    writer would make the compliant no-op lane red. Either one alone puts an rc
    file under `$RUNNER_TEMP` — the read credential is a `--remote_header` line
    and no helper — and a credential on a runner is a credential to delete
    whichever half it is, so both count.
    """
    action = _RC_WRITER.parent.as_posix()
    findings = []
    for name, document in documents:
        for job_name, job in (document.get("jobs") or {}).items():
            steps = [
                step for step in (job.get("steps") or []) if action in str(step.get("uses", ""))
            ]
            writers = [
                step
                for step in steps
                if any(
                    str((step.get("with") or {}).get(credential, "")).strip()
                    for credential in ("read-auth", "write-auth")
                )
                and not _truthy_input(step, "cleanup")
            ]
            if not writers:
                continue
            cleanups = [step for step in steps if _truthy_input(step, "cleanup")]
            if not cleanups:
                findings.append(
                    f"{name}:{job_name} passes a cache credential and calls "
                    f"`{action}` with no `cleanup: true` step"
                )
                continue
            if not any("always()" in str(step.get("if", "")) for step in cleanups):
                findings.append(
                    f"{name}:{job_name} has a `cleanup: true` step whose `if:` is "
                    f"{[str(step.get('if', '<absent>')) for step in cleanups]} — not `always()`, "
                    f"so a cancelled or failed job leaves the credential on the runner"
                )
    return findings


def _truthy_input(step: dict, name: str) -> bool:
    """A `with:` input YAML may have parsed as a bool, a string, or an expression."""
    value = (step.get("with") or {}).get(name)
    return str(value).strip().lower() == "true"


def _rc_files_configuring_a_credential_helper(rc_files: Iterable[Path]) -> list[str]:
    """BZL-CACHE-04 (MUST): `--credential_helper` never from a shipped rc file.

    Bazel resolves and spawns the helper with the full client environment, before the
    sandbox, with no check on where the flag came from — a read-only `bazel query
    //...` on a fresh clone is enough to execute it (reproduced on 9.2.0, closed by the
    vendor as intended, bazel#30439). The helper belongs in
    `$RUNNER_TEMP/bazel-cache.rc`, written by WP-22's composite action on a trusted
    event and consumed through `--bazelrc=` (ADR ruling 4a).
    """
    return [
        f"{path}:{number}: {line.strip()}"
        for path in rc_files
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1)
        if "credential_helper" in _SHELL_COMMENT.sub("", line)
    ]


# ---------------------------------------------------------------------------
# The reader red states
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("term", "expected"),
    [
        pytest.param("(a) && (b)", "(a) && (b)", id="not-parenthesised"),
        pytest.param("((a && b))", "a && b", id="doubly-parenthesised"),
        pytest.param(" (a) ", "a", id="parenthesised"),
        pytest.param("a && b", "a && b", id="bare"),
    ],
)
def test_the_paren_reader_strips_only_an_enclosing_pair(term: str, expected: str) -> None:
    assert _unwrap_parens(term) == expected


@pytest.mark.parametrize(
    ("expression", "expected"),
    [
        pytest.param("a && b", frozenset({"a", "b"}), id="flat"),
        pytest.param("(a && b) && c", frozenset({"a", "b", "c"}), id="nested"),
        pytest.param("a && (b && (c && d))", frozenset({"a", "b", "c", "d"}), id="deep"),
        pytest.param("a || b", None, id="top-level-or"),
        pytest.param("(a || b) && c", None, id="nested-or"),
        pytest.param("a && (b || c)", None, id="or-in-a-conjunct"),
        pytest.param("x == 'a || b'", frozenset({"x == 'a || b'"}), id="or-inside-a-literal"),
        pytest.param("${{ a && b }}", frozenset({"a", "b"}), id="expression-wrapped"),
    ],
)
def test_the_conjunct_reader_refuses_a_reachable_or(
    expression: str, expected: frozenset[str] | None
) -> None:
    """A `||` anywhere it can be reached means a term is optional, so the reader refuses
    rather than returning a set that reads as mandatory."""
    assert _conjuncts(expression) == expected


@pytest.mark.parametrize(
    ("value", "gated"),
    [
        pytest.param(
            "${{ (github.event_name == 'push' && github.ref == 'refs/heads/main')"
            " && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}",
            True,
            id="the-adr-spelling",
        ),
        pytest.param(
            "${{ github.event_name == 'push' && github.ref == 'refs/heads/main'"
            " && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}",
            True,
            id="unparenthesised-but-identical",
        ),
        pytest.param("${{ secrets.BAZEL_CACHE_WRITE_AUTH }}", False, id="ungated"),
        pytest.param(
            "${{ github.event_name == 'push' && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}",
            False,
            id="event-without-ref",
        ),
        pytest.param(
            "${{ github.ref == 'refs/heads/main' && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}",
            False,
            id="ref-without-event",
        ),
        pytest.param(
            "${{ (github.event_name == 'push' && github.ref == 'refs/heads/main'"
            " || github.event_name == 'pull_request') && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}",
            False,
            id="an-or-widens-the-gate",
        ),
        pytest.param(
            "${{ (github.event_name == 'push' && github.ref == 'refs/heads/main')"
            " && secrets.BAZEL_CACHE_WRITE_AUTH || secrets.BAZEL_CACHE_WRITE_AUTH }}",
            False,
            id="fallback-is-the-credential",
        ),
        pytest.param(
            "${{ (github.event_name == 'push' && github.ref == 'refs/heads/main')"
            " && secrets.BAZEL_CACHE_WRITE_AUTH || 'unset' }}",
            False,
            id="fallback-is-not-empty",
        ),
        pytest.param(
            "${{ (github.event_name == 'push' && github.ref == 'refs/heads/next')"
            " && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}",
            False,
            id="another-branch",
        ),
    ],
)
def test_the_trusted_event_gate_reader_discriminates(value: str, gated: bool) -> None:
    assert _gated_on_the_trusted_event(value) is gated


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        pytest.param(f"bazel test //... {UPLOAD_OFF}", ([], []), id="the-negation-is-not-a-grant"),
        pytest.param(
            "bazel test //... --noremote_upload_local_results",
            ([], []),
            id="the-no-prefixed-negation-is-not-a-grant",
        ),
        pytest.param(
            f"bazel test //...  # {UPLOAD_ON} would go here", ([], []), id="prose-is-not-a-grant"
        ),
        pytest.param(
            f"bazel test //... {UPLOAD_ON}", ([], [UPLOAD_ON]), id="unconditional-is-ungated"
        ),
        pytest.param(
            "bazel test //... --remote_upload_local_results",
            ([], ["--remote_upload_local_results"]),
            id="the-bare-flag-is-true",
        ),
        pytest.param(
            f"task bazel:test:unit -- {_UPLOAD_GRANT}", ([UPLOAD_ON], []), id="the-adr-gate"
        ),
        pytest.param(
            f"task bazel:test:unit -- ${{{{ github.event_name == 'push' && '{UPLOAD_ON}' || '' }}}}",
            ([], [UPLOAD_ON]),
            id="half-a-gate-is-no-gate",
        ),
        pytest.param(
            f"task bazel:test:unit -- ${{{{ (github.event_name == 'push' && github.ref =="
            f" 'refs/heads/main') && '{UPLOAD_ON}' || '{UPLOAD_ON}' }}}}",
            ([], [UPLOAD_ON, UPLOAD_ON]),
            id="both-branches-grant",
        ),
    ],
)

def test_the_upload_grant_reader_discriminates(
    value: str, expected: tuple[list[str], list[str]]
) -> None:
    """The reader's own red and green, on the three distinctions it exists to draw.

    `UPLOAD_OFF` shares every character of `UPLOAD_ON` but four, a `#` comment naming
    the grant is prose, and — the one `_flags` gets deliberately backwards — a grant
    inside a `${{ … }}` is the *compliant* shape here rather than an invisible one.
    """
    assert _upload_grants(value) == expected


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        pytest.param("${{ secrets.BAZEL_CACHE_READ_AUTH }}", set(TRIGGERS), id="unconditional"),
        pytest.param(
            "${{ (github.event_name == 'push' && github.ref == 'refs/heads/main')"
            " && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}",
            {"push-to-main"},
            id="ruling-5-gate",
        ),
        pytest.param(
            "${{ !(github.event_name == 'push' && github.ref == 'refs/heads/main')"
            " && secrets.BAZEL_CACHE_READ_AUTH || '' }}",
            {"pull-request"},
            id="ruling-5-gate-negated",
        ),
        pytest.param("plain text", set(), id="names-no-cache-secret"),
        pytest.param("${{ secrets.OTEL_OTLP_AUTH }}", set(), id="names-another-secret"),
        # The `''` branch is half the gate and the easy half to drop: any other
        # fallback leaves the credential live on the trigger the condition withholds
        # it from, which is the same defect as no gate.
        pytest.param(
            "${{ !(github.event_name == 'push' && github.ref == 'refs/heads/main')"
            " && secrets.BAZEL_CACHE_READ_AUTH || 'none' }}",
            None,
            id="negated-gate-falls-back-to-a-non-empty-string",
        ),
        # `!(A)` alone is not ruling 5 negated — it withholds the credential from every
        # push, `main` or not, which is a different lane split wearing the same shape.
        pytest.param(
            "${{ !(github.event_name == 'push') && secrets.BAZEL_CACHE_READ_AUTH || '' }}",
            None,
            id="negation-of-something-else",
        ),
        # Two negations: the terms are there and the gate they compose is not this one.
        pytest.param(
            "${{ !(github.event_name == 'push' && github.ref == 'refs/heads/main')"
            " && !github.event.pull_request.draft && secrets.BAZEL_CACHE_READ_AUTH || '' }}",
            None,
            id="two-negated-conjuncts",
        ),
    ],
)
def test_the_credential_trigger_reader_discriminates(
    value: str, expected: set[str] | None
) -> None:
    """DX-81's reader, with its refusals. A shape it guessed at would turn an
    unevaluated gate into a source count nothing established."""
    triggers = _credential_triggers(value)
    assert triggers == (None if expected is None else frozenset(expected))

@pytest.mark.parametrize(
    ("run", "expected"),
    [
        pytest.param(f"bazel test //... {UPLOAD_OFF}", [UPLOAD_OFF], id="passed"),
        pytest.param(f"bazel test //...  # {UPLOAD_OFF} is NOT set", [], id="in-a-comment"),
        pytest.param(
            f"bazel test //... ${{{{ github.ref == 'refs/heads/main' && '' || '{UPLOAD_OFF}' }}}}",
            [],
            id="in-an-expression",
        ),
        pytest.param(
            "bazel test //... --remote_upload_local_results=true",
            ["--remote_upload_local_results=true"],
            id="the-opposite-value",
        ),
        pytest.param(
            f"bazel test //... \\\n  --remote_cache=https://{CACHE_HOST} \\\n  {UPLOAD_OFF}",
            [f"--remote_cache=https://{CACHE_HOST}", UPLOAD_OFF],
            id="line-continued",
        ),
    ],
)
def test_the_flag_reader_reads_past_a_comment_and_an_expression(
    run: str, expected: list[str]
) -> None:
    """The substring trap, pinned. `UPLOAD_OFF in run` is true for four of these five
    and the property those four assert is true for exactly two."""
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
        pytest.param([], [UPLOAD_OFF], id="unconditional-only"),
        pytest.param(["ci"], [UPLOAD_OFF, f"--remote_cache=https://{CACHE_HOST}"], id="active"),
        pytest.param(
            ["dev"],
            [UPLOAD_OFF, "--config=ci", f"--remote_cache=https://{CACHE_HOST}"],
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
            f"build {UPLOAD_OFF}",
            "# build --remote_cache=https://commented.example",
            f"build:ci --remote_cache=https://{CACHE_HOST}",
            "build:dev --config=ci",
            "query:ci --keep_going",
        ]
    )
    assert sorted(_rc_flags([rc], configs)) == sorted(expected)


# ---------------------------------------------------------------------------
# Every sweep's red and green, shown on every run
#
# The fixtures are the tree WP-22/23/24 are expected to produce, and the same tree
# with one property broken. Each violating fixture is *constructed* from the same
# template with one substitution changed, rather than patched with `str.replace`
# afterwards: there is then no needle that can drift and leave the compliant tree
# standing in for a mutation — the harness-is-not-exempt case in `quality-core.md`
# § Unchecked Green.
# ---------------------------------------------------------------------------

#: The gate ADR ruling 5 specifies, written once and substituted into the fixtures.
_GATED = (
    "${{ (github.event_name == 'push' && github.ref == 'refs/heads/main')"
    " && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}"
)

#: DX-81's other half: the same gate negated, carrying the **read** credential, so the
#: `main` push presents the write credential alone and every other trigger presents
#: this one alone. The truthy branch carries the secret in both, because GitHub's
#: `a && b || c` yields `c` whenever `b` is falsy and `''` is falsy — a binding written
#: `<trusted> && '' || secrets.READ` hands the read secret to the `main` push, which is
#: the exact state the gate exists to forbid.
_NEGATED_GATE = (
    "${{ !(github.event_name == 'push' && github.ref == 'refs/heads/main')"
    " && secrets.BAZEL_CACHE_READ_AUTH || '' }}"
)

#: The write lane: `verify-basic.yml`'s `smoke` after the swap. It carries no literal
#: `UPLOAD_OFF` because on this lane the committed `.bazelrc`'s copy is overridden —
#: which is why `_lanes_not_disabling_upload` exempts a write lane and
#: `_ungated_write_credentials` plus the three grant sweeps govern it instead.
#:
#: **Both credentials, on complementary gates** (DX-81). This job serves every trigger
#: — there is no separate main-push job — so it must carry a binding for each, and the
#: two must never be live at once. It is the one fixture where the per-trigger count
#: has something to discriminate, which is why the violations built from it are the
#: ruling's own failure shapes.
_WRITE_LANE = """
name: verify-basic
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
jobs:
  smoke:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: ./.github/actions/bazel-cache-rc
        with:
          read-auth: %(read_credential)s
          write-auth: %(credential)s
      - name: Unit tests
        run: |
          bazel --bazelrc="$RUNNER_TEMP/bazel-cache.rc" test //... --remote_cache=https://%(host)s %(grant)s
      - uses: actions/upload-artifact@v4
        with:
          path: %(upload)s
%(cleanup)s%(second_job)s"""

#: A second job holding the write credential — substituted in rather than appended, so
#: the violating fixture is *built* with two write lanes instead of patched into one.
_SECOND_WRITE_JOB = """
  smoke-again:
    runs-on: ubuntu-latest
    steps:
      - uses: ./.github/actions/bazel-cache-rc
        if: always()
        with:
          cleanup: 'true'
      - uses: ./.github/actions/bazel-cache-rc
        with:
          write-auth: %(credential)s
      - name: Unit tests again
        run: |
          bazel test //... --remote_cache=https://%(host)s %(grant)s
"""

#: C-015's caller half, which a composite action cannot carry itself: no `post:`
#: hook exists for `using: composite`, so the deletion is a second `uses:` under
#: `if: always()` in the same job. Substituted rather than appended, so the
#: violating fixture is *built* without it instead of patched afterwards.
_CLEANUP_STEP = """      - uses: ./.github/actions/bazel-cache-rc
        if: always()
        with:
          cleanup: 'true'
"""

#: The read-only lane: `verify-deep.yml`'s Linux leg. Its `if:` is a platform
#: condition, which C-017 allows — what it forbids is a *trigger* condition.
_READ_LANE = """
name: verify-deep
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
jobs:
  build:
    runs-on: ${{ matrix.job.os }}
    steps:
      - name: Unit tests (Bazel)
        if: %(guard)s
        env:
          BAZEL_CACHE_READ_AUTH: ${{ secrets.BAZEL_CACHE_READ_AUTH }}
        run: |
          %(invocation)s
"""

#: The read-credential lane: the shape both real workflows took once the cache
#: stopped being read anonymously. It calls the rc writer with `read-auth` and
#: **no** `write-auth` in any spelling, so it is not a write lane — but it does
#: put an rc file under `$RUNNER_TEMP`, which is what obliges it to the same
#: `if: always()` deletion. The credential is bound unconditionally on purpose:
#: every lane may read, and a fork PR is given the empty string by GitHub rather
#: than by an expression here.
_READ_CREDENTIAL_LANE = """
name: verify-basic
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
jobs:
  smoke:
    runs-on: ubuntu-latest
    steps:
      - uses: ./.github/actions/bazel-cache-rc
        with:
          read-auth: ${{ secrets.BAZEL_CACHE_READ_AUTH }}
      - name: Unit tests (Bazel)
        run: |
          %(invocation)s
%(cleanup)s"""

_COMPLIANT_INVOCATION = f"bazel test //... --remote_cache=https://{CACHE_HOST} {UPLOAD_OFF}"
_PLATFORM_GUARD = "matrix.job.os == 'ubuntu-latest'"


def _read_credential_lane(cleanup: str = _CLEANUP_STEP) -> str:
    return _READ_CREDENTIAL_LANE % {"invocation": _COMPLIANT_INVOCATION, "cleanup": cleanup}


def _write_lane(
    credential: str = _GATED,
    upload: str = "target/nextest/ci/junit.xml",
    cleanup: str = _CLEANUP_STEP,
    grant: str = _UPLOAD_GRANT,
    second_job: str = "",
    read_credential: str = _NEGATED_GATE,
    invocation: str | None = None,
) -> str:
    substitutions = {"credential": credential, "host": CACHE_HOST, "grant": grant}
    rendered = _WRITE_LANE % {
        **substitutions,
        "read_credential": read_credential,
        "upload": upload,
        "cleanup": cleanup,
        "second_job": second_job % substitutions if second_job else "",
    }
    if invocation is None:
        return rendered
    # The `bazel …` line, replaced whole. Substituting it through the template would
    # mean a second `%(…)s` that every caller has to spell, for one violation.
    original = next(line for line in rendered.splitlines() if line.strip().startswith("bazel "))
    return rendered.replace(original, f"{original.split('bazel ')[0]}{invocation}")


def _read_lane(invocation: str = _COMPLIANT_INVOCATION, guard: str = _PLATFORM_GUARD) -> str:
    return _READ_LANE % {"invocation": invocation, "guard": guard}


_COMPLIANT = {
    "write-lane": _write_lane(),
    "read-lane": _read_lane(),
    "read-credential-lane": _read_credential_lane(),
}

_SWEEPS = {
    "one-credential-source": _lanes_not_carrying_exactly_one_credential_source,
    "write-credential-gate": _ungated_write_credentials,
    "extra-write-lane": _extra_write_lanes,
    "upload-grant": _lanes_enabling_upload_off_the_write_lane,
    "write-lane-uploads-nothing": _write_lanes_uploading_nothing,
    "upload-off": _lanes_not_disabling_upload,
    "outage-policy-flag": _lanes_with_an_outage_policy_flag,
    "remote-cache-reach": _lanes_without_remote_cache_reach,
    "runner-temp-upload": _runner_temp_uploads,
    "interpolated-credential": _interpolated_credentials,
    "rc-writer-cleanup": _rc_writers_without_cleanup,
}

_VIOLATIONS = [
    # DX-81, the ruling's own failure shape and the reason it exists: the `main` push
    # presenting the read credential *as well as* the write one. Everything else about
    # this lane is correct — the write credential is gated, the grant is gated, the
    # credential is cleaned up — and Bazel would send both headers on every `PUT`,
    # leaving the origin's parsing to decide which one authenticated the write.
    pytest.param(
        "one-credential-source",
        _write_lane(read_credential="${{ secrets.BAZEL_CACHE_READ_AUTH }}"),
        id="one-credential-source-read-header-beside-the-write-credential",
    ),
    # The mirror: a pull request carrying the helper credential beside its read header.
    # `write-credential-gate` also reds this, and that is the point — it is the *only*
    # sweep that would have, so a gate loosened in a shape that sweep accepts (a second
    # trusted-event term, say) would land with nothing counting the headers.
    pytest.param(
        "one-credential-source",
        _write_lane(credential="${{ secrets.BAZEL_CACHE_WRITE_AUTH }}"),
        id="one-credential-source-helper-on-a-pull-request-lane",
    ),
    # The third source in the measured trio, and the one neither credential binding can
    # show: a `--remote_header` on the invocation itself, beside the rc file's.
    pytest.param(
        "one-credential-source",
        _write_lane(
            invocation=f'bazel --bazelrc="$RUNNER_TEMP/bazel-cache.rc" test //... '
            f"--remote_cache=https://{CACHE_HOST} "
            f'--remote_header="authorization=Basic $CACHE_AUTH"'
        ),
        id="one-credential-source-remote-header-on-the-invocation",
    ),
    # Zero is the same finding as two, and is the defect this replaced: every request an
    # anonymous 401, the lane compiling cold, green at full cost, with `--remote_cache`
    # in the effective flags the whole time.
    pytest.param(
        "one-credential-source",
        _write_lane(read_credential="''"),
        id="one-credential-source-none-at-all-on-a-pull-request",
    ),
    # A binding whose shape the reader cannot evaluate is a finding, not an empty set:
    # its per-trigger count has been established by nothing. This one falls back to a
    # non-empty string, so the credential is live on a trigger its condition names as
    # the one to withhold it from.
    pytest.param(
        "one-credential-source",
        _write_lane(
            read_credential="${{ github.event_name != 'push'"
            " && secrets.BAZEL_CACHE_READ_AUTH || 'fallback' }}"
        ),
        id="one-credential-source-binding-the-reader-refuses",
    ),
    # S-005's red half, verbatim from A2: "remove the trusted-event condition from the
    # `env:` expression in a scratch workflow and show the assertion fail".
    pytest.param(
        "write-credential-gate",
        _write_lane(credential="${{ secrets.BAZEL_CACHE_WRITE_AUTH }}"),
        id="write-credential-gate-removed",
    ),
    pytest.param(
        "write-credential-gate",
        _write_lane(
            credential="${{ (github.event_name == 'push'"
            " || github.event_name == 'pull_request') && secrets.BAZEL_CACHE_WRITE_AUTH || '' }}"
        ),
        id="write-credential-gate-admits-a-pull-request",
    ),
    pytest.param(
        "write-credential-gate",
        _write_lane(
            credential="${{ (github.event_name == 'push'"
            " && github.ref == 'refs/heads/main') && secrets.BAZEL_CACHE_WRITE_AUTH || 'none' }}"
        ),
        id="write-credential-gate-falls-back-to-a-non-empty-string",
    ),
    # The ruling's "only the main-push job", in its three failure shapes. Each is a
    # state a reviewer waves through, because in each the *other* half is visibly
    # correct: a gate with nothing behind it, a grant with no gate, a credential with
    # no grant.
    #
    # A lane an untrusted contributor triggers, uploading. `_READ_LANE` is
    # `verify-deep.yml`'s Linux leg, which runs on `pull_request` — and the grant here
    # is correctly *gated*, so this reds on containment alone and not on the gate.
    pytest.param(
        "upload-grant",
        _read_lane(invocation=f"{_COMPLIANT_INVOCATION} {_UPLOAD_GRANT}"),
        id="upload-grant-on-a-lane-without-the-write-credential",
    ),
    # The grant on the right lane with the gate dropped: the credential is still
    # `main`-only, so every other trigger attempts uploads it cannot authenticate.
    pytest.param(
        "upload-grant",
        _write_lane(grant=UPLOAD_ON),
        id="upload-grant-not-gated-on-the-trusted-event",
    ),
    # The silent no-op this whole lane exists to avoid: the write credential written
    # to the runner, deleted again, and the committed `.bazelrc` disabling every
    # upload it could have made in between.
    pytest.param(
        "write-lane-uploads-nothing",
        _write_lane(grant=""),
        id="write-lane-holds-the-credential-and-cites-no-grant",
    ),
    # "Only the main-push job" has an "only" in it. A second holder is invisible to
    # `write-credential-gate` when it is gated, and exempts itself from `upload-off`
    # by holding the credential at all.
    pytest.param(
        "extra-write-lane",
        _write_lane(second_job=_SECOND_WRITE_JOB),
        id="a-second-job-holds-the-write-credential",
    ),
    # The substring trap: the token is in the file and not in the flags.
    pytest.param(
        "upload-off",
        _read_lane(
            invocation=f"bazel test //... --remote_cache=https://{CACHE_HOST}\n"
            f"          # {UPLOAD_OFF} is handled elsewhere"
        ),
        id="upload-off-only-in-a-comment",
    ),
    pytest.param(
        "upload-off",
        _read_lane(
            invocation=f"bazel test //... --remote_cache=https://{CACHE_HOST}"
            " --remote_upload_local_results=true"
        ),
        id="upload-off-set-to-true",
    ),
    # C-017: unconditional on every trigger, `push` to `main` included.
    pytest.param(
        "upload-off",
        _read_lane(guard="github.event_name == 'pull_request'"),
        id="upload-off-under-a-trigger-condition",
    ),
    pytest.param(
        "outage-policy-flag",
        _read_lane(invocation=f"{_COMPLIANT_INVOCATION} --remote_local_fallback=true"),
        id="outage-policy-fallback-flag-cited",
    ),
    pytest.param(
        "outage-policy-flag",
        _read_lane(invocation=f"{_COMPLIANT_INVOCATION} --experimental_remote_require_cached"),
        id="outage-policy-outage-made-fatal",
    ),
    # S-016's silent failure: a lane green with no cache reach at all.
    pytest.param(
        "remote-cache-reach",
        _read_lane(invocation=f"bazel test //... {UPLOAD_OFF}"),
        id="remote-cache-reach-absent",
    ),
    pytest.param(
        "remote-cache-reach",
        _read_lane(
            invocation=f"bazel test //... --remote_cache=https://elsewhere.example {UPLOAD_OFF}"
        ),
        id="remote-cache-reach-wrong-host",
    ),
    pytest.param(
        "remote-cache-reach",
        _read_lane(invocation=f"bazel test //... --remote_cache={CACHE_HOST} {UPLOAD_OFF}"),
        id="remote-cache-reach-scheme-less",
    ),
    pytest.param(
        "runner-temp-upload",
        _write_lane(upload="${{ runner.temp }}/bep.json"),
        id="runner-temp-upload-expression",
    ),
    pytest.param(
        "runner-temp-upload",
        _write_lane(upload="$RUNNER_TEMP/bazel-cache.rc"),
        id="runner-temp-upload-shell-variable",
    ),
    pytest.param(
        "interpolated-credential",
        _read_lane(
            invocation="echo '${{ secrets.BAZEL_CACHE_READ_AUTH }}' >> .bazelrc.user\n"
            f"          {_COMPLIANT_INVOCATION}"
        ),
        id="interpolated-credential-into-a-run-body",
    ),
    # C-015's caller half. The first is the state WP-24 would have landed in had
    # it landed at all — a write with no deletion; the second is the one a
    # reviewer waves through, because a cleanup step is visibly present and it is
    # the *condition* that is wrong.
    pytest.param(
        "rc-writer-cleanup",
        _write_lane(cleanup=""),
        id="rc-writer-cleanup-absent",
    ),
    pytest.param(
        "rc-writer-cleanup",
        _write_lane(
            cleanup="""      - uses: ./.github/actions/bazel-cache-rc
        if: success()
        with:
          cleanup: 'true'
"""
        ),
        id="rc-writer-cleanup-not-under-always",
    ),
    # The same obligation, reached through the **read** credential. A lane
    # passing only `read-auth` writes no helper and holds no write credential,
    # and that is exactly why it reads as exempt: the rc file it does leave
    # under `$RUNNER_TEMP` carries a live `--remote_header`.
    pytest.param(
        "rc-writer-cleanup",
        _read_credential_lane(cleanup=""),
        id="rc-writer-cleanup-absent-on-a-read-only-caller",
    ),
]


@pytest.mark.parametrize("sweep", sorted(_SWEEPS), ids=lambda sweep: sweep)
@pytest.mark.parametrize("lane", sorted(_COMPLIANT), ids=lambda lane: lane)
def test_every_sweep_passes_the_compliant_fixture(sweep: str, lane: str) -> None:
    """The green half. A compliant lane some sweep reports is a sweep whose red below
    proves nothing — it would fire on the tree WP-22/23/24 are asked to write."""
    document = yaml.safe_load(_COMPLIANT[lane])
    assert _bazel_lanes([(lane, document)]), (
        f"the {lane} fixture has no bazel lane — it parsed, and every sweep reading it "
        f"reads an empty list"
    )
    findings = _SWEEPS[sweep]([(f"{lane}.yml", document)])
    assert not findings, f"`{sweep}` reports the compliant {lane} fixture: {findings}"


@pytest.mark.parametrize(("sweep", "fixture"), _VIOLATIONS)
def test_every_sweep_reds_its_violating_fixture(sweep: str, fixture: str) -> None:
    """The red half, and the proof that the mutation landed.

    The fixture is asserted different from both compliant ones first: a substitution
    that silently produced the compliant tree would leave the sweep correctly finding
    nothing, and that green reads as "the property survived a violation" when it means
    "no violation was built".
    """
    assert fixture not in _COMPLIANT.values(), (
        f"the `{sweep}` violating fixture is byte-identical to a compliant one — it "
        f"mutates nothing, so a finding here would be impossible for the right reason"
    )
    document = yaml.safe_load(fixture)
    assert _bazel_lanes([("violating", document)]), "violating fixture has no bazel lane"
    assert _SWEEPS[sweep]([("violating.yml", document)]), (
        f"`{sweep}` found nothing in a fixture built to violate it — the sweep is a habit, "
        f"not a check"
    )


def test_the_credential_helper_sweep_reads_an_rc_file(tmp_path: Path) -> None:
    """BZL-CACHE-04's sweep, both halves. Its live counterpart below is parametrized
    over the tracked set, which is empty until WP-11 lands an rc file — and an empty
    parameter set is a visible skip, not a silent pass."""
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
# The live assertions
# ---------------------------------------------------------------------------

_RC_WRITER = PurePosixPath(".github/actions/bazel-cache-rc/action.yml")


def _no_bazel_lane() -> str:
    """The observed cause, named rather than assumed — a skip whose message names a
    cause it never observed is itself a defect."""
    return (
        "no step of any `.github/workflows/*.yml` has a `run:` invoking bazel — observed by "
        "reading every step of every workflow in this tree. WP-23 (`verify-basic.yml`) and "
        "WP-24 (`verify-deep.yml`) were to land the lane swap and **WP-30 returned NO-GO**, "
        "so on current plan no lane lands and this assertion stays skipped rather than "
        "pending. Its red state runs today in `test_every_sweep_reds_its_violating_fixture`."
    )


def _rc_writer_callers(credential: str | None = None) -> list[str]:
    """Every workflow step that `uses:` the rc writer, as `<workflow>:<job>`.

    File existence is not the property these floors mean. `_documents()` reads
    `.github/workflows/*.yml` only, so nothing inside `.github/actions/**` can turn
    them green — only a workflow that calls the action can. WP-22 lands the action in
    wave 3 and WP-23/WP-24 wire it in wave 8; keying on the file would fail the floors
    for those five waves against an action that is doing nothing wrong.

    With `credential` given, only the callers passing that input **non-blank**. The
    two halves are wired by different changes — the read credential goes on every
    lane, the write credential on the `main` push alone — so a floor keyed on "any
    caller" reports a read-only lane as a workflow that forgot its write secret.
    """
    action = _RC_WRITER.parent.as_posix()
    return [
        f"{name}:{job_name}"
        for name, document in _documents()
        for job_name, job in (document.get("jobs") or {}).items()
        for step in (job.get("steps") or [])
        if action in str(step.get("uses", ""))
        and (credential is None or str((step.get("with") or {}).get(credential, "")).strip())
    ]


def _no_rc_writer(credential: str | None = None) -> str:
    """The observed cause, for the floors that go live with their first caller.

    Both halves are wired today. The objection that parked the write half — that the
    tracked `.bazelrc` sets `UPLOAD_OFF` unconditionally, so granting write would mean
    flipping a shipped default — dissolved on measurement: a **command-line** build
    option outranks every rc file, so the `main`-push lane passes `UPLOAD_ON` after the
    command word and `.bazelrc` is not touched. Measured on the pinned 9.2.0 by reading
    the build event stream's canonical command line (`=true` and no `=false` with the
    flag, `=false` without it) and behaviourally with `--disk_cache` pointed at a path
    no rc file names, which Bazel then created.

    These messages stay because a floor's honesty is its skip: if the corpus ever loses
    its write caller again, the assertions below must say so rather than pass over an
    empty list. Their red halves run on every invocation against `_VIOLATIONS`.
    """
    named = f"a non-blank `{credential}`" if credential else "it"
    return (
        f"no workflow step calls `{_RC_WRITER.parent}` with {named} — observed by reading "
        f"every step of every workflow in this tree. Callers of the action itself: "
        f"{_rc_writer_callers() or 'none'}. This floor goes live with the first such caller."
    )


def test_the_write_credential_is_gated_on_a_trusted_event() -> None:
    """S-005 / C-016 — the BZL-CACHE-01 control.

    A same-repo pull request receives the full `secrets` context; GitHub withholds
    secrets from **fork** PRs only, and same-repo is the only PR shape this project
    uses. So nothing about the event type keeps the write credential off a PR lane —
    only this predicate does. `verify-basic.yml`'s `smoke` job is the precedent that
    makes the failure concrete: an unconditional job-level `env:` block hands
    `secrets.SCCACHE_AWS_SECRET_ACCESS_KEY` to every same-repo PR run today.
    """
    documents = _documents()
    named = [
        value
        for _, document in documents
        for _, value in _scalars(document)
        if _WRITE_SECRET.search(value)
    ]
    if not named:
        pytest.skip(
            "no workflow names a `BAZEL_CACHE_WRITE*` secret — observed by walking every "
            "scalar of every workflow. WP-22 (the rc writer) and WP-23 "
            "(`verify-basic.yml`) introduce it; this assertion goes live with them, and its "
            "red state runs today in `test_every_sweep_reds_its_violating_fixture"
            "[write-credential-gate-removed]`."
        )
    ungated = _ungated_write_credentials(documents)
    assert not ungated, (
        f"the cache **write** credential is reachable without a trusted event: {ungated}. "
        f"BZL-CACHE-01 (MUST) requires it absent from any lane an untrusted contributor can "
        f"trigger — not merely unused. Bind it as `${{{{ ({TRUSTED_EVENT_TERMS[0]} && "
        f"{TRUSTED_EVENT_TERMS[1]}) && secrets.<NAME> || '' }}}}`. `{UPLOAD_OFF}` does not "
        f"discharge this: the rule calls that its verification column, not its control"
    )


def test_a_write_credential_exists_once_the_rc_writer_does() -> None:
    """The floor under the test above. Its skip is honest only while nothing names the
    secret; once a workflow passes `write-auth`, a corpus that still names no write
    credential means the gate is asserted over nothing — the shape of a check that never
    ran, not of a clean tree.

    Keyed on a caller passing **`write-auth`**, not on any caller at all. The read
    credential gave the action its first caller while the write half stayed unwired,
    and the broader key read that legitimate state as "the rc writer is wired to
    nothing" — a floor firing on the absence of work nobody has started.
    """
    if not _rc_writer_callers("write-auth"):
        pytest.skip(_no_rc_writer("write-auth"))
    assert any(
        _WRITE_SECRET.search(value)
        for _, document in _documents()
        for _, value in _scalars(document)
    ), (
        "a workflow step passes `write-auth` to the `bazel-cache-rc` action and no workflow "
        "passes it a `BAZEL_CACHE_WRITE*` secret — the rc writer is wired to nothing, and "
        "every credential assertion in this file is skipping over an empty corpus"
    )


def test_every_bazel_lane_carries_exactly_one_credential_source() -> None:
    """DX-81: per lane, per trigger, **one** credential source — never none, never two.

    Both halves of `!= 1` are measured defects of this repository.

    **Zero** is what this file used to assert alone. Before the read credential landed
    every request from a workflow lane was an anonymous 401 and `Unit tests (Bazel)`
    compiled cold on every run, while `--remote_cache` sat in the tracked `.bazelrc`
    and every flag assertion here was green: a flag naming a host is not reach.

    **Two** is what replaced it. With a credential helper and a read header both
    configured, a cache `PUT` was measured carrying three `Authorization` headers —
    Bazel `add`s one per source and sends them all. Which one an origin honours is that
    origin's business, and a design needing to know is the defect whichever way the
    answer goes, so nothing in this repository records or depends on that answer. The
    lanes are disjoint instead: the `main` push presents the write credential alone,
    whose user is in both realms, and every other trigger presents the read credential
    alone.

    A **fork** PR is not a third case. GitHub empties every `secrets.*` there, so the
    runtime count is zero by construction, the action writes no rc file,
    `bazel:test:unit` omits `--bazelrc` and the lane reads anonymously over
    `--disk_cache` — the documented fallback, because a step that fails loudly on a
    fork is the defect.
    """
    documents = _documents()
    if not _bazel_lanes(documents):
        pytest.skip(_no_bazel_lane())
    findings = _lanes_not_carrying_exactly_one_credential_source(documents, _rc_texts())
    assert not findings, (
        f"bazel lanes reaching {CACHE_HOST} with other than one credential source: "
        f"{findings}. None means every request is an anonymous 401 and the lane compiles "
        f"cold — green, at full cost. Two means Bazel sends both headers and the origin "
        f"picks; bind the read credential on `${{{{ !({TRUSTED_EVENT_TERMS[0]} && "
        f"{TRUSTED_EVENT_TERMS[1]}) && secrets.<READ> || '' }}}}` and the write one on "
        f"the same gate un-negated, so the two can never be live together"
    )


def test_every_write_credential_caller_deletes_it_in_an_always_step() -> None:
    """C-015's caller half — the obligation a composite action cannot carry.

    `using: composite` has no `post:` hook, so the credential's deletion is the
    caller's step, stated in `action.yml`'s `cleanup` input and enforced here. The
    latency this closes is specific: `test_a_write_credential_exists_once_the_rc_writer_does`
    fires on the *presence* of a caller, so the first workflow to wire `write-auth`
    could land with no cleanup step and every other assertion in this file would
    stay green.

    Skipped, not pending, while no caller exists: WP-24 was aborted after WP-30
    returned NO-GO and the machinery is parked rather than retired (the reopen
    condition is in the plan). The skip names the cause it observed, and the red
    half runs on every invocation in
    `test_every_sweep_reds_its_violating_fixture[rc-writer-cleanup-absent]` and
    `[rc-writer-cleanup-not-under-always]`.
    """
    documents = _documents()
    if not _rc_writer_callers():
        pytest.skip(_no_rc_writer())
    findings = _rc_writers_without_cleanup(documents)
    assert not findings, (
        f"jobs that write the cache credential and do not delete it: {findings}. A composite "
        f"action has no `post:` hook, so `$RUNNER_TEMP/bazel-cache.rc` and its helper survive "
        f"the job unless the caller runs `{_RC_WRITER.parent}` a second time with "
        f"`cleanup: 'true'` under `if: always()` — `success()` is not enough, because the job "
        f"a leftover credential matters on is the cancelled one (C-015)"
    )


def test_only_the_write_lane_may_upload() -> None:
    """The ruling's first half: `UPLOAD_ON` in the `main`-push job and nowhere else.

    The committed `.bazelrc` keeps `UPLOAD_OFF` for every lane, and this is what keeps
    the one override where it belongs — cited by a job that holds the write credential,
    under the same trusted-event gate that releases it. A grant anywhere else is a lane
    an untrusted contributor can trigger writing to a cache every consumer reads.
    """
    documents = _documents()
    if not _bazel_lanes(documents):
        pytest.skip(_no_bazel_lane())
    findings = _lanes_enabling_upload_off_the_write_lane(documents)
    assert not findings, (
        f"bazel lanes granting upload off the `main` push: {findings}. `{UPLOAD_ON}` belongs "
        f"on the one job holding the write credential, inside `${{{{ ({TRUSTED_EVENT_TERMS[0]} "
        f"&& {TRUSTED_EVENT_TERMS[1]}) && '{UPLOAD_ON}' || '' }}}}` — a command-line build "
        f"option, appended after the command word, so the committed `.bazelrc` needs no edit"
    )


def test_the_write_lane_actually_uploads() -> None:
    """The ruling's second half, and the defect it is easiest to ship.

    A job that holds the write credential and cites no grant writes the credential to
    the runner, runs the build with the committed `.bazelrc`'s `UPLOAD_OFF` in force,
    and deletes the credential again. Every other assertion in this file is green on
    that job: it is gated, it is cleaned up, it reaches the cache, it makes no outage
    fatal. The cache simply never gains an entry, at full cost, indefinitely.
    """
    documents = _documents()
    if not _rc_writer_callers("write-auth"):
        pytest.skip(_no_rc_writer("write-auth"))
    findings = _write_lanes_uploading_nothing(documents)
    assert not findings, (
        f"{findings} — the committed `.bazelrc` sets `{UPLOAD_OFF}` unconditionally and is "
        f"read on every invocation, so this lane uploads nothing. Pass `{UPLOAD_ON}` after "
        f"the command word on the lane that should populate the cache, or stop passing "
        f"`write-auth` to a job that does not"
    )


def test_exactly_one_job_holds_the_cache_write_credential() -> None:
    """The "only" in "only the main-push job".

    `_ungated_write_credentials` judges each binding on its own, so a second job with a
    correctly gated `write-auth` passes it — while `_lanes_not_disabling_upload` stops
    governing that job the moment it holds the credential. A widening of the write
    surface would therefore land green twice over, which is what this refuses.
    """
    documents = _documents()
    if not _rc_writer_callers("write-auth"):
        pytest.skip(_no_rc_writer("write-auth"))
    findings = _extra_write_lanes(documents)
    assert not findings, (
        f"more than one job names a `BAZEL_CACHE_WRITE*` secret: {_write_lane_jobs(documents)}. "
        f"The surplus is {findings}. One lane writes to the shared cache and it is the `main` "
        f"push; every other job reads with `read-auth` and nothing else"
    )


def test_the_bazel_lane_reader_is_not_asleep() -> None:
    """The floor under every lane assertion. Once the rc writer exists the lane swap is
    landing, and a reader finding no lane has drifted rather than found a clean tree — a
    difference a skip cannot express on its own."""
    if not _rc_writer_callers():
        pytest.skip(_no_rc_writer())
    assert _bazel_lanes(_documents()), (
        "no workflow step invokes bazel although a workflow already calls the rc writer — either the "
        "lane swap is wired nowhere, or `_BAZEL` no longer matches how the lane spells its "
        "invocation. Both leave every lane assertion below green over an empty list"
    )


def test_every_non_write_bazel_lane_disables_upload() -> None:
    """C-017 / S-006 / S-007 — `UPLOAD_OFF` in the effective flags of every lane that is
    not the write lane, unconditionally on every trigger.

    S-006's fork lane needs no separate assertion about the secrets themselves: GitHub
    withholds them from a fork PR by construction. What remains checkable, and what this
    covers, is that such a lane cannot upload; that it does not *require* remote reach is
    `test_no_bazel_lane_makes_a_cache_outage_fatal`.
    """
    documents = _documents()
    if not _bazel_lanes(documents):
        pytest.skip(_no_bazel_lane())
    findings = _lanes_not_disabling_upload(documents, _rc_texts())
    assert not findings, (
        f"bazel lanes that may upload to the shared cache: {findings}. BZL-CACHE-01's "
        f"verification treats the absence of `{UPLOAD_OFF}` on an untrusted lane as the "
        f"finding, and C-017 requires it on `verify-deep.yml`'s Linux leg on every trigger, "
        f"`push` to `main` included. Put it in the step's `run:`, or in a tracked rc stanza "
        f"the lane's `--config` activates — both are read here"
    )


def test_a_trigger_condition_is_only_a_finding_when_the_flag_is_in_the_run() -> None:
    """Both sides of the `from_the_run` branch, on two documents differing in one place.

    `_lanes_not_disabling_upload` reports a trigger-varying `if:` because a flag written
    into the step's own `run:` under such a guard holds on some triggers and not others.
    That reasoning does not reach a flag supplied by the **tracked** `.bazelrc`, which is
    unconditional by construction: there the `if:` decides whether the lane runs at all,
    which is scheduling. Read the other way it reported `verify-deep.yml:build` — whose
    guard is the draft-PR gate — so the permissive half is pinned here rather than left
    to the live tree, where the shape that exercises it can be edited away.
    """
    guard = "github.event_name == 'pull_request'"
    rc = f"build --remote_cache=https://{CACHE_HOST}\nbuild {UPLOAD_OFF}\n"

    in_the_run = yaml.safe_load(_read_lane(guard=guard))
    findings = _lanes_not_disabling_upload([("in-the-run.yml", in_the_run)])
    assert findings and "trigger condition" in findings[0], (
        f"a lane whose own `run:` carries `{UPLOAD_OFF}` under `{guard}` must still be a "
        f"finding — that flag really does hold on one trigger and not the others. Got "
        f"{findings}"
    )

    from_the_rc = yaml.safe_load(_read_lane(invocation="bazel test //... --config=ci", guard=guard))
    assert _bazel_lanes([("from-the-rc.yml", from_the_rc)]), "the rc-supplied fixture has no lane"
    assert not _lanes_not_disabling_upload([("from-the-rc.yml", from_the_rc)], [rc]), (
        f"the same `{guard}` guard over a lane taking `{UPLOAD_OFF}` from an unconditional "
        f"tracked rc stanza is not a finding: the stanza holds on every trigger, so the "
        f"`if:` governs whether the lane runs, not what flags it runs with"
    )


def test_no_bazel_lane_makes_a_cache_outage_fatal() -> None:
    """S-008 — an unreachable cache host is a warning and exit 0, by construction.

    There is nothing to assert *for* here: with no `--remote_executor` an outage is
    already non-fatal on every Bazel version measured. What can go wrong is a lane citing
    a fallback flag as though it governed this (BZL-CACHE-26, whose finding is the flag's
    presence) or requiring a cache hit outright.
    """
    documents = _documents()
    if not _bazel_lanes(documents):
        pytest.skip(_no_bazel_lane())
    findings = _lanes_with_an_outage_policy_flag(documents, _rc_texts())
    assert not findings, (
        f"lanes citing a flag that does not govern a cache-only outage, or that makes one "
        f"fatal: {findings}. Measured on 8.7.0 and 9.2.0, all four fallback-flag "
        f"combinations give `WARNING: Remote Cache: Connection refused`, every action local, "
        f"exit 0 — so a fallback flag here is dead configuration presented as an outage "
        f"policy (BZL-CACHE-26), and `--remote_require_cached` turns the outage into the red "
        f"build S-008 forbids"
    )


def test_a_bazel_lane_reaches_the_remote_cache() -> None:
    """S-016's structural half — a lane pointing nowhere is the silent zero-hit lane.

    "Zero hits → the lane is green with no cache reach at all, indefinitely and silently"
    is what S-016 exists to catch, and the configuration half of it is checkable here and
    now: a `--remote_cache` in the effective flags, with an explicit `https://` scheme and
    this repository's host. The **hit count** is not derivable from configuration; what
    counts as a hit is `remote_cache_hits`, and
    `test_s016_is_not_satisfiable_by_a_disk_cache_hit` is its red and green.
    """
    documents = _documents()
    if not _bazel_lanes(documents):
        pytest.skip(_no_bazel_lane())
    findings = _lanes_without_remote_cache_reach(documents, _rc_texts())
    assert not findings, (
        f"bazel lanes with no reach to the shared cache: {findings}. A lane that reads no "
        f"cache is green forever at full cost, which is the whole value proposition going "
        f"unasserted (S-016); a scheme-less URI is worse than none, since both remote flags "
        f"default to `grpcs` and would dial a port nothing serves"
    )


@pytest.mark.parametrize(
    "rc_file", _tracked_rc_files(), ids=lambda path: path.relative_to(ROOT).as_posix()
)
def test_no_tracked_rc_file_configures_a_credential_helper(rc_file: Path) -> None:
    """BZL-CACHE-04 (MUST) / ADR ruling 4a — the helper is never in a shipped rc file.

    Parametrized over the tracked set rather than looped inside one test: with no tracked
    rc file the parameter set is empty and pytest reports the case as skipped, which is a
    visible "nothing was read". A loop would report the same green as a clean tree.
    """
    findings = _rc_files_configuring_a_credential_helper([rc_file])
    assert not findings, (
        f"a tracked rc file configures `--credential_helper`: {findings}. Bazel spawns that "
        f"program with the full client environment, before the sandbox, on a read-only "
        f"`bazel query //...` from a fresh clone (bazel#30439, closed as intended). The "
        f"helper belongs in `$RUNNER_TEMP/bazel-cache.rc`, written by "
        f"`.github/actions/bazel-cache-rc` on a trusted event (ADR ruling 4a)"
    )


def test_no_upload_step_takes_a_runner_temp_path() -> None:
    """ADR ruling 4b line 4. Live over the whole workflow corpus, not only bazel lanes:
    the rc file and the credential helper live under `$RUNNER_TEMP`, and an uploaded
    artifact is a download for anyone who can read the run.

    Floored on its reader — no upload step found means the sweep stopped reading, which is
    not the same as the tree being clean.
    """
    documents = _documents()
    uploads = [
        step
        for _, document in documents
        for job in (document.get("jobs") or {}).values()
        for step in (job.get("steps") or [])
        if "upload" in str(step.get("uses", ""))
    ]
    assert uploads, (
        "no workflow step uses an upload action — the reader found nothing, which is not "
        "the same as no step taking a `$RUNNER_TEMP` path"
    )
    findings = _runner_temp_uploads(documents)
    assert not findings, (
        f"upload steps taking a path under `$RUNNER_TEMP`: {findings}. That directory holds "
        f"the CI-only rc file and the credential helper, whose whole job is to print the "
        f"credential on stdout; `bep_to_otlp.py`'s inputs are written elsewhere"
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
        f"every step script to a file under `/home/runner/work/_temp/`, so the "
        f"`${{{{ secrets.X }}}}` spelling writes the credential to a second on-disk location "
        f"and through the expression-expansion path. Pass it as a step `env:` entry instead"
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
# S-016's owner-gated half
# ---------------------------------------------------------------------------


def _authorization(credential: str) -> str:
    """The `Authorization` header value for `credential`.

    `BAZEL_CACHE_READ_AUTH` feeds `--remote_header=authorization=…` (ADR ruling 5b), so it
    is normally a complete header value. A bare `user:pass` is encoded rather than sent
    verbatim: a red because the value had the wrong shape is indistinguishable from the red
    this test exists for, and an indistinguishable red is as useless as an indistinguishable
    green.
    """
    if ":" in credential and " " not in credential:
        return "Basic " + base64.b64encode(credential.encode()).decode()
    return credential


def test_the_remote_cache_serves_the_read_credential() -> None:
    """S-016 — owner-gated, and the one test here that touches the network.

    **Why this cannot be structural.** S-016's subject is that a warm same-repo PR lane
    reports *nonzero* remote cache hits. No property of the configuration establishes that:
    a perfectly-configured lane reads zero blobs from a realm answering 401 to every
    method, which is what `bazel-cache.ocx.sh` does today (plan M-04). The reader realm
    exists only on an unpushed `server-hetzner1` branch and applying it is owner-only, so a
    test *requiring* it would sit permanently skipped — the unchecked green C-029 forbids.

    **What this asserts, and its red state.** Precondition: `BAZEL_CACHE_READ_AUTH`
    exported — observable, and independent of the outcome, so the assertion is not its own
    precondition. Assertion: the cache host answers that credential with something other
    than 401/403, i.e. the reader realm is deployed and the account works. Red today, on
    demand and for the right reason: export any value and the host 401s. Green is
    unreachable until the owner acts, and saying so is the honest half of this.

    **What this does not assert.** The hit count itself — that needs a warm cache and a
    real build on a lane, and WP-30 returned NO-GO, so no such lane lands. What a hit *is*
    no longer waits on any of that: `test_s016_is_not_satisfiable_by_a_disk_cache_hit`
    settles it here, red and green, against real BEP bytes.
    """
    credential = os.environ.get(READ_CREDENTIAL, "")
    if not credential:
        pytest.skip(
            f"`{READ_CREDENTIAL}` is unset in this process environment — observed via "
            f"`os.environ`. The GitHub secret does not exist and the reader realm of "
            f"{CACHE_ENDPOINT} is not deployed (plan M-04, C-029). **Owner action that "
            f"unblocks this:** apply `server-hetzner1` branch `bazel-cache-reader-realm`, "
            f"create repository secrets `{READ_CREDENTIAL}` and `BAZEL_CACHE_WRITE_AUTH`, "
            f"then export `{READ_CREDENTIAL}` for this run. The nonzero-hit half of S-016 "
            f"needs a bazel lane, which WP-30's NO-GO means nothing lands; its predicate is "
            f"asserted here regardless by "
            f"`test_s016_is_not_satisfiable_by_a_disk_cache_hit`."
        )
    request = urllib.request.Request(
        CACHE_ENDPOINT,
        method="HEAD",
        headers={
            "Authorization": _authorization(credential),
            "User-Agent": PROBE_USER_AGENT,
        },
    )
    try:
        answer = urllib.request.urlopen(request, timeout=10)
        status, headers = answer.status, answer.headers
    except urllib.error.HTTPError as refused:
        status, headers = refused.code, refused.headers
    except OSError as unreachable:
        pytest.skip(
            f"`{READ_CREDENTIAL}` is set but {CACHE_ENDPOINT} is unreachable from this host: "
            f"{unreachable!r}. That is a transport failure, not a verdict on the realm — "
            f"S-008 is the assertion that an unreachable cache is survivable."
        )
    # nginx's `auth_basic` always offers a challenge; Cloudflare's signature block does
    # not. Without this the probe reports the edge's verdict under the realm's name.
    if status == 403 and "WWW-Authenticate" not in headers:
        pytest.skip(
            f"{CACHE_ENDPOINT} answered 403 with no `WWW-Authenticate` challenge "
            f"(CF-RAY={headers.get('CF-RAY')!r}, server={headers.get('Server')!r}) — the "
            f"Cloudflare edge refused this client before nginx saw it, so this is not a "
            f"verdict on the reader realm. Probe from a host the edge admits."
        )
    assert status not in (401, 403), (
        f"{CACHE_ENDPOINT} answered {status} to `{READ_CREDENTIAL}` — the reader realm is not "
        f"deployed, or this account is not in `bazel-cache-readers.htpasswd`. Every lane "
        f"holding this credential then reads zero blobs and stays green at full cost, which "
        f"is exactly the silent failure S-016 exists to make loud"
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


def test_cross_compile_scans_its_release_build() -> None:
    """AM-2: the one release build (no `__testing`) on every `main` push scans green on real bytes."""
    steps = _run_steps(_load(VERIFY_DEEP)["jobs"]["cross-compile"])
    scans = [index for index, run in steps if f"{PROVENANCE_CHECK} --scan" in run]
    builds = [index for index, run in steps if "cargo xwin build --release" in run]
    assert scans, "verify-deep.yml `cross-compile` has no `release_provenance_check.py --scan` step"
    assert builds and min(scans) > min(builds), f"--scan at step {scans}, build at step {builds}"
