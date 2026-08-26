# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""NFR latency gate for the per-prompt environment reconciler (C-044, S-045).

Run standalone (from ``test/``)::

    uv run python bench/shell_latency.py --out bench/results/shell-latency.json

or, the supported form, ``task test:shell-latency``.

What this gate is
-----------------
C-044 budgets the per-prompt no-op at ``exec_floor + Δ`` and states two
obligations, only one of which is a wall clock:

1. **Zero execs on the no-op prompt path.** The emitted hook carries the
   watch-set paths and decides shell-side with ``[ file -nt stamp ]`` builtins.
   :func:`measure_exec_counts` drives the *real* emitted hook and counts actual
   ``exec``s of the binary. This is the load-bearing gate: it is deterministic,
   it is what users pay on every prompt, and it is asserted in **both**
   directions in every run — quiet prompts must exec **0** times and a touched
   watch-set path must exec **exactly 1** time. A harness that never execs fails
   the second assert; one that always execs fails the first. Neither a vacuous
   green nor a vacuous red is reachable.

2. **``exec_floor + Δ``, Δ ≤ 2 ms, floor measured in the same job.** Applied to
   both halves C-044 names — ``ocx self activate`` at shell startup and ``ocx
   self activate --reconcile`` per prompt — each against the bare-exec floor.
   Both sides of each delta are the min of N ≥ 10 **separate process spawns**,
   interleaved, never a two-sample subtraction: each sample is independently
   exposed to scheduler placement even inside one job, and only ``min`` is
   robust to that, because noise can inflate a sample but never deflate one.

Measuring what ships, and nothing else
--------------------------------------
Neither measured command is written down. Both are **derived at run time from
the shipped artifact that issues them**, one rung apart:

* ``startup`` is **recorded from the real POSIX shim**
  (:func:`startup_command`): the shipped ``ENV_SH`` body is sourced by a real
  interactive ``bash``, with a recorder standing in for ``$_ocx_bin``, and the
  argv it captures is the argv that gets timed. Two earlier versions were
  hand-copied and both measured something no shell runs — one passed
  ``--no-hook`` (skipping ``resolve_walk`` and ``reconcile::watch_paths``, the
  entire per-shell-start cost this gate exists to bound, reporting 0.798 ms for
  a 1.494 ms path), the next forced ``--hook``, which is **rung 2** of the hook
  ladder and therefore bypasses the rung the shim actually drives. Recording
  removes the copy: see "The decision under test" below.
* ``reconcile`` is **parsed out of the hook body the binary just emitted**
  (:func:`_derive_reconcile_command`), so it cannot drift from what a prompt
  runs. It was previously hand-copied, and the copy disagreed with the shipped
  hook about ``--offline``, so CI timed a command no prompt runs — at 35.9 ms
  total against a 3.8 ms floor on this box, Δ 32.1 ms, an order of magnitude
  over the budget, and network-dependent on top (a second measurement of the
  same disagreement reported 47.9 ms). That is precisely why the command is
  derived rather than written down: no recorded number has to stay true for the
  gate to stay aimed, and the flag can move in ``hook.rs`` — as it since has —
  without this file knowing.

The decision under test
-----------------------
Whether a prompt hook is emitted at all is decided by a five-rung ladder
(``crates/ocx_cli/src/options/hook.rs``). A shim never spells its answer as
``--hook``: that is rung 2, and it would outrank both ``OCX_NO_HOOK`` and
``[shell] hook``. Every shim instead states **interactivity** —
``--interactive`` / ``--no-interactive``, from ``$-``, ``status is-interactive``
or ``[Environment]::UserInteractive`` — and that pair is the *input* to rung 5
(``Interactive::resolve`` → ``Hook::enabled``). Rung 5 is the rung a real shell
takes, so it is the rung this gate must time.

That is not a stylistic preference. The defect this whole contract exists to
close was that no shell ever registered the hook through the install path, and
it survived because every test spelled its intent as ``--hook`` and so drove
rung 2. A benchmark forcing ``--hook`` reproduces that shape exactly: it stays
green while ``Interactive::resolve`` or a shim's flag emission is broken.

Recording the argv from the shim closes both halves at once, and the closure is
load-bearing rather than decorative — :func:`_activation_stream` requires the
recorded command's stdout to carry a prompt hook, so:

* break ``Interactive::resolve`` (ignore the flags, fall through to the probe)
  and the tty-less arena resolves non-interactive, emits no hook, and the gate
  raises;
* break the shim's emission (send ``--no-interactive``, or drop the pair) and
  the recorded argv itself carries the break, with the same result.

Both leave ``--hook`` working, which is the point: the old gate could not see
either.
* Both are structurally checked before they are timed, and the check has two
  halves in two places: :func:`_activation_stream` requires the startup
  command's stdout to **carry** the prompt hook, and :func:`measure_wall_clock`
  requires the floor command's stdout to **not** carry one — otherwise the
  subtraction cancels the very work it reports. Both raise rather than returning
  a failing gate, so ``--expect-fail`` cannot absorb either.

When the runner is too noisy to have an opinion
----------------------------------------------
A wall-clock gate that reds without a regression is worse than no gate: it
teaches its readers to re-run until green, which is how a real regression gets
waved through. Both Δ asserts therefore have three outcomes, not two, and the
third is **INCONCLUSIVE** — no verdict, loudly reported, exit 0 on the wall
clock alone.

Which outcome applies is decided by :func:`_budget_gate` from
:func:`floor_spread_ms`: the spread of the **bare-exec floor**, measured in the
same interleaved cycles. A Δ under budget always passes, contended or not,
because contention inflates a min-of-N and never deflates one. A Δ over budget
is a red when the floor was tight enough to resolve that Δ, and an abstention
when it was not. The threshold is the Δ itself — no new constant, because "you
cannot measure a 2 ms difference on a machine that scatters a bare exec by more
than 2 ms" is not a tunable.

Three things stop that from becoming an amnesty:

* the classifier reads the **floor series alone** — never the Δ, never the
  breach, never the injected delay, which lands only in the measured processes;
* only the two wall-clock gates can abstain. The exec counts and the reconciler
  fixed point are deterministic, decide on every run, and a red in either is
  still a red run on the noisiest runner there is;
* the fault-injection step is its live control. The injection leaves the floor
  untouched, so an injected run on any machine that can measure at all reaches
  the red branch — and a classifier that started abstaining everywhere would
  make that step report no red and fail.

