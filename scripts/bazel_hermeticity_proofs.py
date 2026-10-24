#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Stage-3 hermeticity — the three checks that gate removing `no-remote-cache` (C-023).

    scripts/bazel_hermeticity_proofs.py --prove-hermeticity [--probe-root DIR]
    scripts/bazel_hermeticity_proofs.py --check-declared --label L \
        --unchanged U.json --edited E.json --control C.json
    scripts/bazel_hermeticity_proofs.py --check-ambient --label L --touched T.json \
        --rerun-a A.json:OUT --rerun-b B.json:OUT --after-touch C.json:OUT
    scripts/bazel_hermeticity_proofs.py --check-tool-pin --label L \
        --baseline B.json:OUT --plain P.json:OUT --forced F.json:OUT [--marker M]

**What this gates.** C-023 ships `website/site.bzl` carrying `no-remote-cache`, and
only the *removal* of that tag is gated on the three greens below. So a red here is
not "a slow build": it is an unsound cache entry crossing a machine boundary. The
probe measures that literally — see `site_undeclared` below, whose stale output
survives `bazel clean` and comes back out of the disk cache.

**This file lands before its subject.** WP-34 (wave 9) writes the site rule; this is
wave 7. A harness that could only red after WP-34 landed is a harness nobody can red
today, so — as WP-13 and WP-35 did — everything is split three ways:

* **Comparators** — pure functions over already-read inputs, red-proven as pytest
  (`scripts/tests/test_bazel_hermeticity_proofs.py`) on fixtures built here. WP-34
  owns the `bazel build` invocations and calls `--check-*` for the verdict, so the
  check its implementation is written against is one already watched go red.
* **`--prove-hermeticity`** — a live Bazel run with a subject of its own: eight
  targets in a throwaway workspace, four of them deliberately unsound, built by the
  pinned binary with **no network fetch and no remote flag**. It is what makes the
  three checks measurements rather than assertions.
* **`--check-*`** — the three modes WP-34 drives.

---

## The reader, and the trap under it

**There is no per-label "was this cached" field for a build action.** BEP publishes
`cachedLocally` on a `TestResult`; an `ActionExecuted` event has no equivalent. The
natural substitute is "did an `ActionExecuted` event appear for this label, under
`--build_event_publish_all_actions`" — and measured on 9.2.0 that event is **byte
identical** (modulo `startTime`/`endTime`) between a genuine execution and a
`--disk_cache` hit. Both carry `success`, `type`, `commandLine`, a `primaryOutput`
and nothing else; the only place the difference appears is the build-level
`buildMetrics.actionSummary.runnerCount`, as `{"name": "disk cache hit", "count": N,
"execKind": "Remote"}`.

So this reader is named for what it can actually see: **`invalidated`**, meaning
*the local action cache could not satisfy this action* — the action key moved, or
the action cache is not there. It does **not** mean a process ran. That is the
property all three checks below want, and saying "executed" would be a claim the
bytes do not support.

Two consequences, both load-bearing:

1. A run taken after `bazel clean` has no action cache at all, so **every** label
   reads `invalidated` there and the per-label reader is meaningless on it. That run
   is read through `runnerCount` instead, and `disk_cache_hits()` is the reader.
2. A digest comparison across two builds is evidence only if those builds really
   re-executed. `reproducibility_findings` therefore takes a per-reading
   `reexecuted` flag sourced from `runnerCount` having **no** cache-hit runner, and
   reds `hermetic-repro-not-reexecuted` when it is false — otherwise two disk-cache
   hits compare equal and a date-stamping action reports reproducible.

Beyond that the reader floors on itself, because a floor that is an exit code is no
floor: the BEP's own `unstructuredCommandLine` is checked for
`--build_event_publish_all_actions` (without it only *failed* actions are published,
so every label reads "not invalidated" and the whole corpus greens over nothing),
`buildFinished` must be present, and the label must appear in the run's
`targetCompleted` universe — "absent from the executed set" means "cached" only for
a target that was in the build.

