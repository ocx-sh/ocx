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
- the tier partition (D5): `verify-basic.yml` is what a pull request pays
  for, so it runs on Linux only, and the Windows and macOS unit legs are
  `verify-deep.yml`'s `build` matrix. A leg re-added to basic is a cost every
  push pays twice; a leg dropped from deep's matrix is a `cfg(windows)` /
  `cfg(target_os = "macos")` arm nothing compiles anywhere.

The workflows are read with PyYAML (D11). One YAML 1.1 trap: the bare key
`on` loads as the boolean `True`, so `_on` looks it up under both spellings.
Glob liveness is `git ls-files -- ':(glob)<pattern>'`: git's `:(glob)`
pathspec is GitHub's filter grammar (`*` stops at `/`, `**` crosses it, a
leading `**/` matches at any depth), so no matcher is written here.

Run:
    uv run --directory .claude/tests pytest test_workflows.py -q
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

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
    return {
        f"{document.relative_to(ROOT).as_posix()}: {workflow} {job}"
        for document in sorted((ROOT / ".claude").rglob("*.md"))
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
# WP-42 R16: a parser's gate step and its self-test travel together in CI
# ---------------------------------------------------------------------------

# `<gate step>` → `<the step that proves that gate's parser>`. Both are shell
# parsers whose red state is a pair of fixture logs; a job that runs the gate
# without the proof is green whether or not the parser still matches anything.
# Keys and values are compared against the whole `run:` line, never as a
# substring: `task rust:test:ceiling` is a prefix of its own self-test's
# command, so a substring test would find the proof in place of the gate.
_CI_SELF_TEST_PAIRS = {"task rust:test:ceiling": "task rust:test:ceiling:self-test"}


@pytest.mark.parametrize(("gate", "proof"), sorted(_CI_SELF_TEST_PAIRS.items()))
def test_a_ci_job_running_a_parser_gate_also_runs_its_self_test(gate: str, proof: str) -> None:
    carriers = [
        (workflow.name, name, job)
        for workflow in WORKFLOWS
        for name, job in (_load(workflow).get("jobs") or {}).items()
        if any(str(step.get("run", "")).strip() == gate for step in (job.get("steps") or []))
    ]
    assert carriers, f"no workflow job runs `{gate}` — this pairing asserts nothing"
    for workflow, name, job in carriers:
        proofs = [step for step in (job.get("steps") or []) if str(step.get("run", "")).strip() == proof]
        assert proofs, (
            f"{workflow} job `{name}` runs `{gate}` but never `{proof}` — the gate's own "
            f"parser is then proved nowhere in CI (WP-42 R16)"
        )
        assert any(not _disarmed(job, step) for step in proofs), (
            f"{workflow} job `{name}` runs `{proof}` only disarmed "
            f"({[_disarmed(job, step) for step in proofs]}) — a tolerated failure is the "
            f"same green as no step"
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
    # The deep workflow runs the acceptance suite with the same xdist flags
    # the local gate uses. It once spelled this `task test`, which is the
    # SERIAL entry point: 21:41 for 3819 tests on an idle four-core runner.
    "test:parallel": ("verify-deep.yml", "task test:parallel"),
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