What else this gate asserts, and what it only records
-----------------------------------------------------
* The per-prompt ``self activate --reconcile`` Δ, as ``reconcile.delta_ms``,
  against :data:`RECONCILE_BUDGET_MS` — C-044's original 2 ms, restored
  (ocx-sh/ocx#340). Its red state is demonstrated by a fault injection aimed at
  it specifically.
* The **cold** cost of the same command, as ``reconcile.cold_delta_ms``, where
  "cold" means the host-capability record was deleted before the spawn. Recorded
  only, and the reason the asserted number is the warm one: see "Cold and warm"
  below.
* The **shell-side** cost of applying a reconcile stream, as
  ``eval.ms_per_apply`` over ``eval.stream_bytes``
  (:func:`measure_reconcile_streams`). Every other number here times ocx's own
  process; this one times the work the *shell* does with ocx's output, which is
  a per-prompt cost no spawn measurement can see. The same function reads the
  reconciler's **fixed point** (ocx-sh/ocx#342), which *is* asserted: the first
  fire must apply the arena's path entries and the steady-state fire after it
  must apply none.
* The **arena's own non-vacuity**, as :func:`_consent_gates` — see the next
  section. Asserted.

What the arena has to contain for any of it to mean anything
------------------------------------------------------------
A per-prompt latency number is a number *about a project*, and the per-project
terms that matter all walk ``lock.tools``: composition resolves an entry per
tool, and the consent predicate derives a source per tool. So the lock's tool
count is the arena's scaling knob, and a tool-free lock pins it at zero.

One term is *not* on the timed path, and saying which matters more than the
total. ``project::consent::verified_sources`` — the ``refs/origins/`` read, one
directory per tool — is gated on ``whitelist.namespaces.is_some()``, because
both branches that consume it sit behind ``namespace_granted``. The timed leg
grants through clause 3 (``OCX_CONSENT_PATHS``), so it does not pay that read at
all. An earlier revision of this paragraph asserted the opposite — that
``activate.rs`` computes it unconditionally under every grant — which was true
until the read was gated. The deltas below are therefore real tool-count
scaling, but they come from composition and the lock walk, not from the store
read; :func:`_consent_gates` states what that leaves unmeasured.

Until 2026-08-26 this arena's lock had **no tools**. The loop body ran zero
times, and a regression whose cost scaled with locked-tool count was invisible to
this gate by construction — the docstring even defended the shape, on the grounds
that an ``[env]``-only project "is the only kind a latency gate can build
hermetically". It is not. Consent reads the lock for its **source set** and the
store for its **record**, and a record is a plain file: ``shell_matrix``'s
``lock_tool``, ``declaration_hash_of`` and ``record_origin`` build the whole
thing with no registry, no package and no network. :data:`WALL_PROJECT_TOOLS`
tools now go in.

Half a fix would have been worse than none, because ``verified_sources``
``return``s at the **first** tool the store cannot corroborate: an arena that
declares eight tools and corroborates none measures one iteration and looks
exactly like one that measures eight. So the tools are asserted from both sides —
:func:`_consent_gates` gates the lock's tool count *and* drives the same lock
through consent clause 2 (:func:`measure_clause_two`), where a grant is
reachable only when every tool corroborates. What clause 2 costs is not timed and
does not need to be; :func:`_consent_gates` says why.

Cold and warm
-------------
``HostCapabilities`` persists its per-host answer at
``$OCX_HOME/state/host/capabilities.json`` with a one-hour TTL
(``crates/ocx_lib/src/oci/host_capabilities.rs``). Every number here is measured
**warm**, and deliberately:

* Warm is what a prompt pays. The record outlives the process, so within a TTL
  hour every prompt after the first reads it. C-044 budgets a *per-prompt* cost.
* Warm is not achieved by accident: :func:`measure_wall_clock` runs each command
  once before sampling, and a min over N ≥ 2 samples could not report a cold
  figure even if it did not.
* Warm-only is safe **because cold is over budget**, measured here at Δ 3.150 ms
  against a 2 ms budget. So a regression that stopped the record persisting —
  which would make every prompt cold — cannot hide behind this gate; it reds it.
  The floor cannot cancel it either: bare ``ocx version`` runs on the static
  bypass and never calls ``detect_and_cache`` (``command/version.rs``), so the
  detection cost lands entirely in the delta.

The cold leg is measured anyway, interleaved and recorded, because nothing else
would notice the once-per-hour cost growing. ``reconcile.cold_record`` says
whether a record was actually there to delete: the binary persists one on Linux
only, and on a host that persists nothing "cold" and "warm" are the same path —
which the artifact then states rather than implies.

Fault injection (plan §7.6)
---------------------------
``__OCX_TESTING_LATENCY_INJECT_MS`` is passed into the **measured processes** and
consumed inside the binary under the ``__testing`` feature — not slept off in
this harness. That placement is the contract (C-044:1826 asks for extra work on
the measured code path) and it is what makes the injection *discriminating*: a
gate aimed at the wrong command records no delay at all, so ``--expect-fail``
fails instead of certifying a measurement of the wrong thing.

There are **two** seams, one per asserted budget:
``ocx_lib::shell::hook::registration``, which only a hook emission reaches, and
``ocx_lib::shell::hook::checkpoint``, which only ``--reconcile`` emits. One run
covers both, because the two budgets are the same 2 ms and each measured command
is a separate process with its own copy of the variable: the startup process
pays at registration, the reconcile process at checkpoint, and the floor — which
never receives the variable, and would ignore it anyway — pays at neither.
``--expect-fail-gate`` is repeatable and **every** named gate must be red, so a
red somewhere else cannot stand in for the red under test. The value is written
into the artifact as ``injected_delay_ms``, so a green produced under injection
is a self-evident contradiction in the run's own output.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Path bootstrap — allow `python bench/shell_latency.py` from test/
# ---------------------------------------------------------------------------
_BENCH_DIR = Path(__file__).resolve().parent
_TEST_DIR = _BENCH_DIR.parent
for _candidate in (_TEST_DIR, _TEST_DIR / "src"):
    if str(_candidate) not in sys.path:
        sys.path.insert(0, str(_candidate))

import shell_matrix as matrix  # noqa: E402

# ---------------------------------------------------------------------------
# Contract constants
# ---------------------------------------------------------------------------

#: C-044's Δ for the **shell-startup** pass. Not a tunable: it is the contract,
#: so it is a constant and not a flag. Changing it is a spec change.
DELTA_BUDGET_MS = 2.0

#: C-044's Δ for the **per-prompt reconcile**. The same 2 ms the spec has always
#: named, restored 2026-08-26 after a brief amendment to 25 ms (ocx-sh/ocx#340).
#:
#: The amendment was wrong about its own cause. The ~16 ms it was fitted to was
#: not the reconciler: it was `HostCapabilities::detect_and_cache` walking the
#: loader directories on every process, ~7,800 per-entry `file_type().await`
#: hops whose answer changes no byte of the emitted stream. With that walk moved
#: to one `spawn_blocking` over `std::fs` and its answer persisted per host, the
#: reconcile measures **Δ 1.000 – 1.276 ms** (min-of-15 reconcile minus
#: min-of-15 floor, ten interleaved runs on a WSL2 dev box) — inside the
#: contract, so the contract stands as written. The emitted stream is unchanged.
#:
#: Headroom against the worst of those ten runs is 0.724 ms. That is what the
#: fault injection is sized against: see `test/taskfile.yml`.
#:
#: The headroom is real but not generous, and the shortfall is *contention*
#: rather than code: 28 of 29 runs measured 1.00–1.67 ms, and the one that did
#: not measured 2.96 ms while an unrelated `rustc` held 88% of a core on the same
#: box. A measured command does more work than the floor, so it collects more
#: than its share of a scheduling storm and `min` does not fully subtract that
#: out — reproduced deliberately since, at 0.96–2.96 ms across eight runs with
#: three rival spinners on one cpu, and at deltas so scrambled they went
#: **negative** on a 32-core box carrying 96 of them.
#:
#: The budget did not move for that. Widening it is what the withdrawn 25 ms
#: amendment did, and the cost was a year of not measuring the thing. Instead the
#: measurement now has to be *admissible* before it is asserted:
#: :func:`floor_spread_ms` reports how far the bare-exec floor itself scattered,
#: and a breach whose overshoot is smaller than that scatter is NO VERDICT
#: rather than a red (:func:`_budget_gate`). The comparison is against the
#: overshoot, not against this budget: resolving 2 ms is not the question, and
#: asking it certified a 0.010 ms breach as a confident red. A breach the
#: machine could resolve is a regression, and now says so without the caveat.
RECONCILE_BUDGET_MS = 2.0

#: The floor the shell-startup **positive control** demands (see
#: :func:`evaluate`). Measured, not chosen: eight interleaved reps of
#: `startup` vs `ocx version`, and of `ocx version` vs *itself* as the
#: same-work case, gave
#:
#: ===================  ==================  =====================
#: statistic            startup vs floor    floor vs floor
#: ===================  ==================  =====================
#: median Δ             +0.462 … +0.617     −0.139 … +0.143
#: min Δ                +0.412 … +0.791     −0.132 … +0.142
#: ===================  ==================  =====================
#:
#: which is why the control uses the **median** delta and a positive floor. Two
#: findings drove both choices. A `> 0` threshold passed the same-work case in
#: six of eight reps, so the control as written was three-quarters vacuous. And
#: the min delta is the noisier of the two estimators here (spread 0.379 vs
#: 0.155) — it has not converged at N=15, and a min-of-15 draw as low as 0.115
#: has been observed on a healthy tree, which any useful positive floor would
#: have red. 0.25 ms splits the measured gap: ~1.8x above the worst same-work
#: draw and ~1.8x below the smallest healthy one.
STARTUP_WORK_FLOOR_MS = 0.25

#: Samples per command. The plan's floor is N >= 10 for both the floor and the
#: total. 15 costs 0.207 s of the run's wall clock (10 costs 0.153 s). Raising it
#: to 25 costs 0.361 s and measured no tighter a min: over eight runs at 25 the
#: startup Δ ranged 0.359–0.636 ms and the reconcile Δ 1.026–1.275 ms, against
#: 0.303–0.557 and 1.000–1.276 over ten runs at 15. The marginal sample has
#: stopped buying anything by 15.
SAMPLES = 15

#: Quiet prompts fired after convergence. Any exec here is a broken
#: short-circuit, so more is only more evidence. They are pure shell builtins:
#: the nine past the first cost 7 ms in total, and the whole exec-count phase
#: including arena setup runs in 0.072 s.
QUIET_PROMPTS = 10

#: Applies of the steady-state stream timed in one shell session. The stream is
#: ~3.5 KB of `PATH` string surgery, so 200 puts the loop around 0.3 s while
#: pushing the per-apply figure two decimal places clear of the clock's
#: microsecond resolution.
EVAL_ITERATIONS = 200

#: Fault-injection knob (plan §7.6). Testing-only, hence the `__OCX_TESTING_`
#: prefix. 0 / unset is the shipped state. It is passed to every measured process
#: except the floor, and consumed in `hook::registration` (startup) and
#: `hook::checkpoint` (reconcile) under the `__testing` feature.
INJECT_ENV = "__OCX_TESTING_LATENCY_INJECT_MS"

#: Where `HostCapabilities` persists its per-host detection record, relative to
#: `$OCX_HOME` (`crates/ocx_lib/src/oci/host_capabilities.rs`). Deleting it is
#: what makes a sample cold — see "Cold and warm" in the module docstring.
CAPABILITY_RECORD = Path("state") / "host" / "capabilities.json"

#: The two `--expect-fail-gate` substrings `test/taskfile.yml` passes. Held here
#: so :func:`self_check` can prove each one matches its own gate and *only* its
#: own gate: a gate rename then reds the self-check, which runs before any
#: process is spawned. A drift between these and the taskfile's copies is caught
#: from the other side — an unmatched needle exits 1.
STARTUP_GATE_NEEDLE = "shell startup <="
RECONCILE_GATE_NEEDLE = "per-prompt reconcile"


# ---------------------------------------------------------------------------
# Pure evaluation — same shape as bench/compare.py: no I/O, no sys.exit, no
# print, so it is unit-testable and the caller owns output and process exit.
# ---------------------------------------------------------------------------


@dataclass(slots=True)
class Gate:
    """One asserted property, with the numbers that decided it.

    ``inconclusive`` marks a wall-clock gate whose budget could **not** be
    asserted because the runner was preempting the bare-exec floor by more than
    the Δ under test (:func:`floor_spread_ms`). Such a gate carries
    ``passed=True`` — it did not fail, because nothing was decided — and is
    reported as its own status everywhere a status is printed. Only the two
    budget gates can ever be inconclusive; the exec counts and the fixed point
    are deterministic and always decide.
    """

    name: str
    observed: float
    budget: float
    passed: bool
    unit: str
    note: str = ""
    inconclusive: bool = False


@dataclass(slots=True)
class LatencyReport:
    """Full gate report: what was asserted, and what was only recorded."""

    gates: list[Gate]
    records: dict[str, Any] = field(default_factory=dict)
    passed: bool = False

    @property
    def inconclusive(self) -> list[Gate]:
        """The gates that abstained. Non-empty means no wall-clock verdict."""
        return [gate for gate in self.gates if gate.inconclusive]


def unmatched_gate_needles(needles: Sequence[str], gates: Sequence[Gate]) -> list[str]:
    """The needles in ``needles`` that name no **failed** gate.

    PURE. Empty means every needle went red, which is what ``--expect-fail-gate``
    demands. Extracted from :func:`main` so both of its colours can be shown on
    inputs nobody has to schedule: an always-empty version of this function would
    make every ``--expect-fail-gate`` run exit 0 whatever went red, which is the
    exact failure mode the flag exists to prevent for the gates themselves.

    An **inconclusive** gate is not a red one and never satisfies a needle: an
    abstention is the absence of a verdict, and reading it as the demonstrated
    red state would let a contended runner certify a fault injection it never
    observed. :func:`main` tells the two apart before it gets here.
    """
    red = [gate.name for gate in gates if not gate.passed]
    return [needle for needle in needles if not any(needle in name for name in red)]


def floor_spread_ms(floor_samples: Sequence[float]) -> float:
    """How far the **bare-exec floor** scattered in this run, in ms.

    PURE. The 90th-percentile floor sample minus the fastest one: the amount by
    which this machine delayed a process doing *none* of the measured work,
    across the same interleaved cycles that produced the deltas.

    The 90th percentile and not the max, for the same reason the deltas use
    ``min``: a single unlucky sample is what min-of-N exists to absorb, and
    calling one outlier "contention" would abstain on a healthy tree (a max-based
    spread already drew 2.16 ms on an idle box, inside the 19-run idle sample
    below, whose p90 spread never passed 1.19 ms).

    Compared against the Δ under test by :func:`_budget_gate`, which needs no
    threshold constant of its own: a 2 ms difference is not resolvable on a
    machine whose bare exec is itself scattered by more than 2 ms. Measured, four
    regimes, 15 samples each on a 32-core WSL2 box:

    ===========================  =================  ==================
    regime                       floor p90 spread   reconcile Δ
    ===========================  =================  ==================
    idle (19 runs)               0.349 – 1.154 ms   0.944 – 1.297 ms
    pinned to 1 cpu, no rival    0.47 – 0.61 ms     0.383 – 0.583 ms
    pinned, 1 rival spinner      0.32 – 0.57 ms     0.497 – 0.691 ms
    pinned, 3 rival spinners     3.67 – 6.28 ms     0.961 – 2.960 ms
    96 spinners, 32 cores        34.4 – 63.7 ms     −39.6 – +24.8 ms
    ===========================  =================  ==================

    The two contended regimes are where a false red lives: six of eight
    three-rival runs measured a Δ over the 2 ms budget with no code change at
    all, and the oversubscribed regime produced **negative** deltas — a floor
    slower than the command it is subtracted from, which is proof the arithmetic
    had stopped meaning anything. Both are separated from every quiet regime by
    this statistic with ~1.7x margin on each side, and neither is separated by
    the relative spreads first tried: ``median/min`` overlaps (25% quiet vs 29%
    contended) and even ``p90/min`` overlaps once the whole box is loaded (1.35
    quiet vs 1.42 under 96 spinners), because uniform load scales both operands
    and leaves the ratio flat while the absolute noise passes 30 ms.
    """
    ordered = sorted(floor_samples)
    return ordered[int(0.9 * (len(ordered) - 1))] - ordered[0]


def _budget_gate(*, name: str, observed: float, budget: float, spread: float, breach: str) -> Gate:
    """One ``exec_floor + Δ`` gate, decided against an **admissible** measurement.

    PURE. Three outcomes, and which one applies is settled in this order:

    1. ``observed <= budget`` → **PASS**, whatever the machine was doing.
       Contention inflates a min-of-N and can never deflate one, so a Δ that fits
       the budget on a loaded runner fits it on a quiet one a fortiori. There is
       no contention question to ask on the green side, which is why this branch
       comes first.
    2. over budget, and the floor scattered by **less** than the **margin**
       ``observed - budget`` → **FAIL**. The machine resolved a difference this
       small, and the difference is over budget: that is a regression.
    3. over budget, and the floor scattered by **more** than the margin →
       **INCONCLUSIVE**. No verdict: the runner could not resolve an overshoot
       this small during this run, so the number is a measurement of the runner.
       Reported, annotated, and recorded; never a red, and never silently a
       green either.

    **The margin is the resolvable quantity, not the budget.** An earlier
    revision compared the scatter against ``budget``, which asks whether the
    runner can resolve 2 ms — a question nobody is asking. What is actually
    being decided is whether ``observed`` sits above or below ``budget``, and
    the distance between them is the margin. A run measuring 2.010 ms against a
    2.000 ms budget with 0.692 ms of floor scatter is asserting a 0.010 ms
    difference on an instrument with 69x that much noise; under the old
    comparison it reported a confident FAILED, and it would flap red and green
    on runner noise forever. That is the shape this rule exists to refuse, and
    it was the one shape it let through.

    What keeps rule 3 from becoming "excuse whatever is red" is the *size* of a
    real regression, not the classifier's blindness — a genuine breach clears
    the floor scatter by a wide margin and still reaches rule 2, while only
    breaches down in the noise abstain. The ``--expect-fail-gate`` step in
    ``test/taskfile.yml`` proves exactly that on every invocation: its injected
    delay lands well above any quiet-box scatter, so if this classifier ever
    started abstaining on resolvable overshoots, the injected run would abstain
    too, no gate would go red, and that step fails.
    """
    over = observed > budget
    margin = abs(observed - budget)
    inconclusive = over and spread > margin
    return Gate(
        name=name,
        observed=observed,
        budget=budget,
        passed=not over or inconclusive,
        unit="ms",
        inconclusive=inconclusive,
        note=""
        if not over
        else (
            f"no verdict: the observed {observed:.3f} ms is over budget by {margin:.3f} ms, but the bare-exec "
            f"floor itself scattered {spread:.3f} ms across this run — the overshoot is below this runner's "
            "resolution, so it measures the runner and not the reconciler — re-run on a quiet machine"
        )
        if inconclusive
        else f"{breach} (over by {margin:.3f} ms against {spread:.3f} ms of floor scatter, so the machine "
        "could resolve this)",
    )


def _fixed_point_gates(streams: Mapping[str, float]) -> list[Gate]:
    """The reconciler's fixed point, as a pair (ocx-sh/ocx#342).

    Two gates, not one, and the first is why the second means anything: an arena
    whose project never composed would report zero applies at steady state and
    pass a lone "steady == 0" check while proving nothing at all. Requiring the
    *first* fire to apply makes "converged" and "inert" distinguishable.

    ``streams`` is required. There is no "not measured" branch, because an empty
    gate list is indistinguishable from a passing one under
    ``all(g.passed for g in gates)`` — a genuine unmeasured state would have to
    produce a **failing** gate, not none.
    """
    first = streams.get("first_applies", 0.0)
    steady = streams.get("steady_applies", 0.0)
    return [
        Gate(
            name="first reconcile applies the project's path entries",
            observed=first,
            budget=1.0,
            passed=first >= 1.0,
            unit="lines",
            note=""
            if first >= 1.0
            else (
                "the first fire performed no PATH surgery, so the arena composed nothing — "
                "the steady-state gate below would then pass vacuously"
            ),
        ),
        Gate(
            name="steady-state reconcile applies nothing",
            observed=steady,
            budget=0.0,
            passed=steady == 0.0,
            unit="lines",
            note=""
            if steady == 0.0
            else (
                f"the reconciler has no fixed point: a prompt with nothing changed still emits "
                f"{steady:.0f} PATH apply line(s), which every prompt then pays forever "
                "(ocx-sh/ocx#342)"
            ),
        ),
    ]


def _consent_gates(consent: Mapping[str, float]) -> list[Gate]:
    """The arena's own non-vacuity, as a pair — same shape and same reason as
    :func:`_fixed_point_gates`.

    The wall-clock numbers above describe a per-prompt reconcile. What that
    reconcile *costs* depends on the lock it evaluates consent over, because
    ``project::consent::verified_sources`` loops over the lock's tools and reads
    one directory per tool, unconditionally, before any clause is consulted. So
    two things have to hold before the deltas mean anything about a real project,
    and neither implies the other:

    1. **The lock carries tools at all**, against :data:`WALL_MIN_TOOLS` and not
       against the arena's own sizing knob. A tool-free lock runs the loop body
       zero times — the state this arena shipped in until 2026-08-26 — and no
       per-tool regression, however large, can move a number measured under it.
    2. **Every one of them is corroborated.** ``verified_sources`` ``return``s at
       the first tool whose package directory records no origin, so an arena that
       declares eight tools and corroborates seven measures *one* iteration.
       Clause 2 grants over the whole lock or not at all, which is what makes the
       observation binary and this gate's ``observed`` a count rather than a
       fraction.

    Gate 1 is why gate 2 is not vacuous — over a tool-free lock "everything is
    corroborated" is true and worthless — and gate 2 is why gate 1 is not
    cosmetic. Exactly as with the fixed point, an empty gate list would be
    indistinguishable from a passing one, so there is no "not measured" branch.

    **What stays unmeasured, and it is more than it was.** The timed reconcile
    is granted by clause 3, so it pays neither ``namespace_granted`` — one glob
    match per *source*, and ``source_of`` truncates to ``<registry>/<org>``, so
    that is one entry for a whole fleet project rather than one per tool — nor
    ``verified_sources``, whose N host-leaf derivations and N directory reads
    are gated on a namespaces grant the timed leg does not carry. An earlier
    revision claimed that second one *was* measured; gating the read made the
    claim false, and the gate would have kept passing either way, so it is
    corrected here rather than left to be rediscovered.

    Reaching clause 2 in the *timed* run stays unavailable for the original
    reason: it withholds the project's ``[env]``, which is the PATH surgery the
    fixed-point gates read. So clause 2 is driven here for its evidence rather
    than its clock, and a regression confined to ``verified_sources`` would not
    red this gate. What these gates do still prove is that the arena grows a
    real lock and that clause 2 corroborates all of it — measured at 8 tools
    1.371 ms against 96 tools 4.478 ms, over budget, so tool-count scaling is
    observable here even with the store read off the clock.
    """
    locked = consent.get("locked_tools", 0.0)
    corroborated = consent.get("corroborated_tools", 0.0)
    return [
        Gate(
            name="the timed arena's lock carries tools",
            observed=locked,
            budget=float(WALL_MIN_TOOLS),
            passed=locked >= WALL_MIN_TOOLS,
            unit="tools",
            note=""
            if locked >= WALL_MIN_TOOLS
            else (
                f"the arena locked {locked:.0f} tool(s), under the {WALL_MIN_TOOLS} it takes for the "
                "consent path's per-tool loop to be a loop at all — every delta below is measured on "
                "a lock nobody has"
            ),
        ),
        # Budget is the *observed* lock size, not the constant, so this gate says
        # one thing only — "every tool the lock declares is corroborated" — and
        # leaves "the lock declares enough tools" to its sibling. Two gates, two
        # independent reds; overlapping them would make a tool-free arena red
        # both and the pair indistinguishable from either alone.
        Gate(
            name="consent clause 2 corroborates every locked tool",
            observed=corroborated,
            budget=locked,
            passed=corroborated >= locked,
            unit="tools",
            note=""
            if corroborated >= locked
            else (
                "clause 2 refused this lock, so `verified_sources` returned at the first "
                f"uncorroborated tool: the timed reconcile measured one loop iteration, not {locked:.0f}"
            ),
        ),
    ]


def evaluate(
    *,
    floor_samples: Sequence[float],
    startup_samples: Sequence[float],
    reconcile_samples: Sequence[float],
    cold_reconcile_samples: Sequence[float],
    exec_counts: Mapping[str, int],
    streams: Mapping[str, float],
    consent: Mapping[str, float],
    capability_record: bool,
    budget_ms: float = DELTA_BUDGET_MS,
    reconcile_budget_ms: float = RECONCILE_BUDGET_MS,
) -> LatencyReport:
    """Decide the gate from raw samples.

    PURE — no I/O, no exit, no print.

    Parameters
    ----------
    floor_samples:
        Wall clock (ms) of the bare-exec floor, one entry per process spawn.
    startup_samples:
        Wall clock (ms) of the shell-startup activation, one per spawn.
    reconcile_samples:
        Wall clock (ms) of the per-prompt reconcile, one per spawn. The wall
        arena spawns a fresh process per sample with no ``__OCX_ENV_STATE``
        carrier, so every sample takes the full **applying** path — not the
        stat-only short circuit an established session's second prompt takes.
    cold_reconcile_samples:
        The same command with the host-capability record deleted before each
        spawn. Recorded only.
    exec_counts:
        ``{"quiet": n, "after_touch": n}`` from :func:`measure_exec_counts`.
    streams:
        :func:`measure_reconcile_streams`' mapping. ``ms_per_apply`` is recorded
        only; ``first_applies`` / ``steady_applies`` are gated — they are the
        reconciler's fixed point (ocx-sh/ocx#342).
    consent:
        :func:`measure_clause_two`' mapping. Gated: it is what makes the timed
        reconcile's per-tool consent work non-vacuous, and a wall-clock number
        measured over a lock the store cannot corroborate is a number about a
        one-iteration loop.
    capability_record:
        Whether a host-capability record existed to delete, i.e. whether
        ``cold_reconcile_samples`` measured a genuinely colder path. Recorded
        only; the binary persists one on Linux alone.
    budget_ms:
        C-044's Δ for the shell-startup pass.
    reconcile_budget_ms:
        C-044's Δ for the per-prompt reconcile.

    Returns
    -------
    LatencyReport
        ``passed`` is True iff every gate passed. Recorded-only numbers never
        affect it. A wall-clock gate that reached **no verdict** on a contended
        runner counts as passed and is listed in ``report.inconclusive``; see
        :func:`_budget_gate` for when that happens and why it cannot be reached
        by a run the machine was able to measure.
    """
    floor = min(floor_samples)
    startup = min(startup_samples)
    reconcile = min(reconcile_samples)
    cold = min(cold_reconcile_samples)
    startup_delta = startup - floor
    reconcile_delta = reconcile - floor
    cold_delta = cold - floor
    # The positive control's statistic, and not the one the budgets use. See
    # STARTUP_WORK_FLOOR_MS for the measurements that separate the two.
    startup_work = statistics.median(startup_samples) - statistics.median(floor_samples)
    # The reconcile's counterpart. Recorded, never gated, and its whole content is
    # the identity `work - delta`: that difference is the measured series'
    # median-to-min gap minus the floor's, i.e. how much MORE than the floor the
    # timed command itself scattered. It is the one number that separates a runner
    # that got noisier from one that got uniformly slower, and the startup half was
    # only ever recoverable by subtracting two printed gate rows by hand. The
    # reconcile half was not recorded at all, so answering that question for
    # ocx-sh/ocx#340's CI flap cost a fourteen-run dev-box study. It is one
    # subtraction.
    reconcile_work = statistics.median(reconcile_samples) - statistics.median(floor_samples)
    # Admissibility, computed from the floor alone and shared by both budgets:
    # the same runner produced both deltas, so one scatter figure decides for
    # both. See `_budget_gate` for what it does and cannot do.
    spread = floor_spread_ms(floor_samples)

    quiet = exec_counts["quiet"]
    touched = exec_counts["after_touch"]

    gates = [
        Gate(
            name="no-op prompt execs zero times",
            observed=float(quiet),
            budget=0.0,
            passed=quiet == 0,
            unit="execs",
            note=""
            if quiet == 0
            else f"the shell-side short-circuit did not hold: {quiet} exec(s) across quiet prompts",
        ),
        Gate(
            name="touched watch-set path execs exactly once",
            observed=float(touched),
            budget=1.0,
            passed=touched == 1,
            unit="execs",
            note=""
            if touched == 1
            else (
                f"expected exactly 1 exec after touching a watch-set path, saw {touched} — "
                "a 0 here means the whole exec count is vacuous"
            ),
        ),
        _budget_gate(
            name="shell startup <= exec_floor + delta",
            observed=startup_delta,
            budget=budget_ms,
            spread=spread,
            breach=f"C-044: startup costs floor + {startup_delta:.3f} ms, budget is {budget_ms:.3f} ms",
        ),
        # The budget's positive control. A `ConfigLoader` pass, a project walk
        # and a hook emission cannot be free, so a delta indistinguishable from
        # zero is not a fast path — it is the floor and the measured command
        # doing the same work, which passes the budget for free. The structural
        # pair (`_activation_stream` + `measure_wall_clock`) is the primary
        # guard and *raises*; this is the numeric backstop for the case those
        # cannot see, a floor command that quietly grew expensive.
        Gate(
            name="shell startup does measurably more work than the bare floor",
            observed=startup_work,
            budget=STARTUP_WORK_FLOOR_MS,
            passed=startup_work >= STARTUP_WORK_FLOOR_MS,
            unit="ms",
            note=""
            if startup_work >= STARTUP_WORK_FLOOR_MS
            else (
                f"startup's median is {startup_work:.3f} ms above the floor's, under the "
                f"{STARTUP_WORK_FLOOR_MS:.3f} ms a real ConfigLoader pass costs — the two commands "
                "are measuring the same work"
            ),
        ),
        # C-044's other half, asserted since 2026-08-25 (ocx-sh/ocx#340). It was
        # a `::warning` while the shipped cost was ~16 ms; that cost turned out
        # to be a host-capability directory walk rather than the reconciler, so
        # the spec's original 2 ms stands and the warning is an assert.
        _budget_gate(
            name="per-prompt reconcile <= exec_floor + delta",
            observed=reconcile_delta,
            budget=reconcile_budget_ms,
            spread=spread,
            breach=(
                f"C-044: the per-prompt reconcile costs floor + {reconcile_delta:.3f} ms, "
                f"budget is {reconcile_budget_ms:.3f} ms"
            ),
        ),
        *_fixed_point_gates(streams),
        *_consent_gates(consent),
    ]

    records = {
        "floor_ms": floor,
        # The admissibility evidence, recorded whether or not it was needed: a
        # reader comparing two runs has to be able to see how quiet each machine
        # was, not just what each verdict came out as.
        "floor_spread_ms": spread,
        # Derived from the gates themselves, never recomputed: a recorded field
        # that can disagree with the verdict it describes is worse than no field.
        "measurement_admissible": not any(gate.inconclusive for gate in gates),
        "startup_ms": startup,
        "startup_delta_ms": startup_delta,
        "startup_work_ms": startup_work,
        "reconcile_work_ms": reconcile_work,
        "reconcile": {
            "total_ms": reconcile,
            "delta_ms": reconcile_delta,
            "contract_budget_ms": reconcile_budget_ms,
            "contract_met": reconcile_delta <= reconcile_budget_ms,
            "cold_total_ms": cold,
            "cold_delta_ms": cold_delta,
            "cold_record": capability_record,
        },
        "eval": dict(streams),
        "consent": dict(consent),
        "samples": len(floor_samples),
        "quiet_prompts": quiet,
    }

    return LatencyReport(gates=gates, records=records, passed=all(g.passed for g in gates))


def format_report(report: LatencyReport) -> str:
    """Human-readable table. Mirrors bench/compare.py's ``_format_report``."""
    lines = [f"{'Gate':<56} {'Observed':>12} {'Budget':>10} {'Status':>8}", "-" * 90]
    for gate in report.gates:
        status = "INCONC" if gate.inconclusive else "PASS" if gate.passed else "FAIL"
        lines.append(
            f"{gate.name:<56} "
            f"{gate.observed:>10.3f} {gate.unit:<2} "
            f"{gate.budget:>9.3f} "
            f"{status:>8}"
        )
        if gate.note:
            lines.append(f"    {gate.note}")
    rec = report.records
    cold = rec["reconcile"]
    lines += [
        "",
        "Recorded (not asserted):",
        f"  exec floor (min of {rec['samples']})          {rec['floor_ms']:>9.3f} ms",
        f"  shell startup ConfigLoader pass       {rec['startup_delta_ms']:>9.3f} ms  (C-042)",
        (
            f"  per-prompt applying reconcile delta   {cold['delta_ms']:>9.3f} ms  "
            f"(C-044 budget {cold['contract_budget_ms']:.1f} ms, met={cold['contract_met']})"
        ),
        (
            f"  ... same, median-based                {rec['reconcile_work_ms']:>9.3f} ms  "
            f"(minus the min-based delta above = {rec['reconcile_work_ms'] - cold['delta_ms']:+.3f} ms, "
            f"the scatter the timed command carried and the floor did not)"
        ),
        (
            f"  ... same, cold capability record      {cold['cold_delta_ms']:>9.3f} ms  "
            f"(record present to delete: {cold['cold_record']})"
        ),
        (
            f"  shell-side eval of one apply         {rec['eval']['ms_per_apply']:>9.3f} ms  "
            f"({rec['eval']['stream_bytes']:.0f} bytes over a "
            f"{rec['eval']['path_segments']:.0f}-segment PATH, {rec['eval']['iterations']:.0f} applies)"
        ),
        (
            f"  steady-state stream                  {rec['eval']['steady_stream_bytes']:>9.0f} B   "
            f"({rec['eval']['steady_applies']:.0f} apply lines, ocx-sh/ocx#342)"
        ),
        (
            f"  bare-exec floor scatter (p90-min)    {rec['floor_spread_ms']:>9.3f} ms  "
            f"(below every breach margin, so each wall-clock verdict is admissible: "
            f"{rec['measurement_admissible']})"
        ),
        "",
        f"Overall: {'PASSED' if report.passed else 'FAILED'}"
        + (
            f" — but {len(report.inconclusive)} wall-clock gate(s) reached NO VERDICT on a contended runner"
            if report.inconclusive
            else ""
        ),
    ]
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Measurement
# ---------------------------------------------------------------------------


def _inject_ms() -> float:
    raw = os.environ.get(INJECT_ENV, "")
    return float(raw) if raw.strip() else 0.0


def _sample(cmd: Sequence[str], *, cwd: Path, env: Mapping[str, str]) -> float:
    """One process spawn, wall clock in ms."""
    start = time.perf_counter()
    subprocess.run(list(cmd), cwd=str(cwd), env=dict(env), capture_output=True, check=False)
    return (time.perf_counter() - start) * 1000.0


#: The shipped POSIX shim, as source. ``ENV_SH`` is byte-identical across
#: installs — there is no install-time substitution — so the const **is** the
#: artifact. It is read rather than materialized because materializing it means
#: ``ocx self setup``, whose first phase installs ocx from a registry; a
#: hermetic latency arena cannot run that. A rename or a re-quoting raises out of
#: :func:`_posix_shim_body` instead of falling back to a copy, because a fallback
#: copy is the very thing this replaces.
_SHIM_SOURCE = Path(__file__).resolve().parents[2] / "crates" / "ocx_lib" / "src" / "setup" / "shims.rs"
_SHIM_CONST_OPEN = 'pub const ENV_SH: &str = r#"'
_SHIM_CONST_CLOSE = '"#;'

#: Stands in for ``$_ocx_bin`` while the shim is sourced: records one invocation
#: per blank-line-separated block, one argument per line, and deliberately does
#: **not** hand over to the real binary. The derivation wants the shim's argv,
#: not its output, and a shim that also ran ocx would make this step cost what it
#: is about to measure. Sibling of ``_COUNTER_SHIM``, planted at the same path
#: for the same reason: that absolute path is what the shim invokes (C-045).
_ARGV_RECORDER = """#!/bin/sh
for __ocx_a in "$@"; do printf '%s\\n' "$__ocx_a" >> {out}; done
printf '\\n' >> {out}
exit 0
"""


def _posix_shim_body() -> str:
    """The shipped ``ENV_SH`` body, out of the Rust source that defines it."""
    source = _SHIM_SOURCE.read_text(encoding="utf-8")
    if _SHIM_CONST_OPEN not in source:
        raise RuntimeError(
            f"cannot find {_SHIM_CONST_OPEN!r} in {_SHIM_SOURCE} — the shim const moved or was re-quoted, and "
            "this gate must drive the shipped shim rather than a copy of it"
        )
    body = source.split(_SHIM_CONST_OPEN, 1)[1].split(_SHIM_CONST_CLOSE, 1)[0]
    if "self activate" not in body:
        raise RuntimeError(f"the extracted ENV_SH body issues no `self activate`:\n{body}")
    return body


def startup_command(ocx: Path, *, root: Path) -> list[str]:
    """The argv an interactive login shell runs, **recorded from the shipped shim**.

    Not copied out of ``shims.rs``: the shim body is written to disk, a recorder
    is planted at the absolute ``$OCX_HOME/symlinks/.../bin/ocx`` path the shim
    invokes, and ``bash -i`` sources it. Whatever the shim decides — the shell it
    detects, its ``case "$-" in *i*)`` interactivity answer, every flag it
    forwards — is what comes back, so this gate cannot disagree with the shipped
    shim about what a shell start costs. See "The decision under test" in the
    module docstring for why the interactivity answer specifically matters.

    ``bash -i`` and not a pty: ``$-`` carries ``i`` for ``bash -i`` with no
    terminal on any descriptor, which is also the production condition — every
    shim runs ``self activate`` inside a command substitution with stderr
    redirected, so the binary never sees a tty even in a real login shell. A pty
    would make the arena *less* like production, not more, and would put tty
    write cost inside every timed sample.

    The recorded ``argv[0]`` is the shim's own ``$OCX_HOME`` path — the recorder,
    here — so it is replaced by the binary under test, exactly as
    :func:`_derive_reconcile_command` does. Every flag after it is the shim's.
    """
    bash = shutil.which("bash")
    if bash is None:
        raise RuntimeError("bash is required to record the shim's invocation and is not on PATH")

    ocx_home = root / "ocx"
    hooked = ocx_home / "symlinks" / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"
    hooked.parent.mkdir(parents=True, exist_ok=True)
    recorded = root / "shim-argv"
    recorded.write_bytes(b"")
    hooked.write_text(_ARGV_RECORDER.format(out=_sh_quote(str(recorded))), encoding="utf-8")
    hooked.chmod(0o755)

    shim = ocx_home / "env.sh"
    shim.write_text(_posix_shim_body(), encoding="utf-8")

    session = subprocess.run(
        [bash, "--norc", "--noprofile", "-i", "-c", f". {_sh_quote(str(shim))}"],
        cwd=str(root),
        env=matrix.clean_env(root, bash, ocx_home=ocx_home),
        capture_output=True,
        check=False,
        text=True,
    )
    invocations = [block.splitlines() for block in recorded.read_text(encoding="utf-8").split("\n\n") if block.strip()]
    activations = [argv for argv in invocations if argv[:2] == ["self", "activate"]]
    if len(activations) != 1:
        raise RuntimeError(
            f"expected exactly one `self activate` invocation from the shipped shim, recorded {len(activations)} "
            f"of {len(invocations)}: {invocations}\nbash exited {session.returncode}, stderr:\n{session.stderr}"
        )
    return [str(ocx), *activations[0]]


def _activation_stream(cmd: Sequence[str], *, cwd: Path, env: Mapping[str, str]) -> str:
    """Run the startup command once and prove it is the one under contract.

    The positive half of the structural pair; :func:`measure_wall_clock` owns the
    negative half. Raises rather than returning a failing gate: a measurement
    pointed at the wrong command is a broken harness, and ``--expect-fail`` must
    not be able to read that as "the injection worked".
    """
    result = subprocess.run(list(cmd), cwd=str(cwd), env=dict(env), capture_output=True, check=True, text=True)
    if "__ocx_prompt_hook" not in result.stdout:
        raise RuntimeError(
            f"the startup command emits no prompt hook, so it is not the path C-044 budgets: {list(cmd)}\n"
            f"stdout:\n{result.stdout}"
        )
    return result.stdout


def _derive_reconcile_command(activation: str, ocx: Path) -> list[str]:
    """Extract the per-prompt reconcile invocation from the emitted hook body.

    Derived, never hand-written: the bench and the hook then cannot disagree
    about which command a prompt runs. The emitted bash line is::

        eval "$('<binary>' --offline self activate --reconcile --shell=bash 2>/dev/null)" || true

    ``argv[0]`` is the hook's own ``$OCX_HOME/symlinks/.../bin/ocx`` path, which
    the wall-clock arena does not populate, so it is replaced by the binary under
    test; every flag after it is whatever the hook emitted.
    """
    lines = [line for line in activation.splitlines() if "self activate --reconcile" in line]
    if len(lines) != 1:
        raise RuntimeError(f"expected exactly one reconcile call site in the emitted hook, found {len(lines)}")
    line = lines[0]
    start = line.find("$(")
    end = line.find(" 2>/dev/null", start)
    if start < 0 or end < 0:
        raise RuntimeError(f"cannot read the reconcile invocation out of: {line!r}")
    argv = shlex.split(line[start + 2 : end])
    if len(argv) < 2 or "--reconcile" not in argv:
        raise RuntimeError(f"the parsed reconcile invocation is not one: {argv!r} (from {line!r})")
    return [str(ocx), *argv[1:]]


def measure_wall_clock(
    ocx: Path,
    *,
    cwd: Path,
    env: Mapping[str, str],
    startup: Sequence[str],
    reconcile: Sequence[str],
    samples: int = SAMPLES,
) -> dict[str, list[float]]:
    """Interleaved min-of-N sampling of floor, startup and applying reconcile.

    Interleaved on purpose: a job whose runner slows down halfway through would
    otherwise load the whole drift onto whichever command ran second.

    A fourth series, ``reconcile_cold``, runs the same reconcile command with the
    host-capability record deleted first. It is recorded, never gated — see
    "Cold and warm" in the module docstring for why the *asserted* number is the
    warm one. The cold spawn re-persists the record itself, so it restores the
    warm state for the next iteration wherever it sits in the cycle.

    The warm-up pass below is what makes "warm" a property of this harness rather
    than a coincidence: every command runs once before any sample is taken, which
    populates the page cache, any first-run on-disk layout, and the capability
    record.

    The injected delay reaches the **measured** commands and never the floor: the
    floor must stay honest or the subtraction cancels the injection out and the
    gate reds for no one. Each measured command consumes it at its own seam —
    startup inside ``hook::registration``, reconcile inside ``hook::checkpoint``
    — so a command that emits neither gets no delay and ``--expect-fail`` fails
    rather than passing on a measurement of something else.
    """
    inject = _inject_ms()
    commands = {
        "floor": [str(ocx), "version"],
        "startup": list(startup),
        "reconcile": list(reconcile),
        "reconcile_cold": list(reconcile),
    }
    envs = {name: dict(env) for name in commands}
    if inject:
        for name in commands:
            if name != "floor":
                envs[name][INJECT_ENV] = str(inject)

    record = Path(env["OCX_HOME"]) / CAPABILITY_RECORD
    warm = {
        name: subprocess.run(cmd, cwd=str(cwd), env=envs[name], capture_output=True, check=False, text=True)
        for name, cmd in commands.items()
    }
    # The floor's other half: it has to be a command that does *not* do the
    # measured work, or `startup - floor` cancels the very thing it reports.
    # `_activation_stream` pins the positive side of the same pair.
    if "__ocx_prompt_hook" in warm["floor"].stdout:
        raise RuntimeError(
            f"the floor command emits an activation stream, so the delta cancels it: {commands['floor']}"
        )

    out: dict[str, list[float]] = {name: [] for name in commands}
    for _ in range(samples):
        for name, cmd in commands.items():
            if name == "reconcile_cold":
                record.unlink(missing_ok=True)
            out[name].append(_sample(cmd, cwd=cwd, env=envs[name]))
    return out


_COUNTER_SHIM = """#!/bin/sh
# Generated by test/bench/shell_latency.py. Counts real execs of the ocx binary
# by appending one byte per invocation, then hands over. Counting by file append
# rather than by inspecting a process list is deliberate: a `pgrep`-style
# detector run from a shell whose own command line carries the search term
# matches itself in every state and so answers the same in every state.
printf 'x' >> {counter}
exec {real} "$@"
"""


def measure_exec_counts(
    ocx: Path, *, root: Path, env: Mapping[str, str], startup: Sequence[str]
) -> dict[str, int]:
    """Drive the real emitted bash hook and count binary execs.

    The hook embeds the absolute path of the binary under
    ``$OCX_HOME/symlinks/.../bin/ocx`` (C-045 — no emitted snippet calls bare
    ``ocx``), so a counting shim placed at exactly that path intercepts every
    exec the hook performs, and only those.

    ``startup`` is the shim-recorded activation (:func:`startup_command`), used
    here for the same reason the wall clock uses it: registering the hook through
    the production rung-5 decision rather than through a ``--hook`` override is
    what makes this arena's "no prompt hook in the emitted stream" raise a check
    on the shipped path instead of on an override no shell passes.

    Returns ``{"quiet": n, "after_touch": n}``:

    * ``quiet`` — execs across :data:`QUIET_PROMPTS` prompts with nothing
      changed. Must be 0.
    * ``after_touch`` — execs across one prompt after a watch-set path is
      touched. Must be 1, which is what keeps ``quiet == 0`` from being the
      answer a dead harness also gives.
    """
    bash = shutil.which("bash")
    if bash is None:
        raise RuntimeError("bash is required for the zero-exec gate and is not on PATH")

    ocx_home = Path(env["OCX_HOME"])
    hooked = ocx_home / "symlinks" / "ocx.sh" / "ocx" / "cli" / "current" / "content" / "bin" / "ocx"
    hooked.parent.mkdir(parents=True, exist_ok=True)
    counter = root / "exec-count"
    counter.write_bytes(b"")
    hooked.write_text(
        _COUNTER_SHIM.format(counter=_sh_quote(str(counter)), real=_sh_quote(str(ocx))),
        encoding="utf-8",
    )
    hooked.chmod(0o755)

    # A watch-set path the hook compares against its stamp. `ocx.lock` under
    # OCX_HOME is in the emitted watch set; assert that rather than assume it,
    # so a watch-set change turns this gate red instead of silently making the
    # touch phase a no-op.
    watched = ocx_home / "ocx.lock"
    watched.parent.mkdir(parents=True, exist_ok=True)
    watched.write_text("", encoding="utf-8")

    activation = subprocess.run(
        list(startup),
        cwd=str(root),
        env=dict(env),
        capture_output=True,
        check=True,
        text=True,
    ).stdout
    # Hook presence first, watch set second, and the order carries the
    # diagnosis: no hook means the rung-5 interactivity decision did not reach
    # `Hook::enabled`, and *every* downstream property — the watch set, the exec
    # counts — is then absent for that one reason. Checking the watch set first
    # reports the symptom furthest from the cause.
    if "__ocx_prompt_hook" not in activation:
        raise RuntimeError(
            "the shim's own activation emits no prompt hook, so the shipped interactivity decision "
            f"(`Interactive::resolve` -> `Hook::enabled`, rung 5) did not reach it: {list(startup)}\n"
            f"emitted stream:\n{activation}"
        )
    if _sh_quote(str(watched)) not in activation:
        raise RuntimeError(
            f"{watched} is not in the emitted watch set; the touch phase would prove nothing.\n"
            f"emitted hook:\n{activation}"
        )

    script = root / "session.sh"
    script.write_text(
        "\n".join(
            [
                "set -u",
                'eval "$(' + " ".join(_sh_quote(part) for part in startup) + ')"',
                # Converge: the first prompt reconciles by construction (the
                # carrier's pwd is unset), which is C-044's own first-prompt
                # boundary and not part of the no-op measurement.
                "__ocx_prompt_hook",
                f"printf '' > {_sh_quote(str(counter))}",
                f"for _ in $(seq 1 {QUIET_PROMPTS}); do __ocx_prompt_hook; done",
                f"wc -c < {_sh_quote(str(counter))} | tr -d ' \\n' > {_sh_quote(str(root / 'quiet'))}",
                f"printf '' > {_sh_quote(str(counter))}",
                # A watch-set path newer than the stamp must break the
                # short-circuit. `touch` alone can land inside the stamp's
                # mtime granularity, so push it forward.
                #
                # `-t CCYYMMDDhhmm.SS`, not `-d '+1 hour'`: the relative form is
                # a GNU extension, and BSD `touch -d` wants an ISO-8601 instant,
                # so on macOS the old spelling failed into a bare `touch` —
                # mtime = now, landing in the stamp's own second. That is the
                # same-tick ceiling EC-FP-001/EC-FP-002 accept by design and
                # forbid an acceptance test from assuming away, and macOS
                # /bin/bash is 3.2, whose `-nt` compares whole seconds. The arm
                # therefore measured the ceiling instead of the change and read
                # 0 execs. `-t` is POSIX and identical on both.
                #
                # No fallback: a touch that cannot push the mtime forward must
                # fail the session loudly rather than quietly weaken the gate.
                f"touch -t {time.strftime('%Y%m%d%H%M.%S', time.localtime(time.time() + 3600))} "
                f"{_sh_quote(str(watched))} || exit 1",
                "__ocx_prompt_hook",
                f"wc -c < {_sh_quote(str(counter))} | tr -d ' \\n' > {_sh_quote(str(root / 'touched'))}",
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    session = subprocess.run(
        [bash, "--norc", "--noprofile", str(script)],
        cwd=str(root),
        env=dict(env),
        capture_output=True,
        check=False,
        text=True,
    )
    if session.returncode != 0:
        raise RuntimeError(
            f"hook session exited {session.returncode}\nstdout:\n{session.stdout}\nstderr:\n{session.stderr}"
        )
    return {
        "quiet": int((root / "quiet").read_text(encoding="utf-8")),
        "after_touch": int((root / "touched").read_text(encoding="utf-8")),
    }


# ponytail: `str.format` and not an f-string, matching nothing else in this file
# on purpose. `measure_exec_counts` builds its script from joined f-strings, and
# converting this one would buy nothing: BOTH forms double every `{` and `}` a
# shell parameter expansion needs, so the `{{ }}` noise below is inherent to
# interpolating bash from Python, not to the idiom. A 33-line program is also
# more legible as one block than as a list of quoted lines.
_EVAL_SESSION = """set -u
eval "$({startup})"
# Two streams, and the pair is the gate. The FIRST fire applies the project's
# entries; the steady-state fire that follows it must apply nothing at all
# (ocx-sh/ocx#342 — the reconciler's fixed point). Capturing only the second one
# cannot tell "converged" from "this arena never composed anything", which is
# exactly how the old 95-byte figure passed for a project that was inert.
first="$({reconcile} 2>/dev/null)"
eval "$first" || true
printf '%s' "$first" > {first_out}
steady="$({reconcile} 2>/dev/null)"
printf '%s' "$steady" > {steady_out}
# The apply's cost scales with (emitted entries x PATH segments), so the second
# operand is recorded next to the first: 0.02 ms over a 5-segment PATH says
# nothing about a developer's 46-segment one.
__ifs=$IFS; IFS=:; set -- $PATH; IFS=$__ifs; printf '%s' "$#" > {segments_out}
if [ -z "${{EPOCHREALTIME:-}}" ]; then
  # bash < 5 (macOS /bin/bash is 3.2) has no sub-second clock builtin. Record
  # the sentinel rather than a wrong number.
  printf '%s' -1 > {micros_out}
  exit 0
fi
# Time the APPLYING stream: post-fix the steady one carries no path surgery at
# all, so timing it would measure the absence and report a flattering zero.
#
# `//[!0-9]/` and not `/./`: bash renders EPOCHREALTIME with the LC_NUMERIC
# radix character, so under a comma locale removing a literal `.` removes
# nothing, `$(( ))` then evaluates `12345,678` as a comma operator, and
# ms_per_apply is silently wrong. Stripping every non-digit is radix-agnostic,
# which beats pinning LC_ALL: it needs no locale to exist to be correct.
__start=${{EPOCHREALTIME//[!0-9]/}}
__i=0
while [ "$__i" -lt {iterations} ]; do
  eval "$first"
  __i=$((__i + 1))
done
__end=${{EPOCHREALTIME//[!0-9]/}}
printf '%s' "$(( (__end - __start) / {iterations} ))" > {micros_out}
"""

#: The marker every path-kind apply line carries in the bash/zsh arm
#: (``crates/ocx_lib/src/shell.rs`` — ``__ocx_p='<dir>'; PATH=…``). Its presence
#: in an emitted stream is what "this prompt performs PATH surgery" looks like
#: from outside the binary. As a plain substring it also matches the POSIX
#: list arm (ash/ksh/dash, same ``__ocx_p='<dir>'`` prologue) and PowerShell's
#: ``$__ocx_p='<dir>'`` — once per apply line in each, since the trailing
#: ``unset``/``Remove-Variable`` carries no ``=``. That breadth is harmless
#: here: this arena only ever runs ``--shell=bash``.
PATH_APPLY_MARKER = "__ocx_p="


def measure_reconcile_streams(
    *,
    root: Path,
    cwd: Path,
    env: Mapping[str, str],
    startup: Sequence[str],
    reconcile: Sequence[str],
    iterations: int = EVAL_ITERATIONS,
) -> dict[str, float]:
    """Read the reconciler's two emitted streams, and time the shell applying one.

    The **gated** half is the pair of streams (ocx-sh/ocx#342): the first fire
    must apply the project's path entries (the arena is composing something) and
    the steady-state fire after it must apply none (the reconciler has a fixed
    point). Both come out of a single shell session, which is why they are read
    here and not in two functions — a second session's "first" fire would be a
    steady-state one.

    The **recorded** half is ``ms_per_apply``. Every other measurement in this
    file spawns ocx and times the process; this one times what the shell does
    with ocx's output — the ``PATH`` string surgery the emitted lines perform —
    which is real per-prompt latency no spawn measurement can observe. It is
    measured over the *applying* stream, since the steady one has no surgery left
    to time, and it is ``-1`` where the shell has no sub-second clock (bash < 5).
    """
    bash = shutil.which("bash")
    if bash is None:
        raise RuntimeError("bash is required for the eval-cost measurement and is not on PATH")

    micros_out = root / "eval-micros"
    first_out = root / "eval-first-stream"
    steady_out = root / "eval-steady-stream"
    segments_out = root / "eval-path-segments"
    script = root / "eval-session.sh"
    script.write_text(
        _EVAL_SESSION.format(
            startup=" ".join(_sh_quote(part) for part in startup),
            reconcile=" ".join(_sh_quote(part) for part in reconcile),
            first_out=_sh_quote(str(first_out)),
            steady_out=_sh_quote(str(steady_out)),
            segments_out=_sh_quote(str(segments_out)),
            micros_out=_sh_quote(str(micros_out)),
            iterations=iterations,
        ),
        encoding="utf-8",
    )
    session = subprocess.run(
        [bash, "--norc", "--noprofile", str(script)],
        cwd=str(cwd),
        env=dict(env),
        capture_output=True,
        check=False,
        text=True,
    )
    if session.returncode != 0:
        raise RuntimeError(
            f"eval-cost session exited {session.returncode}\nstdout:\n{session.stdout}\nstderr:\n{session.stderr}"
        )
    micros = int(micros_out.read_text(encoding="utf-8"))
    first = first_out.read_text(encoding="utf-8")
    steady = steady_out.read_text(encoding="utf-8")
    if not first.strip():
        raise RuntimeError(
            "the first reconcile emitted an empty stream: the arena is inert, so neither the "
            "fixed-point gate nor the eval timing would prove anything"
        )
    return {
        "ms_per_apply": -1.0 if micros < 0 else micros / 1000.0,
        "stream_bytes": float(len(first)),
        "steady_stream_bytes": float(len(steady)),
        "first_applies": float(first.count(PATH_APPLY_MARKER)),
        "steady_applies": float(steady.count(PATH_APPLY_MARKER)),
        "path_segments": float(segments_out.read_text(encoding="utf-8")),
        "iterations": float(iterations),
    }


#: Path entries the wall arena's project contributes. Six, because that is what
#: ocx-sh/ocx#342 measured the re-emission cost over, and because one entry
#: cannot tell a per-key gate from a per-plan one.
WALL_PROJECT_ENTRIES = 6

#: Locked tools the wall arena's project carries, each with a distinct digest and
#: a corroborating pull-origin marker in the arena's store.
#:
#: **Not decoration.** The consent predicate the per-prompt reconcile runs is
#: quantified over the lock's tools: `project::consent::verified_sources` loops
#: `for tool in &lock.tools`, and for each one derives the host leaf, pins it and
#: reads that package's `refs/origins/` directory. It is computed
#: *unconditionally*, before `evaluate_with_stamp` is even called, so it is on
#: the measured path under every grant. With a tool-free lock the loop body ran
#: **zero** times, and this gate could not see a regression whose cost scales
#: with locked-tool count at all — the arena reported 0.5-1.3 ms deltas that were
#: true of no real project. That was the shape of this file for its whole life
#: before 2026-08-26.
#:
#: One tool would not have fixed it either: `verified_sources` `return`s at the
#: **first** tool the store cannot corroborate, so an arena with tools but no
#: origin markers measures exactly one iteration however many it declares. Every
#: tool here therefore gets a marker, and the clause-2 gate below is what proves
#: they all landed.
#:
#: Eight, sized against the two things it has to sit between: it must be a
#: plausible project (the ocx repo's own `ocx.toml` locks seven), and the whole
#: per-tool cost must stay well inside the 2 ms budget on a healthy tree so the
#: gate does not red on its own arena. Measured at 8 tools the reconcile Δ is
#: 1.1-1.5 ms against the same 1.0-1.3 ms the tool-free arena reported, i.e. the
#: per-tool consent cost is ~25 µs and eight of them are ~0.2 ms.
WALL_PROJECT_TOOLS = 8

#: The floor :func:`_consent_gates` holds the arena to — deliberately **not**
#: :data:`WALL_PROJECT_TOOLS`.
#:
#: A gate whose budget is the same knob it measures passes for every value of
#: that knob, zero included: it measures itself, and the pre-fix arena would
#: satisfy it just by setting the count to what the pre-fix arena had. So the
#: count above is a *sizing* choice and this is the *property*, and only this one
#: is asserted against.
#:
#: Two, because one cannot tell a loop from a single call: the regression class
#: under test is "cost scales with locked-tool count", and a one-tool arena has
#: no scale to observe. The corroboration gate covers the rest — with one tool it
#: cannot distinguish "iterated" from "bailed at the first".
WALL_MIN_TOOLS = 2

#: The consent source every arena tool claims and every marker corroborates.
#: One namespace for all of them, because `source_of` truncates to
#: `<registry>/<org>` — so the source *set* stays at one entry however many tools
#: there are, which is what a real fleet project looks like.
WALL_TOOL_NAMESPACE = "ocx.sh/acme"

#: The one externally observable spelling of `Decision::Activate(Grant::Namespace)`.
#:
#: Emitted by `command/self_group/activate.rs` when consent was granted by
#: **clause 2** and the project's own `[env]` is therefore withheld — a namespaces
#: grant authorizes the package channel only. No other decision reaches it: a
#: `paths` or stamp grant applies the `[env]`, and an inert project emits the
#: "is not activated" hint instead. So observing it proves the whole clause-2
#: chain end to end — every locked tool corroborated by a recorded origin,
#: `verified_sources` returning `Some`, and `namespace_granted` matching — which
#: is exactly what the timed arena needs to be true of its own lock.
NAMESPACE_GRANT_MARKER = "a namespaces grant covers packages only"


def _write_wall_project(project: Path, *, env: Mapping[str, str], ocx: Path) -> None:
    """Fill ``project`` with the project the timed reconcile composes for.

    The directory is the **caller's**, not this function's: ``run_gate`` needs
    the same path for the ``OCX_CONSENT_PATHS`` grant and for ``cwd=`` before
    this runs, and a second, independent spelling of it here is how the grant
    ends up pointing at a directory the arena no longer uses — leaving every
    number measured on an inert reconcile while the gate stays green.

    Built from the acceptance matrix's own fixture helpers rather than
    hand-written TOML, so a change to the project shape moves this arena with it
    instead of leaving it silently inert.

    Three things go in, and the third is the one this file spent its life
    without:

    * ``[env]`` — :data:`WALL_PROJECT_ENTRIES` path entries, the surgery the
      fixed-point gates read.
    * a lock **from ``ocx lock``**, never hand-written: the composer refuses a
      lock whose ``declaration_hash`` does not match the ``ocx.toml`` beside it,
      and a mismatched hash degrades the reconcile to "emitting nothing" with the
      arena still looking correct.
    * :data:`WALL_PROJECT_TOOLS` **locked tools**, spliced into that lock with its
      generated ``declaration_hash`` carried across verbatim
      (:func:`shell_matrix.declaration_hash_of`), each with a corroborating
      origin marker. An offline ``ocx lock`` cannot resolve a tool — that is the
      whole reason the tools are not declared in the ``ocx.toml`` — but consent
      reads the lock for its **source set**, not for anything it has to fetch, so
      the source set is exactly what a fixture can supply. This is what makes the
      measured ``verified_sources`` loop iterate at all.

    Nothing here needs a registry, a package or the network. What it needs is the
    store's *record*, and that record is a plain file.
    """
    entries = "\n".join(
        f'P{index} = {{ type = "path", value = "bin{index}" }}' for index in range(1, WALL_PROJECT_ENTRIES + 1)
    )
    matrix.write_project(project, entries)
    locked = matrix.run_lock(ocx, project, dict(env))
    if locked.returncode != 0:
        raise RuntimeError(f"`ocx lock` failed in the wall arena ({locked.returncode})\n{locked.stderr}")
    declaration = matrix.declaration_hash_of(project / "ocx.lock")

    ocx_home = Path(env["OCX_HOME"])
    registry, _, org = WALL_TOOL_NAMESPACE.partition("/")
    blocks = []
    for index in range(1, WALL_PROJECT_TOOLS + 1):
        # A distinct digest per tool, so each lands in its own CAS shard and each
        # costs its own `read_dir` — one shared digest would collapse N tools
        # into one directory read and hide the scaling this arena exists to
        # expose.
        digest = "sha256:" + hashlib.sha256(f"ocx-latency-tool-{index}".encode()).hexdigest()
        repository = f"{WALL_TOOL_NAMESPACE}/t{index}"
        blocks.append(matrix.lock_tool(f"t{index}", repository, digest=digest))
        matrix.record_origin(ocx_home, registry=registry, digest=digest, origin=f"{registry}/{org}/t{index}")
    matrix.write_lock(project, "\n".join(blocks), declaration_hash=declaration)


def measure_clause_two(
    *,
    cwd: Path,
    env: Mapping[str, str],
    reconcile: Sequence[str],
) -> dict[str, float]:
    """Drive the arena's lock through consent **clause 2** and count what it corroborated.

    The timed reconcile is granted by clause 3 (``OCX_CONSENT_PATHS``), and it has
    to be: clause 2 authorizes the *package* channel alone, so under it the
    project's own ``[env]`` is withheld and the fixed-point gates would have no
    PATH surgery to read. That is a property of the contract, not of the arena.

    What the timed run therefore cannot show is that its own lock is *evidence-
    backed* — and without that the whole point of adding tools is lost, because
    ``verified_sources`` ``return``s at the first tool the store cannot
    corroborate. An arena with eight locked tools and one bad marker measures one
    iteration and looks identical from the outside.

    So this runs the **same derived reconcile command**, in the same arena, with
    the paths grant swapped for a namespaces grant, and reads back the one
    message only ``Decision::Activate(Grant::Namespace)`` can emit
    (:data:`NAMESPACE_GRANT_MARKER`). Seeing it proves, for this exact lock and
    this exact store: every tool resolved a host leaf, every leaf pinned, every
    package directory carried a recorded origin, and every recorded source
    matched the namespace. Which is to say the loop the timed run measures
    iterates :data:`WALL_PROJECT_TOOLS` times rather than one.

    ``locked_tools`` is counted from the lock **on disk**, not from the constant,
    so a splice that silently wrote fewer tools than it meant to reds rather than
    passing against its own intention.

    Returns ``{"locked_tools": n, "corroborated_tools": n or 0}``. The second is
    all-or-nothing by construction — clause 2 grants over the whole lock or not
    at all — which is why it is the tool count and not a fraction.
    """
    child = {key: value for key, value in env.items() if key != "OCX_CONSENT_PATHS"}
    child["OCX_CONSENT_NAMESPACES"] = WALL_TOOL_NAMESPACE
    result = subprocess.run(
        list(reconcile),
        cwd=str(cwd),
        env=child,
        capture_output=True,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"the clause-2 probe exited {result.returncode}; the hook path exits 0 in every state "
            f"(C-051), so this is not an inert arena but a broken one\nstderr:\n{result.stderr}"
        )
    locked_tools = float((cwd / "ocx.lock").read_text(encoding="utf-8").count("[[tool]]"))
    granted = NAMESPACE_GRANT_MARKER in result.stdout
    return {"locked_tools": locked_tools, "corroborated_tools": locked_tools if granted else 0.0}


def _sh_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


def _runner_class() -> str:
    """Best-effort identification of the machine, for the artifact's trend line."""
    cpus = os.cpu_count() or 0
    label = os.environ.get("RUNNER_NAME") or os.environ.get("HOSTNAME") or "local"
    return f"{label} ({cpus} cpu)"


def run_gate(ocx: Path, *, samples: int = SAMPLES) -> tuple[LatencyReport, dict[str, Any]]:
    """Build a hermetic arena, measure everything, evaluate. Returns (report, artifact)."""
    shell = shutil.which("bash") or "/bin/sh"
    # Two arenas on purpose: the exec-count arena has a counting shim standing
    # in for the binary and a hand-written ocx.lock, both of which would move
    # the numbers the wall-clock arena is supposed to report.
    with tempfile.TemporaryDirectory(prefix="ocx-latency-exec-") as raw_exec:
        exec_root = Path(raw_exec)
        # Recorded once, from the shipped shim, and driven by both arenas. Its
        # own sub-root keeps the recorder out of the arena OCX_HOME that the
        # counting shim owns — one fake binary per `$_ocx_bin` path.
        shim_root = exec_root / "shim"
        shim_root.mkdir()
        startup = startup_command(ocx, root=shim_root)
        exec_counts = measure_exec_counts(
            ocx,
            root=exec_root,
            env=matrix.clean_env(exec_root, shell, ocx_home=exec_root / "ocx"),
            startup=startup,
        )
    with tempfile.TemporaryDirectory(prefix="ocx-latency-wall-") as raw_wall:
        wall_root = Path(raw_wall)
        # The grant is the whole point of the arena: an unconsented project is
        # inert, so every number below would be measured on a reconcile that
        # composes nothing — which is what made the shell-side eval figure a
        # 95-byte no-op before ocx-sh/ocx#342 was gated here. One `project`
        # binding feeds the grant, the arena builder and `cwd=` alike.
        project = wall_root / "project"
        env = matrix.clean_env(
            wall_root,
            shell,
            ocx_home=wall_root / "ocx",
            OCX_CONSENT_PATHS=str(project.resolve()),
        )
        _write_wall_project(project, env=env, ocx=ocx)
        # One activation run decides both what gets timed and whether it is the
        # right thing: the reconcile command is read back out of this stream,
        # and a stream without a prompt hook raises before anything is measured.
        # `startup` is the shim's own recorded argv, so that raise fires on the
        # production interactivity decision (module docstring, "The decision
        # under test") and not on a `--hook` override no shell passes.
        activation = _activation_stream(startup, cwd=project, env=env)
        reconcile = _derive_reconcile_command(activation, ocx)
        wall = measure_wall_clock(
            ocx,
            cwd=project,
            env=env,
            startup=startup,
            reconcile=reconcile,
            samples=samples,
        )
        # Read inside the arena's lifetime, and after sampling: the cold leg
        # deletes this file and the spawn that follows re-persists it, so its
        # presence here is the evidence that the cold samples were cold.
        capability_record = (Path(env["OCX_HOME"]) / CAPABILITY_RECORD).exists()
        streams = measure_reconcile_streams(root=wall_root, cwd=project, env=env, startup=startup, reconcile=reconcile)
        # Last, and inside the arena: it swaps the grant, so it must not run
        # before anything that is timed or read under the paths grant.
        consent = measure_clause_two(cwd=project, env=env, reconcile=reconcile)

    report = evaluate(
        floor_samples=wall["floor"],
        startup_samples=wall["startup"],
        reconcile_samples=wall["reconcile"],
        cold_reconcile_samples=wall["reconcile_cold"],
        exec_counts=exec_counts,
        streams=streams,
        consent=consent,
        capability_record=capability_record,
    )
    artifact = {
        "schema_version": 1,
        "contract": "C-044",
        "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": f"{platform.system()}/{platform.machine()}",
        "runner_class": _runner_class(),
        "ocx_binary": str(ocx),
        "ocx_version": _ocx_version(ocx),
        "injected_delay_ms": _inject_ms(),
        "delta_budget_ms": DELTA_BUDGET_MS,
        "reconcile_budget_ms": RECONCILE_BUDGET_MS,
        # The exact argv behind every number, so a reader never has to trust
        # that the gate measured what its prose says it measured.
        "commands": {"floor": [str(ocx), "version"], "startup": startup, "reconcile": reconcile},
        "gates": [asdict(g) for g in report.gates],
        "passed": report.passed,
        **report.records,
    }
    return report, artifact


def _ocx_version(ocx: Path) -> str:
    result = subprocess.run([str(ocx), "version"], capture_output=True, check=False, text=True)
    return result.stdout.strip() or "unknown"


def _summarize(artifact: dict[str, Any]) -> None:
    """Emit the GitHub job summary + annotations. CI-only glue, no assertions."""
    reconcile = artifact["reconcile"]
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not summary_path:
        return
    with open(summary_path, "a", encoding="utf-8") as handle:
        handle.write(
            "\n".join(
                [
                    "### Shell latency gate (C-044)",
                    "",
                    f"- platform: `{artifact['platform']}` on `{artifact['runner_class']}`",
                    f"- exec floor: **{artifact['floor_ms']:.3f} ms** (min of {artifact['samples']})",
                    (
                        f"- shell startup Δ: **{artifact['startup_delta_ms']:.3f} ms** "
                        f"(budget {artifact['delta_budget_ms']:.1f} ms)"
                    ),
                    (
                        f"- per-prompt reconcile Δ: **{reconcile['delta_ms']:.3f} ms** "
                        f"(C-044 budget {reconcile['contract_budget_ms']:.1f} ms, asserted)"
                    ),
                    (
                        f"- ... cold capability record: **{reconcile['cold_delta_ms']:.3f} ms** "
                        f"(recorded; record present to delete: {reconcile['cold_record']})"
                    ),
                    (
                        f"- steady-state apply lines: **{artifact['eval']['steady_applies']:.0f}** "
                        f"(fixed point, must be 0)"
                    ),
                    (
                        f"- bare-exec floor scatter: **{artifact['floor_spread_ms']:.3f} ms** "
                        f"(wall-clock verdict admissible: {artifact['measurement_admissible']})"
                    ),
                    f"- no-op prompt execs: **{artifact['quiet_prompts']}**",
                    f"- injected delay: **{artifact['injected_delay_ms']:.1f} ms**",
                    "",
                ]
            )
        )


# ---------------------------------------------------------------------------
# Self-check — the one runnable check for the pure functions, both colours on
# inputs nobody has to schedule.
# ---------------------------------------------------------------------------

_GREEN = {
    "floor_samples": [3.0, 3.4, 3.2],
    "startup_samples": [3.9, 4.1, 4.4],
    "reconcile_samples": [4.5, 4.9, 5.0],
    "cold_reconcile_samples": [6.5, 6.9, 7.0],
    "exec_counts": {"quiet": 0, "after_touch": 1},
    # Every key `measure_reconcile_streams` returns, not just the gated two: a
    # failing case renders `format_report` into its own assertion message, and a
    # fixture missing a recorded-only key turns that diagnostic into a KeyError —
    # a red for the wrong reason, which is a red nobody can read.
    "streams": {
        "ms_per_apply": 1.3,
        "stream_bytes": 3500.0,
        "steady_stream_bytes": 1525.0,
        "first_applies": 6.0,
        "steady_applies": 0.0,
        "path_segments": 7.0,
        "iterations": float(EVAL_ITERATIONS),
    },
    "consent": {
        "locked_tools": float(WALL_PROJECT_TOOLS),
        "corroborated_tools": float(WALL_PROJECT_TOOLS),
    },
    "capability_record": True,
}

#: A floor series from a contended runner: p90 5.5 ms against a 3.0 ms min, so
#: :func:`floor_spread_ms` reports 2.5 ms — wider than either 2 ms budget. Shaped
#: after the measured `pinned, 3 rival spinners` regime, where the bare-exec
#: floor scattered 3.67-6.28 ms and six of eight runs breached the budget with no
#: code change at all.
_CONTENDED_FLOOR = [3.0, 5.5, 9.0]

#: The startup series that goes with it. Shifted by the same contention, so the
#: same-work control (`median(startup) - median(floor)`) still clears its floor:
#: without this, every contended case would red on the control instead of
#: exercising the abstention under test.
_CONTENDED_STARTUP = [3.9, 6.5, 9.4]


def self_check() -> None:
    """Assert the pure functions flip on every gate and every needle they own."""
    evaluator: list[bool] = []
    reports: list[LatencyReport] = []

    def case(
        *, expect_pass: bool, why: str, red_gate: str = "", abstains: str = "", **overrides: Any
    ) -> LatencyReport:
        """One evaluator case, asserted red or green — and red for the right reason.

        ``red_gate`` names the gate a red case must fail on. Without it a case
        that reds on some *other* gate reads as a pass, which is how a mutation
        gets absorbed by a neighbour's assert and the case under test is never
        exercised at all.

        ``abstains`` names the gate a case expects to reach **no verdict**, and
        every case that does not name one asserts there were none. That default
        is the load-bearing half: an abstention carries ``passed=True``, so a
        classifier that started abstaining on everything would turn every red
        case here green while the assert on ``report.passed`` still read as
        satisfied.
        """
        report = evaluate(**{**_GREEN, **overrides})  # type: ignore[arg-type]
        reports.append(report)
        assert report.passed is expect_pass, f"{why}\n{format_report(report)}"
        reds = [gate.name for gate in report.gates if not gate.passed]
        if red_gate:
            assert reds == [red_gate], f"{why}: expected only {red_gate!r} red, got {reds}"
        abstained = [gate.name for gate in report.inconclusive]
        assert abstained == ([abstains] if abstains else []), (
            f"{why}: expected abstentions {[abstains] if abstains else []}, got {abstained}\n{format_report(report)}"
        )
        evaluator.append(expect_pass)
        return report

    green = case(expect_pass=True, why="the baseline inputs must pass every gate")
    assert green.records["reconcile"]["contract_met"]
    # `reconcile_work_ms` is recorded, so no gate reds when it is wired to the
    # wrong sample list — it would just print a plausible number forever. Its
    # whole content is the identity below, so that is what gets pinned: the
    # excess of the median-based figure over the min-based one is the reconcile
    # series' median-to-min gap minus the floor's.
    excess = green.records["reconcile_work_ms"] - green.records["reconcile"]["delta_ms"]
    want = (statistics.median(_GREEN["reconcile_samples"]) - min(_GREEN["reconcile_samples"])) - (
        statistics.median(_GREEN["floor_samples"]) - min(_GREEN["floor_samples"])
    )
    assert abs(excess - want) < 1e-9, (
        f"reconcile_work_ms must encode the reconcile series' median-gap excess over the floor's: "
        f"got {excess:.6f} ms, want {want:.6f} ms"
    )

    # The overshoot has to clear `_GREEN`'s own 0.2 ms floor scatter, or the
    # case abstains and never reaches the red it is asserting. It read +0.001
    # until 2026-08-26, which passed only because the classifier was then
    # comparing the scatter against the budget instead of against the overshoot;
    # the fixture carried the same defect as the code it was checking.
    over_budget = case(
        expect_pass=False,
        why="startup over budget must fail",
        red_gate="shell startup <= exec_floor + delta",
        startup_samples=[3.0 + DELTA_BUDGET_MS + 0.5] * 3,
    )

    case(
        expect_pass=False,
        why="an exec on a quiet prompt must fail",
        red_gate="no-op prompt execs zero times",
        exec_counts={"quiet": 1, "after_touch": 1},
    )

    case(
        expect_pass=False,
        why="zero execs after a touched watch-set path must fail — otherwise quiet==0 is vacuous",
        red_gate="touched watch-set path execs exactly once",
        exec_counts={"quiet": 0, "after_touch": 0},
    )

    # C-044's reconcile half, asserted since ocx-sh/ocx#340 and back at the
    # spec's original 2 ms since the ~16 ms that forced the amendment was traced
    # to the host-capability walk and removed.
    slow_reconcile = case(
        expect_pass=False,
        why="a reconcile over budget must fail, not warn",
        red_gate="per-prompt reconcile <= exec_floor + delta",
        reconcile_samples=[3.0 + RECONCILE_BUDGET_MS + 0.5] * 3,  # clears the 0.2 ms scatter — see above
    )
    assert not slow_reconcile.records["reconcile"]["contract_met"]

    # The budget's positive control: a startup indistinguishable from the floor
    # satisfies `delta <= budget` for free, and that is what a gate pointed at
    # the wrong command looks like. Under the old `> 0` threshold this case
    # passed only because the two sample lists were byte-identical; a real
    # same-work pair red it in two reps of eight.
    control = "shell startup does measurably more work than the bare floor"
    case(
        expect_pass=False,
        why="a startup measured at the bare floor must fail: it is not the measured path",
        red_gate=control,
        startup_samples=_GREEN["floor_samples"],
    )
    # The case the old `> 0` threshold could not red: a delta that is positive
    # but inside the same-work noise band measured for this pair (<= 0.143 ms).
    case(
        expect_pass=False,
        why="a startup only noise above the floor must fail: the control has a measured floor, not zero",
        red_gate=control,
        startup_samples=[value + STARTUP_WORK_FLOOR_MS / 2 for value in _GREEN["floor_samples"]],  # type: ignore[operator]
    )

    # ocx-sh/ocx#342's pair. A steady-state fire that still applies is the
    # unfixed reconciler; a first fire that applies nothing is an inert arena,
    # under which the steady-state gate would pass for the wrong reason.
    case(
        expect_pass=False,
        why="a steady-state prompt that still applies must fail",
        red_gate="steady-state reconcile applies nothing",
        streams={**_GREEN["streams"], "steady_applies": 3.0},  # type: ignore[dict-item]
    )
    case(
        expect_pass=False,
        why="an arena whose first fire applies nothing must fail: steady==0 proves nothing",
        red_gate="first reconcile applies the project's path entries",
        streams={**_GREEN["streams"], "first_applies": 0.0},  # type: ignore[dict-item]
    )

    # The arena's own non-vacuity pair. A tool-free lock is the state this file
    # shipped in for its whole life: every wall-clock number below it was
    # measured on a consent path whose per-tool loop ran zero times, and no
    # regression that scales with locked-tool count could move any of them.
    case(
        expect_pass=False,
        why="a tool-free arena must fail: the consent path's per-tool loop never runs",
        red_gate="the timed arena's lock carries tools",
        consent={"locked_tools": 0.0, "corroborated_tools": 0.0},
    )
    # And the half a tool count alone cannot see: `verified_sources` returns at
    # the FIRST uncorroborated tool, so eight declared and none corroborated
    # measures one iteration while the lock still reads as full.
    case(
        expect_pass=False,
        why="an uncorroborated lock must fail: clause 2 refused, so the loop stopped at tool 1",
        red_gate="consent clause 2 corroborates every locked tool",
        consent={"locked_tools": float(WALL_PROJECT_TOOLS), "corroborated_tools": 0.0},
    )
    # The knob is not the contract. `WALL_MIN_TOOLS` is what the gate asserts
    # against precisely so zeroing this one cannot make the gate agree with it —
    # and this is the assert that stops the knob being zeroed unnoticed anyway.
    assert WALL_PROJECT_TOOLS >= WALL_MIN_TOOLS, (
        f"the arena is sized at {WALL_PROJECT_TOOLS} tool(s), under the {WALL_MIN_TOOLS} its own "
        "gate demands — every run would red on an arena the constant asked for"
    )

    # The margin, not the budget, is what the floor has to be able to resolve.
    # These four are the smallest thing that fails if that comparison regresses:
    # rows 1 and 2 are the SAME breach on different machines and must disagree,
    # which is what makes this a resolution test rather than a redness test. Row
    # 1 is the shape that shipped red from CI on 2026-08-26 while the classifier
    # compared the scatter against `budget` and called 0.010 ms resolvable.
    for why, observed, spread, want in (
        ("a 0.010 ms overshoot under 0.692 ms of scatter is not resolvable", 2.010, 0.692, "abstain"),
        ("the same overshoot on a quiet floor is a red", 2.010, 0.002, "fail"),
        ("a 2.606 ms overshoot clears 0.692 ms of scatter outright", 4.606, 0.692, "fail"),
        ("under budget never abstains, however noisy the box", 1.594, 8.000, "pass"),
    ):
        probe = _budget_gate(
            name="probe", observed=observed, budget=RECONCILE_BUDGET_MS, spread=spread, breach="probe"
        )
        got = "abstain" if probe.inconclusive else ("pass" if probe.passed else "fail")
        assert got == want, f"{why}: expected {want}, got {got} (margin {abs(observed - RECONCILE_BUDGET_MS):.3f} ms)"

    # Contention (ocx-sh/ocx#340 follow-up). A floor that scattered wider than
    # the overshoot cannot decide that overshoot, so a breach on such a run is
    # NO VERDICT rather than a red — and the three cases below are what stop
    # that from becoming an amnesty. `_CONTENDED_FLOOR` scatters 2.5 ms (p90 5.5,
    # min 3.0) against a 2 ms budget; its startup series is shifted with it so
    # the same-work control still passes and cannot absorb the case under test.
    breaching_reconcile = [5.5, 8.0, 11.0]  # min 5.5 - floor 3.0 = 2.5 ms, over budget
    breaching_startup = [6.0, 8.0, 10.0]  # min 6.0 - floor 3.0 = 3.0 ms, over budget
    contended = {"floor_samples": _CONTENDED_FLOOR}
    assert floor_spread_ms(_CONTENDED_FLOOR) > RECONCILE_BUDGET_MS
    assert floor_spread_ms(_GREEN["floor_samples"]) <= RECONCILE_BUDGET_MS  # type: ignore[arg-type]
    # p90 and not max: one outlier in an otherwise tight series must NOT read as
    # contention, or a single unlucky sample abstains on a healthy tree.
    assert floor_spread_ms([3.0, 3.1, 3.2, 3.2, 3.3, 3.3, 3.4, 3.4, 3.5, 99.0]) < 1.0

    case(
        expect_pass=True,
        why="a reconcile over budget on a floor that scattered wider than the budget must reach no verdict",
        abstains="per-prompt reconcile <= exec_floor + delta",
        **contended,
        startup_samples=_CONTENDED_STARTUP,
        reconcile_samples=breaching_reconcile,
    )
    case(
        expect_pass=True,
        why="the startup budget abstains on the same evidence — one floor, both budgets",
        abstains="shell startup <= exec_floor + delta",
        **contended,
        startup_samples=breaching_startup,
    )
    # The asymmetry that keeps rule 3 honest: contention never rescues anything,
    # because a Δ inside the budget needs no rescuing. Only a breach can abstain,
    # and the classifier reaches that question without ever seeing the breach.
    case(
        expect_pass=True,
        why="a contended run whose deltas are inside the budget passes outright, with nothing abstaining",
        **contended,
        startup_samples=_CONTENDED_STARTUP,
    )
    # And the other side of the same coin: on a quiet floor a breach is a red,
    # not an abstention. `slow_reconcile` above already asserts it through the
    # default `abstains=""`; this pins the classifier's input explicitly so a
    # spread that silently grew cannot turn that case into an abstention while
    # the assert on `report.passed` still reads as satisfied.
    assert not slow_reconcile.inconclusive and not over_budget.inconclusive

    # `--expect-fail-gate`'s own two colours. Each needle must match the gate it
    # names and *only* that one: a run red on the other budget must not satisfy
    # it, or the flag certifies any red at all — which is what it exists to stop.
    needles: list[bool] = []
    for needle, red_here, green_here in (
        (RECONCILE_GATE_NEEDLE, slow_reconcile, over_budget),
        (STARTUP_GATE_NEEDLE, over_budget, slow_reconcile),
    ):
        assert unmatched_gate_needles([needle], red_here.gates) == [], (
            f"{needle!r} must match the gate that failed\n{format_report(red_here)}"
        )
        assert unmatched_gate_needles([needle], green_here.gates) == [needle], (
            f"{needle!r} must NOT be satisfied by a red on the other budget\n{format_report(green_here)}"
        )
        needles += [True, False]
    # An abstention is not a red, so it must not satisfy a needle either — a
    # contended runner would otherwise certify a fault injection it never
    # observed. `main` returns before the needle check on such a run; this pins
    # the matcher itself, so the two guards are independent.
    abstained = case(
        expect_pass=True,
        why="an abstaining reconcile gate must still be an abstention here",
        abstains="per-prompt reconcile <= exec_floor + delta",
        floor_samples=_CONTENDED_FLOOR,
        startup_samples=_CONTENDED_STARTUP,
        reconcile_samples=breaching_reconcile,
    )
    assert unmatched_gate_needles([RECONCILE_GATE_NEEDLE], abstained.gates) == [RECONCILE_GATE_NEEDLE], (
        "a gate that reached no verdict must not satisfy the needle that demands its red state"
    )
    needles.append(False)

    # Both needles at once is what the taskfile passes: one injected run has to
    # red both budgets, and a single red must not stand in for the pair.
    assert unmatched_gate_needles([STARTUP_GATE_NEEDLE, RECONCILE_GATE_NEEDLE], over_budget.gates) == [
        RECONCILE_GATE_NEEDLE
    ]
    needles.append(False)

    passes = sum(evaluator)
    abstentions = sum(len(report.inconclusive) for report in reports)
    print(
        f"self-check: {len(evaluator)} evaluator cases ({passes} green, {len(evaluator) - passes} red, "
        f"{abstentions} gate-level abstentions) + {len(needles)} --expect-fail-gate needle cases "
        f"({sum(needles)} matched, {len(needles) - sum(needles)} unmatched), all as specified"
    )


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, help="write the machine-readable result here")
    parser.add_argument("--samples", type=int, default=SAMPLES, help=f"process spawns per command (default {SAMPLES})")
    parser.add_argument("--self-check", action="store_true", help="exercise the pure evaluator and exit")
    parser.add_argument(
        "--expect-fail",
        action="store_true",
        help="invert the exit code: succeed only when the gate FAILS (fault-injection proof)",
    )
    parser.add_argument(
        "--expect-fail-gate",
        metavar="SUBSTRING",
        action="append",
        default=[],
        help=(
            "with --expect-fail, require the named gate specifically to have failed. Repeatable; "
            "EVERY named gate must be red. Without it, any red satisfies the run — which is how a "
            "second assert can ship with its own red state never demonstrated"
        ),
    )
    args = parser.parse_args(argv)

    if args.self_check:
        self_check()
        return 0

    ocx = matrix.ocx_binary()
    if ocx is None:
        print("no ocx binary (set OCX_COMMAND, or build test/bin/ocx)", file=sys.stderr)
        return 2

    report, artifact = run_gate(ocx, samples=args.samples)
    print(format_report(report))
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(artifact, indent=2) + "\n", encoding="utf-8")
        print(f"\nwrote {args.out}")
    _summarize(artifact)

    # An abstention is neither colour, and it short-circuits BOTH modes: a
    # contended runner cannot assert the budget, and it cannot demonstrate the
    # budget's red state either. Announcing it before the `--expect-fail` branch
    # keeps the two from disagreeing about the same measurement — the injected
    # run would otherwise read "the gate PASSED" off a run that decided nothing
    # and report a fault-injection failure that never happened.
    if report.inconclusive:
        names = [gate.name for gate in report.inconclusive]
        print(
            f"\n::warning::no wall-clock verdict on {names}: the bare-exec floor scattered "
            f"{artifact['floor_spread_ms']:.3f} ms across this run, wider than the amount by which the budget "
            "was overshot, so nothing that small is measurable here. The deterministic gates (exec counts, reconciler "
            "fixed point) still decided and are reported above. Re-run on a quiet machine for a wall-clock "
            "answer",
            file=sys.stderr,
        )
        # Only the wall clock abstains. A deterministic gate that went red is
        # still a red run, in either mode — the exec counts and the fixed point
        # do not care how busy the machine was, so contention must not become a
        # blanket amnesty. `report.passed` is False exactly when one of those
        # failed, and then this falls through to the normal handling below.
        if report.passed:
            return 0

    if args.expect_fail:
        if report.passed:
            print(
                f"\n::error::--expect-fail: the gate PASSED with {INJECT_ENV}="
                f"{artifact['injected_delay_ms']} — the injection did not red it. The delay is "
                "consumed inside `hook::registration` (startup) and `hook::checkpoint` (reconcile), "
                "so this means either the measured command emits neither, or the binary was built "
                "without `--features ocx/__testing`",
                file=sys.stderr,
            )
            return 1
        if args.expect_fail_gate:
            unmatched = unmatched_gate_needles(args.expect_fail_gate, report.gates)
            if unmatched:
                reds = [gate.name for gate in report.gates if not gate.passed]
                print(
                    f"\n::error::--expect-fail-gate: the run failed, but not on {unmatched} — "
                    f"red gates were {reds}. A red somewhere else does not demonstrate those gates' "
                    "red state",
                    file=sys.stderr,
                )
                return 1
            print(
                f"\n--expect-fail: {args.expect_fail_gate} failed as required "
                f"(injected {artifact['injected_delay_ms']} ms)"
            )
            return 0
        print(f"\n--expect-fail: gate failed as required (injected {artifact['injected_delay_ms']} ms)")
        return 0
    return 0 if report.passed else 1


if __name__ == "__main__":
    sys.exit(main())