**One BEP file can hold two invocations.** `bazel test`/`build` retries transparently
after `Lost inputs` and writes a second stream into the same
`--build_event_json_file`. Two `started` events is therefore normal input, not
corruption: the reader unions the invalidated sets (a retry re-runs, so "invalidated
in any attempt" is the honest reading), records `attempts`, and requires the
publish-all-actions flag in **every** stream — one stream without it under-reports.

---

## (a) Declared-input invalidation — S-014

Editing one `test/doc_scripts/*.sh` body must invalidate the site rule; an unchanged
tree must do no work. **A hit on the edited tree means `test/doc_scripts/**/*.sh` is
missing from declared inputs and the rule is unsound.**

Three readings, not two, and the third is not a formality. "It rebuilt after the
edit" is equally consistent with *the inputs are declared* and with *this rule
rebuilds on everything*, and A1 already shipped that mistake once in this plan
(DX-18: two probe crates whose reverse closures were the same 16 crates). So
`declared_input_findings` requires an **unchanged** reading (must not invalidate — or
the rule always rebuilds and the green above is vacuous), an **edited** reading (must
invalidate — S-014's red state), and a **control** reading where a file *outside* the
declared set is edited (must not invalidate — or the rule invalidates universally and
the edited reading proves nothing).

## (b) Ambient-input isolation — and its polarity, stated

**The green outcome after touching an undeclared file is a cache HIT.** Not a miss.
This check was shipped inverted once in this chain and a cross-model reviewer caught
it: it demanded a **miss** after touching an **undeclared** file. Measured on the
probe, an undeclared touch invalidates **nothing** — neither the correct `site` nor
the unsound `site_ambient`, which reads that very file by absolute path. Cache state
cannot discriminate here **in either direction**, because the file is not in the key
by construction; that is what "undeclared" means. A check keyed on it answers the
same in every state, which is the definition of a check that never ran.

What discriminates is **output bytes under forced re-execution**, and it has two
independent legs, red-provable separately because the probe carries a target for
each:

* **the clock leg** — two genuinely cold builds of an *unchanged* tree must produce
  byte-identical output. `site_clock` appends `date +%s%N` and reds
  `hermetic-nondeterministic-output`. This is the date-stamp red.
* **the undeclared-read leg** — a third cold build taken *after* an undeclared file
  changed must produce the same bytes again. `site_ambient` reads `ambient.txt` by
  absolute path, is perfectly reproducible on an unchanged tree, and reds
  `hermetic-ambient-input` here. `site`, which reads neither, is green on both.

The cache reading is kept, but as a **precondition**, not the verdict: if the touched
file *did* invalidate the action then it is declared after all and the whole reading
is about the wrong file (`hermetic-ambient-declared`). That inversion — mistaking the
precondition for the finding — is exactly the shipped bug.

**And the sandbox is not the mechanism.** Measured: a genrule running under
`linux-sandbox` (runner confirmed, `execKind: Local`) reads
`<workspace>/ambient.txt` by absolute path without complaint. Bazel's sandbox
reconstructs the *execroot* from declared inputs; it does not unmount the host
filesystem. So no target — tagged or untagged — is protected from an absolute-path
ambient read by its strategy, and check (b) is the only thing standing there.

## (c) Tool-pin participation — and the nuance is the whole check

The ADR expected this green by construction (M-05: the tool digest sits in the
launcher text, so an action re-keys on a tool change). **WP-31 measured otherwise**,
with uv pinned 0.10.0 ↔ 0.10.1: lock bumped, plain `bazel build` → **stale**, 12
action-cache hits, output still `uv 0.10.0`; `bazel fetch --force --repo=@probe` then
build → **1 re-execution**, output `uv 0.10.1`. Root cause: `@tools`'s repo marker
records no `FILE:` entry for `ocx.toml` or `ocx.lock`. `project.bzl` believes it does
(`ctx.path(ctx.attr.ocx_lock)  # register the lock as an input`), but on Bazel 9.2.0
`ctx.path(Label)` does not register a content dependency — `grep ctx.watch
ocx/private/project.bzl` is 0 on the pinned commit and on `main`. rules_ocx's *lazy*
form carries the edge, because it uses `ctx.read()`, which auto-watches.

So "edit the lock, expect a rebuild" is the wrong check against the shipped eager
form: it reds, for a reason that is not the one being tested. `tool_pin_findings`
takes **two** readings and separates the two worlds:

| plain build after pin bump | after `fetch --force` + build | verdict |
|---|---|---|
| invalidated, output moved | (not needed) | green — the pin participates and the repo refetches |
| unchanged, stale output | invalidated, output moved | `hermetic-tool-repo-stale` — **the pin participates**; the repository did not refetch. rules_ocx `ctx.watch`, owner-gated |
| unchanged, stale output | unchanged **or** stale output | `hermetic-tool-pin-absent` — Block. The digest is not in the action key at all and M-05's launcher-text contract is false |

`marker_watch_findings` reads the same defect statically off the repository marker,
with no build at all, and is the independent second discriminator.

**`aquery --output=jsonproto`'s `actionKey` is not a reader for this.** WP-31
measured it byte-identical across all three tool states, because `Action.getKey()`
excludes input digests. It is shipped below as `_action_key_reader`, a named control
that answers "the pin does not participate" in every state — including the state
where the action demonstrably re-executed and emitted the new tool's output.

## PTY under sandbox — the expectation, made checkable, and it is false

C-022 gives the 39 cast targets `no-sandbox`, on the stated ground that a PTY does
not survive the sandbox. **Measured on Bazel 9.2.0 / Linux 6.18 WSL2, it does**:
`pty.openpty()` inside a genrule running under `linux-sandbox` succeeds and returns a
`/dev/pts/*` slave, exactly as it does under `local`. `PTY_SURVIVES_SANDBOX` records
that and `--prove-hermeticity` re-measures it, so the day it stops being true the
probe reds instead of the tag quietly losing its reason.

The tag is not thereby wrong — a cast target has other reasons to want the host (the
compose stack, writes outside the execroot) — but it is now **unjustified by the
stated reason**, and that matters here because `no-sandbox` is a hermeticity waiver.
`sandbox_waiver_findings` refuses a waiver that nothing checks: a label carrying
`no-sandbox` must appear in the set check (b) covers, and the site rule must not
carry the tag at all (C-023 gives it `requires-network` only).

---

**C-029.** No red or green here reaches the owner-gated remote realm. Asserted rather
than promised, three ways: every Bazel argv is passed through `realm_findings` before
it is spawned, the probe ships its own `.bazelrc` with no remote flag and is launched
`--nosystem_rc --nohome_rc` so an ambient one cannot be inherited, and `env_reads`
AST-checks that this file reads no environment variable at all (`Path.expanduser`,
which resolves `HOME` inside the standard library, is the one exception — it places
the probe outside the repository, and is named here).

Every cache claim is taken over **three** runs, never two: cold, warm on the same
server, and warm on a fresh server after `bazel clean` + `shutdown`. Two runs cannot
separate the action cache from `--disk_cache`, and the third is where the probe's
sharpest red lives.

Every mutation in the proofs is proven to have landed before its result is
trusted; a surviving green is *unexplained*, not excused. Fixtures are synthetic,
live under pytest's own `tmp_path` (never `/tmp`, a reaped tmpfs on this host),
and nothing is restored with `git checkout --`, which restores from the index.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import shutil
import subprocess
from pathlib import Path

from bazel_accept_proofs import env_reads, realm_findings
from bazel_gate_proofs import Finding, codes, report

# ---------------------------------------------------------------------------
# Measured facts. Each is one observation on the pinned binary, kept as a
# constant so that a release changing it reds here rather than quietly
# invalidating a contract written on top of it.
# ---------------------------------------------------------------------------

PTY_SURVIVES_SANDBOX = True
"""`pty.openpty()` inside a genrule under `linux-sandbox` → `/dev/pts/*`, exit 0.

Refutes C-022's stated reason for `no-sandbox` on the cast targets. Re-measured
by `--prove-hermeticity`; a change reds `probe-pty-drift`."""

SANDBOX_BLOCKS_ABSOLUTE_READS = False
"""A sandboxed genrule reads `<workspace>/ambient.txt` by absolute path, exit 0.

The sandbox rebuilds the execroot from declared inputs; it does not unmount the
host. So a `no-sandbox` tag removes no ambient-read protection that a sandboxed
target had, and check (b) is the only thing standing there either way."""

ACTION_EVENT_DISTINGUISHES_DISK_HIT = False
"""A `--disk_cache` hit emits an `ActionExecuted` event identical to a real run.

Measured field by field on `//:site`: `success`, `type`, `commandLine`,
`primaryOutput`, `configuration` all equal; only `startTime`/`endTime` differ.
This is why the per-label reader is named `invalidated` and not `executed`."""

PUBLISH_ALL_ACTIONS = "--build_event_publish_all_actions"
"""Without it BEP publishes only *failed* actions, so every label reads "not
invalidated" and all three checks green over nothing. The reader floors on the
flag's presence in the BEP's own recorded argv."""

CACHE_HIT_RUNNERS = ("disk cache hit", "remote cache hit")
"""`runnerCount` names that mean "no process ran". Matched case-insensitively on
a substring of `"cache hit"`, because the exact strings are Bazel-internal."""

# ---------------------------------------------------------------------------
# Messages. `code` is what a test asserts on; `message` is the stderr sentence.
# ---------------------------------------------------------------------------

BEP_UNREADABLE_MSG = "hermeticity: cannot read the {name} BEP {path}: {reason}"
BEP_MALFORMED_MSG = (
    "hermeticity: {count} unparseable event(s) in the {name} BEP — a truncated BEP reads as a "
    "smaller universe, not as a clean one"
)
BEP_NO_ALL_ACTIONS_MSG = (
    "hermeticity: the {name} BEP was written without " + PUBLISH_ALL_ACTIONS + " — only failed "
    "actions are published, so every label would read 'not invalidated' and all three checks "
    "would green over nothing"
)
BEP_TRUNCATED_MSG = (
    "hermeticity: the {name} BEP carries no buildFinished event — the build crashed or the file "
    "is partial, and a short read is not a clean tree"
)
BEP_EMPTY_MSG = (
    "hermeticity: the {name} BEP names 0 completed targets — the reader stopped or the build "
    "analysed nothing; a floor is a count, never an exit code"
)
BEP_ABSENT_LABEL_MSG = (
    "hermeticity: {label} is not in the {name} run's target universe — 'absent from the "
    "invalidated set' means 'cached' only for a target that was in the build"
)

DECLARED_MISSING_MSG = (
    "S-014: {label} was NOT invalidated after test/doc_scripts/*.sh changed — the script tree is "
    "missing from declared inputs and the rule is unsound; a cache entry built before the edit "
    "serves the old site to every other machine"
)
DECLARED_ALWAYS_MSG = (
    "S-014: {label} was invalidated on an UNCHANGED tree — the rule rebuilds on everything, so "
    "'it rebuilt after the edit' is vacuous and proves nothing about declared inputs"
)
DECLARED_OVERBROAD_MSG = (
    "S-014: {label} was invalidated by a change to {control_hint}, which is outside its declared "
    "inputs — the rule invalidates universally, so the edited reading discriminates nothing "
    "(the shape DX-18 refused for A1)"
)

AMBIENT_DECLARED_MSG = (
    "hermeticity(b): {label} WAS invalidated by the touched file, so that file is declared after "
    "all — this reading cannot test ambient isolation. Pick a genuinely undeclared file. (A cache "
    "MISS here is a setup error, never the finding: the inverted form of this check demanded one)"
)
AMBIENT_NOT_REEXECUTED_MSG = (
    "hermeticity(b): the {name} reading did not re-execute ({runners}) — comparing the output of "
    "two cache hits compares the same bytes, and a date-stamping action reports reproducible"
)
AMBIENT_NONDETERMINISTIC_MSG = (
    "hermeticity(b): {label} produced two different outputs from two cold builds of an UNCHANGED "
    "tree ({first} vs {second}) — the action reads something that is not an input (a clock, a "
    "hostname, a pid). Its cache entries are not interchangeable between machines"
)
AMBIENT_INPUT_MSG = (
    "hermeticity(b): {label}'s output moved ({before} -> {after}) when only an UNDECLARED file "
    "changed — the action reads it. The action key did not move with it, so the cache will keep "
    "serving whichever answer was recorded first"
)
AMBIENT_DIGEST_MSG = "hermeticity(b): the {name} reading has no usable output digest ({reason})"

TOOL_PIN_ABSENT_MSG = (
    "hermeticity(c): {label} did not move even after a forced refetch of the tool repository "
    "(invalidated={forced_invalidated}, output {baseline} -> {forced}) — the tool digest is not "
    "in the action key at all. M-05's contract, that the digest lives in the launcher text and so "
    "re-keys the action, is false here and every cached artifact is tool-version-blind"
)
TOOL_REPO_STALE_MSG = (
    "hermeticity(c): {label} kept the old tool's output on a plain build after the pin moved "
    "(output stayed {baseline}), and moved to {forced} only after a forced refetch — the pin "
    "DOES participate in the action key; the repository rule did not refetch. rules_ocx uses "
    "ctx.path(Label), which registers no content dependency on Bazel 9.2.0; ctx.watch is the fix "
    "and it is owner-gated"
)
TOOL_MARKER_MSG = (
    "hermeticity(c): the {repo} repository marker records no FILE: entry for {pins} — the "
    "repository rule does not watch its own pin, so a lock bump cannot refetch it. This is the "
    "static form of the same defect and needs no build"
)
TOOL_MARKER_READER_MSG = (
    "hermeticity(c): the {repo} marker names 0 FILE: entries at all — the reader read nothing, "
    "which is not the same as a marker that watches nothing"
)

WAIVER_UNCHECKED_MSG = (
    "hermeticity: {label} carries no-sandbox — a hermeticity waiver — and is not covered by check "
    "(b)'s reproducibility reading. Measured: the sandbox does not block an absolute-path read "
    "either way, so the waiver's cost is not the sandbox, it is that nothing else is looking"
)
WAIVER_SITE_MSG = (
    "hermeticity: the site rule {label} carries no-sandbox; C-023 gives it requires-network only"
)
WAIVER_READER_MSG = (
    "hermeticity: the tag table names 0 labels — a green over an empty table is a green over "
    "nothing read"
)


# ---------------------------------------------------------------------------
# Readings.
# ---------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True)
class Reading:
    """One `bazel build` invocation, as much of it as BEP will honestly say.

    `invalidated` is **not** "executed" — see the module docstring. It is "the
    local action cache could not satisfy this action", which is what the three
    checks below actually ask about.
    """

    name: str
    invalidated: frozenset[str]
    universe: frozenset[str]
    runners: dict[str, int]
    attempts: int

    @property
    def reexecuted(self) -> bool:
        """No cache-hit runner appeared, so the work in this run really ran."""
        return not any(_is_cache_hit(runner) for runner in self.runners)


@dataclasses.dataclass(frozen=True)
class OutputReading:
    """A `Reading` paired with the sha256 of the artifact it produced."""

    name: str
    run: Reading
    digest: str


def _is_cache_hit(runner: str) -> bool:
    return "cache hit" in runner.lower()


def disk_cache_hits(reading: Reading) -> int:
    """The build-level count of actions served from `--disk_cache`.

    The only place the disk-cache/execution distinction is visible at all, and
    it is an aggregate: BEP's per-action event carries no such field."""
    return sum(count for runner, count in reading.runners.items() if _is_cache_hit(runner))


def read_run(path: Path, name: str) -> tuple[Reading, list[Finding]]:
    """Newline-delimited BEP -> one `Reading`, plus every floor it failed.

    Tolerates two invocation streams in one file (a transparent retry after
    `Lost inputs`): the invalidated sets are unioned, and `attempts` counts the
    distinct `started` uuids so a caller can say which it read.
    """
    findings: list[Finding] = []
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        return (
            Reading(name, frozenset(), frozenset(), {}, 0),
            [Finding("hermetic-bep-unreadable", BEP_UNREADABLE_MSG.format(name=name, path=path, reason=error))],
        )

    invalidated: set[str] = set()
    universe: set[str] = set()
    runners: dict[str, int] = {}
    uuids: set[str] = set()
    command_lines = 0
    with_flag = 0
    finished = False
    malformed = 0

    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            # A truncated last line is what a crashed build leaves. Loud, never
            # shrugged off: a short read is a smaller universe, not a clean one.
            malformed += 1
            continue
        if not isinstance(event, dict):
            malformed += 1
            continue
        identifier = event.get("id") if isinstance(event.get("id"), dict) else {}

        started = event.get("started")
        if isinstance(started, dict) and isinstance(started.get("uuid"), str):
            uuids.add(started["uuid"])

        command = event.get("unstructuredCommandLine")
        if isinstance(command, dict):
            command_lines += 1
            args = command.get("args")
            if isinstance(args, list) and PUBLISH_ALL_ACTIONS in args:
                with_flag += 1

        if isinstance(event.get("action"), dict):
            label = _label_of(identifier, "actionCompleted")
            if label:
                invalidated.add(label)
            else:
                malformed += 1

        if isinstance(event.get("completed"), dict):
            label = _label_of(identifier, "targetCompleted")
            if label:
                universe.add(label)

        metrics = event.get("buildMetrics")
        if isinstance(metrics, dict):
            summary = metrics.get("actionSummary")
            if isinstance(summary, dict):
                for row in summary.get("runnerCount") or []:
                    if isinstance(row, dict) and isinstance(row.get("name"), str):
                        try:
                            runners[row["name"]] = runners.get(row["name"], 0) + int(row.get("count", 0))
                        except (TypeError, ValueError):
                            malformed += 1

        if isinstance(event.get("finished"), dict):
            finished = True

    reading = Reading(name, frozenset(invalidated), frozenset(universe), runners, max(len(uuids), 1))

    if malformed:
        findings.append(
            Finding("hermetic-bep-malformed", BEP_MALFORMED_MSG.format(count=malformed, name=name))
        )
    # Every stream must carry the flag; one that does not under-reports its own
    # half of the file, and the union would then be quietly short.
    if command_lines == 0 or with_flag != command_lines:
        findings.append(
            Finding("hermetic-bep-not-all-actions", BEP_NO_ALL_ACTIONS_MSG.format(name=name))
        )
    if not finished:
        findings.append(Finding("hermetic-bep-truncated", BEP_TRUNCATED_MSG.format(name=name)))
    if not universe:
        findings.append(Finding("hermetic-bep-empty-universe", BEP_EMPTY_MSG.format(name=name)))
    return reading, findings


def _label_of(identifier: dict[str, object], key: str) -> str | None:
    node = identifier.get(key)
    if not isinstance(node, dict):
        return None
    label = node.get("label")
    return label if isinstance(label, str) and label else None


def digest_of(path: Path) -> str:
    """sha256 of an artifact, or `""` when it cannot be read or is empty.

    Empty is folded into unreadable on purpose: an absent output and a
    zero-byte one both hash to something a comparison would call "equal",
    and equal is the green side of every digest check below."""
    try:
        payload = path.read_bytes()
    except OSError:
        return ""
    return hashlib.sha256(payload).hexdigest() if payload else ""


def universe_findings(reading: Reading, label: str) -> list[Finding]:
    if label in reading.universe:
        return []
    return [
        Finding(
            "hermetic-bep-absent-label",
            BEP_ABSENT_LABEL_MSG.format(label=label, name=reading.name),
        )
    ]


# ---------------------------------------------------------------------------
# (a) Declared-input invalidation — S-014.
# ---------------------------------------------------------------------------


def declared_input_findings(
    *, unchanged: Reading, edited: Reading, control: Reading, label: str, control_hint: str
) -> list[Finding]:
    """S-014, over three readings. Any two of them alone discriminate nothing."""
    findings: list[Finding] = []
    for reading in (unchanged, edited, control):
        findings.extend(universe_findings(reading, label))
    if findings:
        return findings

    if label not in edited.invalidated:
        findings.append(Finding("hermetic-declared-input-missing", DECLARED_MISSING_MSG.format(label=label)))
    if label in unchanged.invalidated:
        findings.append(Finding("hermetic-always-rebuilds", DECLARED_ALWAYS_MSG.format(label=label)))
    if label in control.invalidated:
        findings.append(
            Finding(
                "hermetic-overbroad-inputs",
                DECLARED_OVERBROAD_MSG.format(label=label, control_hint=control_hint),
            )
        )
    return findings


# ---------------------------------------------------------------------------
# (b) Ambient-input isolation. The polarity is in the module docstring and in
#     `AMBIENT_DECLARED_MSG`; it is: after touching an UNDECLARED file the
#     expected, correct outcome is a cache HIT.
# ---------------------------------------------------------------------------


def ambient_cache_findings(*, touched: Reading, label: str) -> list[Finding]:
    """The precondition, not the verdict.

    A hit is what a correctly isolated action gives — and, measured, also what
    an unsound one gives, because an undeclared file is not in the key by
    construction. So the only thing this reading can establish is that the file
    really is undeclared. An invalidation means the fixture picked a declared
    file and the reproducibility legs below are testing the wrong thing.
    """
    findings = universe_findings(touched, label)
    if findings:
        return findings
    if label in touched.invalidated:
        findings.append(Finding("hermetic-ambient-declared", AMBIENT_DECLARED_MSG.format(label=label)))
    return findings


def reproducibility_findings(
    *, rerun_a: OutputReading, rerun_b: OutputReading, after_touch: OutputReading, label: str
) -> list[Finding]:
    """The verdict: output bytes under forced re-execution, two legs."""
    findings: list[Finding] = []
    for reading in (rerun_a, rerun_b, after_touch):
        findings.extend(universe_findings(reading.run, label))
        if not reading.digest:
            findings.append(
                Finding(
                    "hermetic-repro-no-digest",
                    AMBIENT_DIGEST_MSG.format(name=reading.name, reason="absent or empty output"),
                )
            )
        # Without this the whole comparison is between cache hits, which are
        # equal by construction — the green a clock-reading action would get.
        elif not reading.run.reexecuted:
            findings.append(
                Finding(
                    "hermetic-repro-not-reexecuted",
                    AMBIENT_NOT_REEXECUTED_MSG.format(
                        name=reading.name, runners=", ".join(sorted(reading.run.runners)) or "none recorded"
                    ),
                )
            )
    if findings:
        return findings

    if rerun_a.digest != rerun_b.digest:
        return [
            Finding(
                "hermetic-nondeterministic-output",
                AMBIENT_NONDETERMINISTIC_MSG.format(
                    label=label, first=rerun_a.digest[:12], second=rerun_b.digest[:12]
                ),
            )
        ]
    if after_touch.digest != rerun_a.digest:
        findings.append(
            Finding(
                "hermetic-ambient-input",
                AMBIENT_INPUT_MSG.format(
                    label=label, before=rerun_a.digest[:12], after=after_touch.digest[:12]
                ),
            )
        )
    return findings


# ---------------------------------------------------------------------------
# (c) Tool-pin participation.
# ---------------------------------------------------------------------------


def tool_pin_findings(
    *, baseline: OutputReading, plain: OutputReading, forced: OutputReading, label: str
) -> list[Finding]:
    """Separate "the pin does not participate" from "the repo did not refetch".

    `baseline` is the output before the pin moved; `plain` is a build after the
    pin moved with nothing else done; `forced` is a build after
    `bazel fetch --force --repo=<tool repo>`.
    """
    findings: list[Finding] = []
    for reading in (baseline, plain, forced):
        findings.extend(universe_findings(reading.run, label))
        if not reading.digest:
            findings.append(
                Finding(
                    "hermetic-tool-no-digest",
                    AMBIENT_DIGEST_MSG.format(name=reading.name, reason="absent or empty output"),
                )
            )
    if findings:
        return findings

    plain_moved = label in plain.run.invalidated and plain.digest != baseline.digest
    forced_moved = label in forced.run.invalidated and forced.digest != baseline.digest

    if plain_moved:
        return []
    if forced_moved:
        return [
            Finding(
                "hermetic-tool-repo-stale",
                TOOL_REPO_STALE_MSG.format(
                    label=label, baseline=baseline.digest[:12], forced=forced.digest[:12]
                ),
            )
        ]
    return [
        Finding(
            "hermetic-tool-pin-absent",
            TOOL_PIN_ABSENT_MSG.format(
                label=label,
                forced_invalidated=label in forced.run.invalidated,
                baseline=baseline.digest[:12],
                forced=forced.digest[:12],
            ),
        )
    ]


def marker_watch_findings(marker_text: str, *, repo: str, pins: tuple[str, ...]) -> list[Finding]:
    """The same defect, read statically off a Bazel repository marker file.

    A marker records one `FILE:<path> <digest>` line per watched file. rules_ocx's
    `@tools` marker names none for `ocx.toml`/`ocx.lock`, which is why a lock bump
    cannot refetch it. Floored on the reader: a marker naming zero `FILE:` entries
    of any kind means the reader read nothing, which is a different answer from
    "watches nothing".
    """
    watched = [
        line.split(":", 1)[1].strip()
        for line in marker_text.splitlines()
        if line.startswith("FILE:")
    ]
    if not watched:
        return [Finding("hermetic-tool-marker-unread", TOOL_MARKER_READER_MSG.format(repo=repo))]
    missing = [pin for pin in pins if not any(entry.split()[0].endswith(pin) for entry in watched if entry)]
    if missing:
        return [
            Finding(
                "hermetic-tool-marker-unwatched",
                TOOL_MARKER_MSG.format(repo=repo, pins=", ".join(missing)),
            )
        ]
    return []


def _action_key_reader(keys: dict[str, str], baseline: str) -> bool:
    """The wrong reader, kept as a named control, used nowhere else.

    `aquery --output=jsonproto`'s `actionKey` is the natural thing to diff, and
    WP-31 measured it byte-identical across all three tool states: unchanged
    pin, bumped pin, bumped pin after a forced refetch. `Action.getKey()`
    excludes input digests, so it moves only when the *command line* moves.
    This reader therefore answers "the pin does not participate" in every state,
    including the one where the action demonstrably re-executed and wrote the
    new tool's output — a constant, not a discriminator.
    """
    return any(key != baseline for key in keys.values())


# ---------------------------------------------------------------------------
# The sandbox waiver.
# ---------------------------------------------------------------------------


def sandbox_waiver_findings(
    *, tags: dict[str, set[str]], reproducibility_checked: set[str], site_label: str
) -> list[Finding]:
    """`no-sandbox` is a hermeticity waiver; refuse one nothing checks."""
    if not tags:
        return [Finding("hermetic-waiver-reader", WAIVER_READER_MSG)]
    findings: list[Finding] = []
    if "no-sandbox" in tags.get(site_label, set()):
        findings.append(Finding("hermetic-site-no-sandbox", WAIVER_SITE_MSG.format(label=site_label)))
    for label in sorted(tags):
        if "no-sandbox" in tags[label] and label not in reproducibility_checked:
            findings.append(
                Finding("hermetic-waiver-unchecked", WAIVER_UNCHECKED_MSG.format(label=label))
            )
    return findings


# ---------------------------------------------------------------------------
# The live probe. Eight targets, four deliberately unsound, no network fetch.
# ---------------------------------------------------------------------------

PROBE_DEFAULT_ROOT = Path("~/.cache/ocx/wp32-hermeticity-probe")
BAZEL_BIN = Path("~/.ocx/symlinks/ocx.sh/bazelbuild/bazel/candidates/9.2.0/content/bazel")

CACHE_TARGETS = ("site", "site_undeclared", "site_clock", "site_ambient", "site_eager", "site_lazy")
PTY_TARGETS = ("pty_sandboxed", "pty_nosandbox")
REPRO_TARGETS = ("site", "site_clock", "site_ambient")

PROBE_SHAPES: dict[str, str] = {
    "site": "correct: declares doc_scripts/*.sh + src/index.md, reads nothing else",
    "site_undeclared": "unsound (a): reads doc_scripts/*.sh by absolute path, declares neither",
    "site_clock": "unsound (b1): appends date +%s%N",
    "site_ambient": "unsound (b2): reads ambient.txt by absolute path",
    "site_eager": "tool from @eager_tool — ctx.path(Label), rules_ocx's shipped shape",
    "site_lazy": "tool from @lazy_tool — ctx.read(), which auto-watches",
}

#: Phase B, the cache table: `True` means the label was **invalidated** in that
#: run (the local action cache could not satisfy it). Shipped as an expectation
#: rather than a note so a Bazel release moving any cell reds here.
#:
#: `warm-fresh-server` is every-cell-True by construction — `bazel clean`
#: removes the action cache, so nothing can be satisfied from it. That run is
#: read through `disk_cache_hits()` instead, and its expectation is the count
#: below, not this table.
PROBE_EXPECTED: dict[str, tuple[bool, ...]] = {}
"""Filled from the measurement below; see `PROBE_RUNS` for the column order."""

PROBE_RUNS = (
    "cold",
    "warm-noop",
    "edit-declared",
    "warm-fresh-server",
    "touch-ambient",
    "bump-pin",
    "forced-fetch",
)

PROBE_EXPECTED = {
    #                   cold  noop  edit-declared  fresh  touch-ambient  bump-pin  forced-fetch
    "site": (True, False, True, True, False, False, False),
    "site_undeclared": (True, False, False, True, False, False, False),
    "site_clock": (True, False, False, True, False, False, False),
    "site_ambient": (True, False, False, True, False, False, False),
    "site_eager": (True, False, False, True, False, False, True),
    "site_lazy": (True, False, False, True, False, True, False),
}

FRESH_SERVER_DISK_HITS = len(CACHE_TARGETS)
"""C-029's third run. After `bazel clean` + `shutdown` the action cache is gone
and every one of the six genrules must come back out of `--disk_cache` — which
is also where `site_undeclared`'s stale output comes from, the literal shape of
an unsound entry reaching another machine."""

#: Phase A, the reproducibility table: (rerun_a == rerun_b, after_touch == rerun_a).
REPRO_EXPECTED: dict[str, tuple[bool, bool]] = {
    "site": (True, True),
    "site_clock": (False, False),
    "site_ambient": (True, False),
}

PIN_BEFORE = "0.10.0"
PIN_AFTER = "0.10.1"
"""WP-31's uv pair, reproduced locally with no network and no rules_ocx."""


def probe_module() -> str:
    return (
        'module(name = "wp32_hermeticity_probe", version = "0.0.0")\n'
        '\npin_ext = use_extension("//:pin.bzl", "pin_ext")\n'
        'use_repo(pin_ext, "eager_tool", "lazy_tool")\n'
    )


def probe_pin_bzl() -> str:
    """Two repository rules over one pin file: one watches it, one does not.

    This is WP-31's rules_ocx measurement, reproduced with no network and no
    ruleset. `_eager_impl` is `project.bzl`'s shipped shape — `ctx.path(Label)`
    followed by a subprocess that reads the file — and on Bazel 9.2.0 that
    registers no content dependency. `_lazy_impl` uses `ctx.read()`, which
    auto-watches, and is the positive control: without it, "the eager repo did
    not refetch" would be indistinguishable from "this workspace cannot
    refetch anything".
    """
    return (
        '"""Generated by scripts/bazel_hermeticity_proofs.py --prove-hermeticity."""\n'
        "\ndef _launcher(ctx, digest):\n"
        '    ctx.file("BUILD.bazel", \'exports_files(["launcher.sh"])\\n\')\n'
        '    ctx.file("launcher.sh", "#!/bin/sh\\necho tool-digest " + digest + "\\n", executable = True)\n'
        "\ndef _eager_impl(ctx):\n"
        "    # rules_ocx's shipped shape: ctx.path(Label), then a subprocess reads it.\n"
        "    path = ctx.path(ctx.attr.pin)\n"
        '    _launcher(ctx, ctx.execute(["cat", str(path)]).stdout.strip())\n'
        "\ndef _lazy_impl(ctx):\n"
        "    # ctx.read() auto-watches, so a pin change refetches this repository.\n"
        "    _launcher(ctx, ctx.read(ctx.attr.pin).strip())\n"
        "\neager_tool = repository_rule(implementation = _eager_impl, attrs = "
        '{"pin": attr.label()})\n'
        "lazy_tool = repository_rule(implementation = _lazy_impl, attrs = "
        '{"pin": attr.label()})\n'
        "\ndef _ext_impl(_ctx):\n"
        '    eager_tool(name = "eager_tool", pin = Label("//:pin.txt"))\n'
        '    lazy_tool(name = "lazy_tool", pin = Label("//:pin.txt"))\n'
        "\npin_ext = module_extension(implementation = _ext_impl)\n"
    )


def probe_build_file(root: Path) -> str:
    """Six cache targets and two PTY targets. The absolute paths are the point.

    An unsound rule cannot be written with a relative path — a genrule's cwd is
    the execroot, which holds only declared inputs. Reaching the *source* tree
    takes an absolute path, and that is exactly the ambient read the sandbox
    was measured not to block.
    """
    return f"""\
genrule(
    name = "site",
    srcs = glob(["doc_scripts/*.sh"]) + ["src/index.md"],
    outs = ["site.txt"],
    cmd = "cat $(SRCS) > $@",
)

genrule(
    name = "site_undeclared",
    srcs = ["src/index.md"],
    outs = ["site_undeclared.txt"],
    cmd = "cat $(SRCS) > $@ && cat {root}/doc_scripts/*.sh >> $@",
)

genrule(
    name = "site_clock",
    srcs = ["src/index.md"],
    outs = ["site_clock.txt"],
    cmd = "cat $(SRCS) > $@ && date +%s%N >> $@",
)

genrule(
    name = "site_ambient",
    srcs = ["src/index.md"],
    outs = ["site_ambient.txt"],
    cmd = "cat $(SRCS) > $@ && cat {root}/ambient.txt >> $@",
)

genrule(
    name = "site_eager",
    srcs = ["src/index.md"],
    outs = ["site_eager.txt"],
    tools = ["@eager_tool//:launcher.sh"],
    cmd = "$(location @eager_tool//:launcher.sh) > $@ && cat $(SRCS) >> $@",
)

genrule(
    name = "site_lazy",
    srcs = ["src/index.md"],
    outs = ["site_lazy.txt"],
    tools = ["@lazy_tool//:launcher.sh"],
    cmd = "$(location @lazy_tool//:launcher.sh) > $@ && cat $(SRCS) >> $@",
)

genrule(
    name = "pty_sandboxed",
    srcs = ["pty_probe.sh"],
    outs = ["pty_sandboxed.txt"],
    cmd = "$(location pty_probe.sh) > $@",
)

genrule(
    name = "pty_nosandbox",
    srcs = ["pty_probe.sh"],
    outs = ["pty_nosandbox.txt"],
    cmd = "$(location pty_probe.sh) > $@",
    tags = ["no-sandbox"],
)
"""


PTY_PROBE = """\
#!/bin/sh
python3 -c '
import os, pty, sys
try:
    master, slave = pty.openpty()
    os.write(slave, b"x")
    os.close(slave)
    os.close(master)
    print("pty-ok")
except Exception as error:
    print("pty-failed", type(error).__name__)
    sys.exit(7)
'
"""


def probe_rc(disk_cache: Path, output_root: Path, repo_cache: Path) -> str:
    """The probe's whole rc. No remote flag of any kind reaches it (C-029).

    `--output_user_root` and `--repo_contents_cache` sit outside the probe
    workspace because Bazel 9 refuses a repo contents cache inside the main
    repository (DX-16), and off `/tmp`, a reaped tmpfs on this host.
    """
    return (
        "# Generated by scripts/bazel_hermeticity_proofs.py --prove-hermeticity.\n"
        "startup --host_jvm_args=-Xmx2g\n"
        f"startup --output_user_root={output_root}\n"
        f"common --repo_contents_cache={repo_cache}\n"
        f"build --disk_cache={disk_cache}\n"
        "build --jobs=4\n"
    )


@dataclasses.dataclass(frozen=True)
class Probe:
    root: Path
    disk_cache: Path
    output_root: Path


def materialise_probe(root: Path) -> Probe:
    if root.exists():
        shutil.rmtree(root)
    (root / "doc_scripts").mkdir(parents=True)
    (root / "src").mkdir(parents=True)
    (root / "MODULE.bazel").write_text(probe_module(), encoding="utf-8")
    (root / "pin.bzl").write_text(probe_pin_bzl(), encoding="utf-8")
    (root / "BUILD.bazel").write_text(probe_build_file(root), encoding="utf-8")
    (root / "doc_scripts" / "a.sh").write_text("echo one\n", encoding="utf-8")
    (root / "doc_scripts" / "b.sh").write_text("echo two\n", encoding="utf-8")
    (root / "src" / "index.md").write_text("# index\n", encoding="utf-8")
    (root / "ambient.txt").write_text("ambient-v1\n", encoding="utf-8")
    (root / "pin.txt").write_text(f"{PIN_BEFORE}\n", encoding="utf-8")
    probe = root / "pty_probe.sh"
    probe.write_text(PTY_PROBE, encoding="utf-8")
    probe.chmod(0o755)

    home = root.parent
    disk_cache = home / f"{root.name}-disk"
    output_root = home / f"{root.name}-out"
    # Shared across probe roots on purpose, and the only one that is: the disk
    # cache and the output base are the subject and are wiped per phase, while
    # the repo contents cache holds nothing but this workspace's own two
    # generated repositories.
    repo_cache = home / "wp32-probe-repo"
    (root / ".bazelrc").write_text(probe_rc(disk_cache, output_root, repo_cache), encoding="utf-8")
    return Probe(root=root, disk_cache=disk_cache, output_root=output_root)


def probe_argv(bazel: Path, command: str, *rest: str) -> list[str]:
    """`--nosystem_rc --nohome_rc`: only the probe's own rc reaches this run.

    Without them an ambient `~/.bazelrc` carrying a `--remote_cache` line would
    make every cache claim below a claim about a realm rather than about
    `--disk_cache`, which is the coupling C-029 forbids.
    """
    return [str(bazel), "--nosystem_rc", "--nohome_rc", command, *rest]


def run_probe(probe: Probe, argv: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv, cwd=probe.root, capture_output=True, text=True, encoding="utf-8", check=False, timeout=900
    )


def probe_build(
    probe: Probe, bazel: Path, name: str, targets: tuple[str, ...]
) -> tuple[Reading, list[Finding], subprocess.CompletedProcess[str]]:
    bep = probe.root / f"bep-{name}.json"
    argv = probe_argv(
        bazel,
        "build",
        *[f"//:{target}" for target in targets],
        f"--build_event_json_file={bep}",
        PUBLISH_ALL_ACTIONS,
    )
    guard = realm_findings({f"argv[{name}]": " ".join(argv)})
    if guard:
        raise SystemExit(report(guard))
    outcome = run_probe(probe, argv)
    reading, findings = read_run(bep, name)
    return reading, findings, outcome


def probe_digests(probe: Probe, targets: tuple[str, ...]) -> dict[str, str]:
    return {name: digest_of(probe.root / "bazel-bin" / f"{name}.txt") for name in targets}


def probe_text(probe: Probe, name: str) -> str:
    path = probe.root / "bazel-bin" / f"{name}.txt"
    return path.read_text(encoding="utf-8").strip() if path.exists() else "<absent>"


def wipe_caches(probe: Probe, bazel: Path) -> None:
    """A cold run must be genuinely cold, and wiping the workspace is not it.

    The action cache lives in the output base, which survives a rebuilt
    workspace; `bazel clean` leaves `--disk_cache`, which is the one thing the
    fresh-server run is about. Shut the server down first — it owns the
    directory about to be removed.
    """
    run_probe(probe, probe_argv(bazel, "shutdown"))
    for stale in (probe.disk_cache, probe.output_root):
        if stale.exists():
            shutil.rmtree(stale)


def run_prove_hermeticity(root: Path) -> int:
    """Two phases, eleven Bazel invocations, three measured tables."""
    bazel = BAZEL_BIN.expanduser()
    if not bazel.exists():
        print(
            f"--prove-hermeticity needs the pinned binary at {bazel} — it resolves from ocx.lock "
            "(`ocx pull`). Refusing to run against whatever `bazel` is on PATH."
        )
        return 1

    probe = materialise_probe(root.expanduser())
    guard = realm_findings({".bazelrc": (probe.root / ".bazelrc").read_text(encoding="utf-8")})
    if guard:
        print("C-029: refusing to run a probe whose rc could reach the remote realm")
        return report(guard)
    print(f"--prove-hermeticity: probe {probe.root}, disk cache {probe.disk_cache}, no remote flag")
    for name, shape in PROBE_SHAPES.items():
        print(f"  //:{name:<16} {shape}")

    findings: list[Finding] = []
    findings.extend(_phase_reproducibility(probe, bazel))
    probe = materialise_probe(root.expanduser())
    findings.extend(_phase_cache_table(probe, bazel))
    findings.extend(_phase_pty(probe, bazel))
    return report(findings)


def _phase_reproducibility(probe: Probe, bazel: Path) -> list[Finding]:
    """Three genuinely cold builds — the only way to force re-execution here.

    `--disk_cache` is the whole point of the exercise, so a "rebuild" that hits
    it is not a re-execution and the digests it returns are the *recorded*
    bytes, not this run's. Each leg therefore wipes both caches, and the floor
    is `Reading.reexecuted` — no cache-hit runner in `runnerCount`.
    """
    print("\nphase A — reproducibility under forced re-execution (three cold builds)")
    findings: list[Finding] = []
    legs: dict[str, dict[str, str]] = {}
    runs: dict[str, Reading] = {}

    for leg in ("rerun-a", "rerun-b", "after-touch"):
        if leg == "after-touch":
            ambient = probe.root / "ambient.txt"
            ambient.write_text("ambient-v2\n", encoding="utf-8")
            landed = ambient.read_text(encoding="utf-8")
            if landed != "ambient-v2\n":
                return [Finding("probe-mutation-lost", f"the ambient mutation did not land: {landed!r}")]
        wipe_caches(probe, bazel)
        reading, parse, outcome = probe_build(probe, bazel, leg, CACHE_TARGETS)
        findings.extend(parse)
        if outcome.returncode != 0:
            print(outcome.stderr[-2000:])
            return [*findings, Finding("probe-build-failed", f"the {leg} build exited {outcome.returncode}")]
        runs[leg] = reading
        legs[leg] = probe_digests(probe, REPRO_TARGETS)
        hits = disk_cache_hits(reading)
        print(f"  {leg:<12} runners={dict(sorted(reading.runners.items()))} cache-hits={hits}")
        if not reading.reexecuted:
            findings.append(
                Finding(
                    "probe-not-cold",
                    f"the {leg} build was served from cache ({hits} hits) — its digests are the "
                    "recorded bytes, not this run's, and every comparison below is vacuous",
                )
            )

    print(f"  {'target':<16}{'a==b':>7}{'c==a':>7}  verdict")
    for name in REPRO_TARGETS:
        same_tree = legs["rerun-a"][name] == legs["rerun-b"][name]
        after = legs["after-touch"][name] == legs["rerun-a"][name]
        verdict = reproducibility_findings(
            rerun_a=OutputReading("rerun-a", runs["rerun-a"], legs["rerun-a"][name]),
            rerun_b=OutputReading("rerun-b", runs["rerun-b"], legs["rerun-b"][name]),
            after_touch=OutputReading("after-touch", runs["after-touch"], legs["after-touch"][name]),
            label=f"//:{name}",
        )
        print(f"  {name:<16}{same_tree!s:>7}{after!s:>7}  {', '.join(codes(verdict)) or 'GREEN'}")
        if (same_tree, after) != REPRO_EXPECTED[name]:
            findings.append(
                Finding(
                    "probe-repro-drift",
                    f"{name} answered {(same_tree, after)}, the measurement on bazel 9.2.0 is "
                    f"{REPRO_EXPECTED[name]}",
                )
            )
    return findings


def _phase_cache_table(probe: Probe, bazel: Path) -> list[Finding]:
    """Phase B: one output base, seven incremental runs, the invalidation table."""
    print("\nphase B — invalidation across seven runs (C-029: cold / warm / warm fresh server)")
    findings: list[Finding] = []
    observed: dict[str, list[bool]] = {name: [] for name in CACHE_TARGETS}
    runs: dict[str, Reading] = {}
    digests: dict[str, dict[str, str]] = {}

    wipe_caches(probe, bazel)
    for run in PROBE_RUNS:
        mutation = _apply_probe_mutation(probe, bazel, run)
        if mutation is not None:
            return [*findings, mutation]
        reading, parse, outcome = probe_build(probe, bazel, run, CACHE_TARGETS)
        findings.extend(parse)
        if outcome.returncode != 0:
            print(outcome.stderr[-2000:])
            return [*findings, Finding("probe-build-failed", f"the {run} build exited {outcome.returncode}")]
        runs[run] = reading
        digests[run] = probe_digests(probe, CACHE_TARGETS)
        for name in CACHE_TARGETS:
            observed[name].append(f"//:{name}" in reading.invalidated)

    widths = (6, 10, 15, 19, 15, 10, 14)
    print(f"  {'target':<16}" + "".join(f"{run:>{w}}" for run, w in zip(PROBE_RUNS, widths, strict=True)))
    for name in CACHE_TARGETS:
        cells = tuple(observed[name])
        print(
            f"  {name:<16}"
            + "".join(
                f"{'INVAL' if cell else 'hit':>{w}}" for cell, w in zip(cells, widths, strict=True)
            )
        )
        if cells != PROBE_EXPECTED[name]:
            findings.append(
                Finding(
                    "probe-table-drift",
                    f"{name} answered {cells}, the measurement on bazel 9.2.0 is "
                    f"{PROBE_EXPECTED[name]} — the mechanism the three checks rest on has moved",
                )
            )

    fresh_hits = disk_cache_hits(runs["warm-fresh-server"])
    print(
        f"  warm-fresh-server: {fresh_hits} disk-cache hits "
        f"(expected {FRESH_SERVER_DISK_HITS}) — read from runnerCount, because after "
        "`bazel clean` every label reads INVAL and the per-label reader says nothing"
    )
    if fresh_hits != FRESH_SERVER_DISK_HITS:
        findings.append(
            Finding(
                "probe-fresh-server-drift",
                f"the fresh-server run reported {fresh_hits} disk-cache hits, not "
                f"{FRESH_SERVER_DISK_HITS} — the third run is what separates the action cache "
                "from --disk_cache, and two runs cannot",
            )
        )
    stale = probe_text(probe, "site_undeclared")
    print(f"  site_undeclared.txt after `bazel clean`: {stale!r}")
    if "ONE-EDITED" in stale:
        findings.append(
            Finding(
                "probe-stale-not-served",
                "site_undeclared came back correct after `bazel clean`, so the unsound entry did "
                "not survive — the red this probe exists to show did not happen",
            )
        )

    print("\n  the three checks, over the runs above:")
    for name, expected in (("site", []), ("site_undeclared", ["hermetic-declared-input-missing"])):
        verdict = declared_input_findings(
            unchanged=runs["warm-noop"],
            edited=runs["edit-declared"],
            control=runs["touch-ambient"],
            label=f"//:{name}",
            control_hint="ambient.txt",
        )
        print(f"    (a) //:{name:<15} {', '.join(codes(verdict)) or 'GREEN'}")
        if codes(verdict) != expected:
            findings.append(
                Finding("probe-a-drift", f"//:{name} answered {codes(verdict)}, expected {expected}")
            )

    print(
        "    (b) polarity           touch-ambient invalidated "
        f"{sorted(label for label in runs['touch-ambient'].invalidated if label.startswith('//:'))} "
        "— the unsound site_ambient HIT, which is why a miss is the wrong expectation"
    )
    if any(label.startswith("//:") for label in runs["touch-ambient"].invalidated):
        findings.append(
            Finding(
                "probe-ambient-polarity",
                "touching an undeclared file invalidated something — then it is declared, and "
                "check (b)'s cache reading is about the wrong file",
            )
        )

    for name, expected in (("site_lazy", []), ("site_eager", ["hermetic-tool-repo-stale"])):
        verdict = tool_pin_findings(
            baseline=OutputReading("baseline", runs["warm-noop"], digests["warm-noop"][name]),
            plain=OutputReading("plain", runs["bump-pin"], digests["bump-pin"][name]),
            forced=OutputReading("forced", runs["forced-fetch"], digests["forced-fetch"][name]),
            label=f"//:{name}",
        )
        # The digests, not the file on disk: by now the tree holds the
        # post-forced-fetch output, which would read green for both.
        print(
            f"    (c) //:{name:<15} {', '.join(codes(verdict)) or 'GREEN'} — "
            f"baseline {digests['warm-noop'][name][:8]}, plain {digests['bump-pin'][name][:8]}, "
            f"forced {digests['forced-fetch'][name][:8]}"
        )
        if codes(verdict) != expected:
            findings.append(
                Finding("probe-c-drift", f"//:{name} answered {codes(verdict)}, expected {expected}")
            )
    return findings


def _apply_probe_mutation(probe: Probe, bazel: Path, run: str) -> Finding | None:
    """The edit each run is taken after. Every one is read back before it counts."""
    if run == "edit-declared":
        target = probe.root / "doc_scripts" / "a.sh"
        target.write_text("echo ONE-EDITED\n", encoding="utf-8")
        if target.read_text(encoding="utf-8") != "echo ONE-EDITED\n":
            return Finding("probe-mutation-lost", "the doc_scripts/a.sh edit did not land")
    elif run == "warm-fresh-server":
        run_probe(probe, probe_argv(bazel, "clean"))
        run_probe(probe, probe_argv(bazel, "shutdown"))
    elif run == "touch-ambient":
        target = probe.root / "ambient.txt"
        target.write_text("ambient-v2\n", encoding="utf-8")
        if target.read_text(encoding="utf-8") != "ambient-v2\n":
            return Finding("probe-mutation-lost", "the ambient.txt edit did not land")
    elif run == "bump-pin":
        target = probe.root / "pin.txt"
        target.write_text(f"{PIN_AFTER}\n", encoding="utf-8")
        if target.read_text(encoding="utf-8") != f"{PIN_AFTER}\n":
            return Finding("probe-mutation-lost", "the pin.txt bump did not land")
    elif run == "forced-fetch":
        outcome = run_probe(probe, probe_argv(bazel, "fetch", "--force", "--repo=@eager_tool"))
        if outcome.returncode != 0:
            return Finding(
                "probe-fetch-failed",
                f"`fetch --force --repo=@eager_tool` exited {outcome.returncode}: "
                f"{outcome.stderr.strip()[-300:]}",
            )
    return None


def _phase_pty(probe: Probe, bazel: Path) -> list[Finding]:
    """C-022's stated reason for `no-sandbox`, measured rather than repeated."""
    print("\nphase C — does a PTY survive the sandbox?")
    findings: list[Finding] = []
    observed: dict[str, str] = {}
    for name in PTY_TARGETS:
        _, parse, outcome = probe_build(probe, bazel, f"pty-{name}", (name,))
        findings.extend(parse)
        observed[name] = probe_text(probe, name)
        print(f"  //:{name:<16} exit={outcome.returncode} output={observed[name]!r}")
    survived = observed.get("pty_sandboxed") == "pty-ok"
    print(
        f"  PTY_SURVIVES_SANDBOX={PTY_SURVIVES_SANDBOX}, measured {survived} — C-022 gives the 39 "
        "cast targets no-sandbox because 'a PTY does not survive the sandbox'"
    )
    if survived != PTY_SURVIVES_SANDBOX:
        findings.append(
            Finding(
                "probe-pty-drift",
                f"pty_sandboxed answered {observed.get('pty_sandboxed')!r}; PTY_SURVIVES_SANDBOX "
                f"is {PTY_SURVIVES_SANDBOX}. C-022's tag rationale turns on this",
            )
        )
    return findings


# ---------------------------------------------------------------------------
# Fixtures. Synthetic BEP, built to the shapes measured above.
# ---------------------------------------------------------------------------


def bep_lines(
    *,
    invalidated: tuple[str, ...],
    universe: tuple[str, ...],
    runners: dict[str, int] | None = None,
    all_actions: bool = True,
    finished: bool = True,
    uuid: str = "11111111-1111-1111-1111-111111111111",
) -> list[str]:
    """One invocation's worth of BEP, in the shapes `read_run` looks for."""
    args = ["build", "//:all", "--build_event_json_file=/dev/null"]
    if all_actions:
        args.append(PUBLISH_ALL_ACTIONS)
    events: list[object] = [
        {"id": {"started": {}}, "started": {"uuid": uuid, "command": "build"}},
        {"id": {"unstructuredCommandLine": {}}, "unstructuredCommandLine": {"args": args}},
        # Noise the reader must ignore: an event whose payload is not an action
        # but whose id carries a label that would move every verdict below.
        {"id": {"progress": {}}, "progress": {"stderr": "//:site_undeclared\n"}},
    ]
    for label in universe:
        events.append({"id": {"targetCompleted": {"label": label}}, "completed": {"success": True}})
    for label in invalidated:
        events.append(
            {
                "id": {"actionCompleted": {"label": label, "primaryOutput": "out"}},
                "action": {"success": True, "label": label, "type": "Genrule"},
            }
        )
    if finished:
        events.append({"id": {"buildFinished": {}}, "finished": {"overallSuccess": True}})
        events.append(
            {
                "id": {"buildMetrics": {}},
                "buildMetrics": {
                    "actionSummary": {
                        "runnerCount": [
                            {"name": name, "count": count}
                            for name, count in (runners or {"linux-sandbox": len(invalidated) or 1}).items()
                        ]
                    }
                },
            }
        )
    return [json.dumps(event) for event in events]


def write_bep(path: Path, *chunks: list[str]) -> None:
    """Write a build event stream, owner-readable only.

    A real BEP serialises the whole client environment and every rc flag value -
    measured: 276 `--client_env` entries, both AWS keys and nine
    `--remote_header` occurrences, with `=` escaped as `\\u003d` so a KEY=VALUE
    scanner reports the file clean. These fixtures are synthetic, but they sit
    in the same directories as the real streams this script's live leg writes,
    and `task bazel:doctor` reds on any world-readable BEP under `~/.cache/ocx`.
    `open(2)` is umask-clipped, so the mode is set after the write, not by it.
    """
    path.write_text("".join(f"{line}\n" for chunk in chunks for line in chunk), encoding="utf-8")
    path.chmod(0o600)


SITE = "//website:site"
UNIVERSE = (SITE, "//website:other")


def reading_of(path: Path, name: str) -> Reading:
    reading, findings = read_run(path, name)
    expect(findings == [], f"the {name} fixture should read clean, got {codes(findings)}")
    return reading


def expect(condition: bool, problem: str) -> None:
    """A loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not condition:
        raise SystemExit(f"bazel hermeticity proofs self-test: {problem}")


def landed(path: Path, needle: str) -> None:
    """Prove a mutation reached the disk before its result is read."""
    text = path.read_text(encoding="utf-8")
    expect(needle in text, f"the mutation did not land in {path.name}: {needle!r} absent")


# ---------------------------------------------------------------------------
# The proofs.
# ---------------------------------------------------------------------------


def prove_reader(scratch: Path) -> int:
    """The floors. Each one is a green that would otherwise be a green over nothing."""
    checks = 0
    good = scratch / "reader-good.json"
    write_bep(good, bep_lines(invalidated=(SITE,), universe=UNIVERSE))
    reading, findings = read_run(good, "good")
    expect(findings == [], f"a well-formed BEP must read clean, got {codes(findings)}")
    expect(reading.invalidated == frozenset({SITE}), f"invalidated={reading.invalidated}")
    expect(reading.universe == frozenset(UNIVERSE), f"universe={reading.universe}")
    expect(reading.attempts == 1, f"attempts={reading.attempts}")
    print(f"reader GREEN: 1 invalidated of a {len(reading.universe)}-label universe, 1 attempt, floors clear")
    checks += 1

    # The floor that matters most: without the flag, BEP publishes only failed
    # actions, so every label reads "not invalidated" and (a) and (c) green.
    flagless = scratch / "reader-noflag.json"
    write_bep(flagless, bep_lines(invalidated=(), universe=UNIVERSE, all_actions=False))
    expect(PUBLISH_ALL_ACTIONS not in flagless.read_text(encoding="utf-8"), "the flag mutation did not land")
    _, findings = read_run(flagless, "noflag")
    expect(codes(findings) == ["hermetic-bep-not-all-actions"], f"got {codes(findings)}")
    print(f"reader RED  : {codes(findings)[0]} — a BEP without the flag reads 0 invalidated of 2")
    checks += 1

    truncated = scratch / "reader-truncated.json"
    lines = bep_lines(invalidated=(SITE,), universe=UNIVERSE)
    truncated.write_text("".join(f"{line}\n" for line in lines[:-2]) + '{"id": {"prog', encoding="utf-8")
    landed(truncated, '{"id": {"prog')
    _, findings = read_run(truncated, "truncated")
    expect(
        set(codes(findings)) == {"hermetic-bep-malformed", "hermetic-bep-truncated"},
        f"got {codes(findings)}",
    )
    print(f"reader RED  : {codes(findings)} — a crashed build's partial BEP is not a clean tree")
    checks += 1

    # Two invocation streams in one file: a transparent retry after `Lost
    # inputs`. Normal input, not corruption — and the union is the reading.
    retried = scratch / "reader-retry.json"
    write_bep(
        retried,
        bep_lines(invalidated=(SITE,), universe=UNIVERSE, uuid="a" * 8 + "-1111-1111-1111-111111111111"),
        bep_lines(invalidated=("//website:other",), universe=UNIVERSE, uuid="b" * 8 + "-1111-1111-1111-111111111111"),
    )
    reading, findings = read_run(retried, "retry")
    expect(findings == [], f"two streams must read clean, got {codes(findings)}")
    expect(reading.attempts == 2, f"attempts={reading.attempts}")
    expect(reading.invalidated == frozenset(UNIVERSE), f"invalidated={reading.invalidated}")
    print(f"reader GREEN: two invocation ids in one file → attempts={reading.attempts}, sets unioned")
    checks += 1

    # And one stream missing the flag still reds, or the union is short.
    half = scratch / "reader-retry-half.json"
    write_bep(
        half,
        bep_lines(invalidated=(SITE,), universe=UNIVERSE),
        bep_lines(invalidated=(), universe=UNIVERSE, all_actions=False, uuid="c" * 8 + "-1111-1111-1111-111111111111"),
    )
    expect(half.read_text(encoding="utf-8").count(PUBLISH_ALL_ACTIONS) == 1, "the half-flag mutation did not land")
    _, findings = read_run(half, "retry-half")
    expect(codes(findings) == ["hermetic-bep-not-all-actions"], f"got {codes(findings)}")
    print(f"reader RED  : {codes(findings)[0]} — one stream of two without the flag under-reports")
    checks += 1
    return checks


def prove_declared(scratch: Path) -> int:
    """(a) S-014, both halves, plus the two controls that make it discriminate."""
    checks = 0
    unchanged = scratch / "a-unchanged.json"
    edited = scratch / "a-edited.json"
    control = scratch / "a-control.json"
    write_bep(unchanged, bep_lines(invalidated=(), universe=UNIVERSE, runners={"internal": 1}))
    write_bep(edited, bep_lines(invalidated=(SITE,), universe=UNIVERSE))
    write_bep(control, bep_lines(invalidated=(), universe=UNIVERSE, runners={"internal": 1}))

    findings = declared_input_findings(
        unchanged=reading_of(unchanged, "unchanged"),
        edited=reading_of(edited, "edited"),
        control=reading_of(control, "control"),
        label=SITE,
        control_hint="README.md",
    )
    expect(findings == [], f"the sound shape must be green, got {codes(findings)}")
    print("S-014 GREEN: edited→INVAL, unchanged→hit, control→hit — all three required")
    checks += 1

    # The S-014 red: the edited tree does not invalidate. Measured live on
    # //:site_undeclared, which serves its pre-edit output out of the disk
    # cache after `bazel clean`.
    stale = scratch / "a-edited-stale.json"
    write_bep(stale, bep_lines(invalidated=(), universe=UNIVERSE, runners={"internal": 1}))
    expect(
        '"actionCompleted"' not in stale.read_text(encoding="utf-8"),
        "the stale-edit mutation did not land: an action event is still present",
    )
    findings = declared_input_findings(
        unchanged=reading_of(unchanged, "unchanged"),
        edited=reading_of(stale, "edited"),
        control=reading_of(control, "control"),
        label=SITE,
        control_hint="README.md",
    )
    expect(codes(findings) == ["hermetic-declared-input-missing"], f"got {codes(findings)}")
    print(f"S-014 RED  : {codes(findings)[0]} — a doc_scripts edit that changes no action key")
    checks += 1

    # Control 1: a rule that rebuilds on everything. "It rebuilt" is then vacuous.
    always = scratch / "a-always.json"
    write_bep(always, bep_lines(invalidated=(SITE,), universe=UNIVERSE))
    landed(always, '"actionCompleted"')
    findings = declared_input_findings(
        unchanged=reading_of(always, "unchanged"),
        edited=reading_of(edited, "edited"),
        control=reading_of(always, "control"),
        label=SITE,
        control_hint="README.md",
    )
    expect(
        set(codes(findings)) == {"hermetic-always-rebuilds", "hermetic-overbroad-inputs"},
        f"got {codes(findings)}",
    )
    print(f"S-014 RED  : {codes(findings)} — the DX-18 shape: universal invalidation greens the edit")
    checks += 1

    # Control 2: the reader floor. A label outside the run's universe reads
    # "not invalidated" for free, which would be an S-014 green over a target
    # that was never in the build.
    findings = declared_input_findings(
        unchanged=reading_of(unchanged, "unchanged"),
        edited=reading_of(edited, "edited"),
        control=reading_of(control, "control"),
        label="//website:absent",
        control_hint="README.md",
    )
    expect(codes(findings) == ["hermetic-bep-absent-label"], f"got {codes(findings)}")
    print(f"S-014 RED  : {codes(findings)[0]} — 'not invalidated' is only a hit for a built target")
    checks += 1
    return checks


def prove_ambient(scratch: Path) -> int:
    """(b) Both legs, and the polarity stated as a runnable fact."""
    checks = 0
    touched = scratch / "b-touched.json"
    write_bep(touched, bep_lines(invalidated=(), universe=UNIVERSE, runners={"internal": 1}))
    hit = reading_of(touched, "touched")
    expect(ambient_cache_findings(touched=hit, label=SITE) == [], "a hit is the expected outcome")
    print(
        "ambient POLARITY: after touching an UNDECLARED file the expected outcome is a cache HIT. "
        "Measured on the probe: the touch invalidated NOTHING — not //:site and not //:site_ambient, "
        "which reads that very file. A check demanding a MISS greens on nothing and reds on nothing."
    )
    checks += 1

    invalidating = scratch / "b-touched-inval.json"
    write_bep(invalidating, bep_lines(invalidated=(SITE,), universe=UNIVERSE))
    landed(invalidating, '"actionCompleted"')
    findings = ambient_cache_findings(touched=reading_of(invalidating, "touched"), label=SITE)
    expect(codes(findings) == ["hermetic-ambient-declared"], f"got {codes(findings)}")
    print(f"ambient RED  : {codes(findings)[0]} — a MISS here is a SETUP error, never the finding")
    checks += 1

    cold = scratch / "b-cold.json"
    write_bep(cold, bep_lines(invalidated=(SITE,), universe=UNIVERSE, runners={"linux-sandbox": 2}))
    run = reading_of(cold, "cold")
    expect(run.reexecuted, "a run with no cache-hit runner must read re-executed")

    def reading(name: str, digest: str) -> OutputReading:
        return OutputReading(name, run, digest)

    stable = hashlib.sha256(b"site-v1").hexdigest()
    moved = hashlib.sha256(b"site-v2").hexdigest()
    drifting = hashlib.sha256(b"site-v3").hexdigest()

    findings = reproducibility_findings(
        rerun_a=reading("rerun-a", stable),
        rerun_b=reading("rerun-b", stable),
        after_touch=reading("after-touch", stable),
        label=SITE,
    )
    expect(findings == [], f"three equal digests must be green, got {codes(findings)}")
    print("ambient GREEN: two cold builds byte-identical, and still identical after the touch")
    checks += 1

    findings = reproducibility_findings(
        rerun_a=reading("rerun-a", stable),
        rerun_b=reading("rerun-b", drifting),
        after_touch=reading("after-touch", moved),
        label=SITE,
    )
    expect(codes(findings) == ["hermetic-nondeterministic-output"], f"got {codes(findings)}")
    print(f"ambient RED  : {codes(findings)[0]} — the date-stamp leg, two cold builds of one tree")
    checks += 1

    findings = reproducibility_findings(
        rerun_a=reading("rerun-a", stable),
        rerun_b=reading("rerun-b", stable),
        after_touch=reading("after-touch", moved),
        label=SITE,
    )
    expect(codes(findings) == ["hermetic-ambient-input"], f"got {codes(findings)}")
    print(f"ambient RED  : {codes(findings)[0]} — reproducible, yet it moved with an undeclared file")
    checks += 1

    # The floor without which both legs are vacuous: comparing two cache hits.
    warm = scratch / "b-warm.json"
    write_bep(warm, bep_lines(invalidated=(SITE,), universe=UNIVERSE, runners={"disk cache hit": 2}))
    landed(warm, "disk cache hit")
    cached = reading_of(warm, "warm")
    expect(not cached.reexecuted, "a disk-cache-hit runner must read not-re-executed")
    findings = reproducibility_findings(
        rerun_a=OutputReading("rerun-a", cached, stable),
        rerun_b=OutputReading("rerun-b", cached, stable),
        after_touch=OutputReading("after-touch", cached, stable),
        label=SITE,
    )
    expect(codes(findings) == ["hermetic-repro-not-reexecuted"], f"got {codes(findings)}")
    print(f"ambient RED  : {codes(findings)[0]} — a clock-reader greens if the builds were cache hits")
    checks += 1
    return checks


def prove_tool_pin(scratch: Path) -> int:
    """(c) Three states, three verdicts — and the constant reader that cannot tell them apart."""
    checks = 0
    inval = scratch / "c-inval.json"
    hit = scratch / "c-hit.json"
    write_bep(inval, bep_lines(invalidated=(SITE,), universe=UNIVERSE))
    write_bep(hit, bep_lines(invalidated=(), universe=UNIVERSE, runners={"internal": 1}))
    invalidated = reading_of(inval, "invalidated")
    cached = reading_of(hit, "cached")

    before = hashlib.sha256(b"uv 0.10.0").hexdigest()
    after = hashlib.sha256(b"uv 0.10.1").hexdigest()

    green = tool_pin_findings(
        baseline=OutputReading("baseline", cached, before),
        plain=OutputReading("plain", invalidated, after),
        forced=OutputReading("forced", invalidated, after),
        label=SITE,
    )
    expect(green == [], f"a participating, refetching pin must be green, got {codes(green)}")
    print("tool-pin GREEN: plain build after the bump invalidated and moved the output")

    stale = tool_pin_findings(
        baseline=OutputReading("baseline", cached, before),
        plain=OutputReading("plain", cached, before),
        forced=OutputReading("forced", invalidated, after),
        label=SITE,
    )
    expect(codes(stale) == ["hermetic-tool-repo-stale"], f"got {codes(stale)}")
    print(f"tool-pin RED  : {codes(stale)[0]} — WP-31's MEASURED state; the pin participates, the repo did not refetch")

    absent = tool_pin_findings(
        baseline=OutputReading("baseline", cached, before),
        plain=OutputReading("plain", cached, before),
        forced=OutputReading("forced", cached, before),
        label=SITE,
    )
    expect(codes(absent) == ["hermetic-tool-pin-absent"], f"got {codes(absent)}")
    print(f"tool-pin RED  : {codes(absent)[0]} — Block: the digest is not in the action key at all")
    checks += 3

    # The three states differ. The natural reader cannot see that they do.
    verdicts = [codes(green), codes(stale), codes(absent)]
    expect(len({tuple(v) for v in verdicts}) == 3, f"the three states must differ, got {verdicts}")
    action_keys = {"plain": "k0", "forced": "k0", "baseline": "k0"}
    control = [_action_key_reader(action_keys, "k0") for _ in verdicts]
    expect(control == [False, False, False], f"the actionKey control must be constant, got {control}")
    print(
        f"tool-pin CONTROL: _action_key_reader answers {control} across three states the real "
        "reader separates — Action.getKey() excludes input digests (WP-31, measured byte-identical)"
    )
    checks += 1

    # The static discriminator, which needs no build at all.
    marker = scratch / "tools.marker"
    marker.write_text(
        "REPO_RULE:@@+ocx+tools\nFILE:/repo/MODULE.bazel abc\nENV:PATH\n", encoding="utf-8"
    )
    findings = marker_watch_findings(
        marker.read_text(encoding="utf-8"), repo="@tools", pins=("ocx.toml", "ocx.lock")
    )
    expect(codes(findings) == ["hermetic-tool-marker-unwatched"], f"got {codes(findings)}")
    print(f"tool-pin RED  : {codes(findings)[0]} — no FILE: entry for ocx.toml/ocx.lock (rules_ocx today)")
    checks += 1

    marker.write_text(
        "REPO_RULE:@@+ocx+tools\nFILE:/repo/ocx.toml abc\nFILE:/repo/ocx.lock def\n", encoding="utf-8"
    )
    landed(marker, "ocx.lock")
    expect(
        marker_watch_findings(marker.read_text(encoding="utf-8"), repo="@tools", pins=("ocx.toml", "ocx.lock")) == [],
        "a marker watching both pins must be green",
    )
    print("tool-pin GREEN: a marker carrying FILE: entries for both pins — the ctx.watch shape")
    checks += 1

    empty = marker_watch_findings("REPO_RULE:@@+ocx+tools\n", repo="@tools", pins=("ocx.lock",))
    expect(codes(empty) == ["hermetic-tool-marker-unread"], f"got {codes(empty)}")
    print(f"tool-pin RED  : {codes(empty)[0]} — 0 FILE: entries is a reader that read nothing")
    checks += 1
    return checks


def prove_sandbox_waiver() -> int:
    """The PTY expectation, and what a `no-sandbox` tag actually costs."""
    checks = 0
    casts = {f"//test/doc_scripts:cast_{i}": {"no-sandbox", "requires-network", "local"} for i in range(3)}
    tags = {**casts, SITE: {"requires-network", "no-remote-cache"}}

    findings = sandbox_waiver_findings(
        tags=tags, reproducibility_checked=set(casts) | {SITE}, site_label=SITE
    )
    expect(findings == [], f"covered waivers must be green, got {codes(findings)}")
    print(
        f"sandbox GREEN: {len(casts)} no-sandbox waivers, all covered by check (b); the site rule "
        "carries requires-network only (C-023)"
    )
    checks += 1

    findings = sandbox_waiver_findings(
        tags=tags, reproducibility_checked={SITE}, site_label=SITE
    )
    expect(codes(findings) == ["hermetic-waiver-unchecked"], f"got {codes(findings)}")
    expect(len(findings) == len(casts), f"one finding per uncovered waiver, got {len(findings)}")
    print(f"sandbox RED  : {codes(findings)[0]} ×{len(findings)} — a waiver nothing looks at")
    checks += 1

    leaked = {**tags, SITE: {"requires-network", "no-sandbox"}}
    expect(leaked[SITE] != tags[SITE], "the site-tag mutation did not land")
    findings = sandbox_waiver_findings(
        tags=leaked, reproducibility_checked=set(casts) | {SITE}, site_label=SITE
    )
    expect(codes(findings) == ["hermetic-site-no-sandbox"], f"got {codes(findings)}")
    print(f"sandbox RED  : {codes(findings)[0]} — C-023 gives the site rule requires-network only")
    checks += 1

    findings = sandbox_waiver_findings(tags={}, reproducibility_checked=set(), site_label=SITE)
    expect(codes(findings) == ["hermetic-waiver-reader"], f"got {codes(findings)}")
    print(f"sandbox RED  : {codes(findings)[0]} — an empty tag table is a green over nothing read")
    checks += 1

    expect(PTY_SURVIVES_SANDBOX is True, "PTY_SURVIVES_SANDBOX must record the measurement")
    expect(SANDBOX_BLOCKS_ABSOLUTE_READS is False, "the sandbox does not block absolute-path reads")
    print(
        "sandbox MEASURED: pty.openpty() under linux-sandbox → /dev/pts/*, exit 0. C-022's stated "
        "reason for no-sandbox on the 39 cast targets is REFUTED on bazel 9.2.0 / Linux 6.18 WSL2; "
        "and a sandboxed genrule reads the source tree by absolute path anyway, so the tag removes "
        "no ambient protection either way"
    )
    checks += 1
    return checks


def prove_c029(scratch: Path) -> int:
    """C-029 — nothing here can reach the owner-gated remote realm."""
    checks = 0
    source = Path(__file__).read_text(encoding="utf-8")
    hits = env_reads(source)
    expect(hits == [], f"this file reads the environment: {hits}")
    print("C-029 GREEN: no environment read in this file (AST, not grep — a grep matches its own prose)")
    checks += 1

    mutated = scratch / "mutated_hermeticity.py"
    mutated.write_text(
        source + "\nimport os\n\n\ndef _leak() -> object:\n    return os.getenv('BAZEL_CACHE_READ_AUTH')\n",
        encoding="utf-8",
    )
    landed(mutated, "os.getenv('BAZEL_CACHE_READ_AUTH')")
    found = env_reads(mutated.read_text(encoding="utf-8"))
    expect(found == ["import os", "os.getenv"], f"the mutated copy must red on both, got {found}")
    print(f"C-029 RED  : {found} — an added credential read is visible to the AST check")
    checks += 1

    rc = probe_rc(scratch / "disk", scratch / "out", scratch / "repo")
    argv = " ".join(probe_argv(Path("bazel"), "build", "//:site", PUBLISH_ALL_ACTIONS))
    expect(realm_findings({".bazelrc": rc, "argv": argv}) == [], "the probe must carry no remote flag")
    print("C-029 GREEN: the probe rc and argv carry no remote-cache flag and no credential name")
    checks += 1

    poisoned = rc + "build --remote_cache=https://bazel-cache.ocx.sh/v1\n"
    expect("remote_cache" in poisoned and "remote_cache" not in rc, "the rc mutation did not land")
    findings = realm_findings({".bazelrc": poisoned})
    expect(codes(findings) == ["realm-flag"], f"got {codes(findings)}")
    print(f"C-029 RED  : {codes(findings)[0]} — a remote flag in the probe rc is refused before it spawns")
    checks += 1
    return checks


def prove_counts() -> int:
    """The probe's tables, checked against each other rather than trusted flat."""
    expect(len(PROBE_RUNS) == 7, f"PROBE_RUNS has {len(PROBE_RUNS)} columns, the measurement has 7")
    for name, row in PROBE_EXPECTED.items():
        expect(len(row) == len(PROBE_RUNS), f"{name} has {len(row)} cells for {len(PROBE_RUNS)} runs")
        expect(row[0] is True, f"{name} must be invalidated on the cold run")
        expect(row[1] is False, f"{name} must be a hit on an unchanged tree")
        expect(row[3] is True, f"{name} reads invalidated after `bazel clean` — there is no action cache")
    expect(set(PROBE_EXPECTED) == set(CACHE_TARGETS), "the table and the target list disagree")
    expect(set(REPRO_EXPECTED) <= set(CACHE_TARGETS), "a repro target is not built by the probe")
    expect(set(PROBE_SHAPES) == set(CACHE_TARGETS), "every probe target needs its shape written down")
    expect(
        FRESH_SERVER_DISK_HITS == len(CACHE_TARGETS),
        f"the fresh-server floor is {FRESH_SERVER_DISK_HITS}, the probe builds {len(CACHE_TARGETS)}",
    )
    # The three unsound targets must each red exactly one of the three checks,
    # or the probe carries a shape nothing reads.
    expect(PROBE_EXPECTED["site"][2] is True, "//:site must invalidate on the declared edit")
    expect(PROBE_EXPECTED["site_undeclared"][2] is False, "//:site_undeclared is the S-014 red")
    expect(PROBE_EXPECTED["site_lazy"][5] is True, "//:site_lazy is (c)'s positive control")
    expect(PROBE_EXPECTED["site_eager"][5] is False, "//:site_eager must be stale on a plain build")
    expect(PROBE_EXPECTED["site_eager"][6] is True, "//:site_eager must move on a forced refetch")
    print(
        f"counts  OK : {len(CACHE_TARGETS)} probe targets × {len(PROBE_RUNS)} runs, "
        f"{len(REPRO_EXPECTED)} reproducibility rows, fresh-server floor {FRESH_SERVER_DISK_HITS} "
        "— internal consistency only; --prove-hermeticity is the reality check"
    )
    return 1


# ---------------------------------------------------------------------------
# The three check modes WP-34 drives.
# ---------------------------------------------------------------------------


def _split_pair(value: str, name: str) -> OutputReading:
    """`BEP.json:OUTPUT` — one build and the artifact it left behind."""
    bep, _, output = value.rpartition(":")
    if not bep or not output:
        raise SystemExit(f"--{name} wants BEP.json:OUTPUT, got {value!r}")
    reading, findings = read_run(Path(bep), name)
    if findings:
        raise SystemExit(report(findings))
    return OutputReading(name, reading, digest_of(Path(output)))


def run_check_declared(*, label: str, unchanged: Path, edited: Path, control: Path) -> int:
    findings: list[Finding] = []
    readings = {}
    for name, path in (("unchanged", unchanged), ("edited", edited), ("control", control)):
        reading, parse = read_run(path, name)
        findings.extend(parse)
        readings[name] = reading
    if findings:
        return report(findings)
    return report(declared_input_findings(label=label, control_hint="the control file", **readings))


def run_check_ambient(*, label: str, touched: Path, rerun_a: str, rerun_b: str, after_touch: str) -> int:
    reading, findings = read_run(touched, "touched")
    findings.extend(ambient_cache_findings(touched=reading, label=label))
    findings.extend(
        reproducibility_findings(
            rerun_a=_split_pair(rerun_a, "rerun-a"),
            rerun_b=_split_pair(rerun_b, "rerun-b"),
            after_touch=_split_pair(after_touch, "after-touch"),
            label=label,
        )
    )
    return report(findings)


def run_check_tool_pin(*, label: str, baseline: str, plain: str, forced: str, marker: Path | None) -> int:
    findings = tool_pin_findings(
        baseline=_split_pair(baseline, "baseline"),
        plain=_split_pair(plain, "plain"),
        forced=_split_pair(forced, "forced"),
        label=label,
    )
    if marker is not None:
        try:
            text = marker.read_text(encoding="utf-8")
        except OSError as error:
            findings.append(Finding("hermetic-tool-marker-unread", f"cannot read {marker}: {error}"))
        else:
            findings.extend(marker_watch_findings(text, repo="@tools", pins=("ocx.toml", "ocx.lock")))
    return report(findings)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--prove-hermeticity", action="store_true", help="the live probe")
    mode.add_argument("--check-declared", action="store_true", help="(a) S-014, three BEP readings")
    mode.add_argument("--check-ambient", action="store_true", help="(b) isolation, one BEP + three pairs")
    mode.add_argument("--check-tool-pin", action="store_true", help="(c) participation, three pairs")
    parser.add_argument("--probe-root", type=Path, default=PROBE_DEFAULT_ROOT)
    parser.add_argument("--label", help="the target under test")
    for name in ("unchanged", "edited", "control", "touched", "marker"):
        parser.add_argument(f"--{name}", type=Path)
    for name in ("rerun-a", "rerun-b", "after-touch", "baseline", "plain", "forced"):
        parser.add_argument(f"--{name}", help="BEP.json:OUTPUT")
    args = parser.parse_args()

    if args.prove_hermeticity:
        return run_prove_hermeticity(args.probe_root)

    if not args.label:
        parser.error("every --check-* mode needs --label")
    if args.check_declared:
        if not all((args.unchanged, args.edited, args.control)):
            parser.error("--check-declared needs --unchanged, --edited and --control")
        return run_check_declared(
            label=args.label, unchanged=args.unchanged, edited=args.edited, control=args.control
        )
    if args.check_ambient:
        if not all((args.touched, args.rerun_a, args.rerun_b, args.after_touch)):
            parser.error("--check-ambient needs --touched, --rerun-a, --rerun-b and --after-touch")
        return run_check_ambient(
            label=args.label,
            touched=args.touched,
            rerun_a=args.rerun_a,
            rerun_b=args.rerun_b,
            after_touch=args.after_touch,
        )
    if not all((args.baseline, args.plain, args.forced)):
        parser.error("--check-tool-pin needs --baseline, --plain and --forced")
    return run_check_tool_pin(
        label=args.label,
        baseline=args.baseline,
        plain=args.plain,
        forced=args.forced,
        marker=args.marker,
    )


if __name__ == "__main__":
    raise SystemExit(main())
