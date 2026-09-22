#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Stage-1 Bazel edge cases — the red states WP-14/15/16 and A1 are written against.

    scripts/bazel_gate_proofs.py --self-test
    scripts/bazel_gate_proofs.py --a1 --warm W.json --leaf L.json --hub H.json \
        --rdeps-leaf rdeps_leaf.txt --rdeps-hub rdeps_hub.txt

This file lands **before** the things it tests. At WP-13 there is no
`MODULE.bazel`, no `.bazelrc`, no `crates/*/BUILD.bazel` and no gate script, so
a harness that could only run against the live tree would be a harness nobody
can red today. Everything here is therefore split in two:

* **Comparators** — pure functions over already-read inputs. These are the
  gates' actual logic and the thing red-proven below. WP-14 (`bazel:pin:check`),
  WP-15 (`bazel:build:drift`) and WP-16 (`bazel:tag:guard`) own the I/O — the
  `bazel query`, the `cargo metadata --locked`, the `bazel --version` — and
  call in here for the verdict, so the check their implementation is written
  against is the one already watched go red.
* **`--a1`** — the one mode with a live subject of its own: A1's leaf-vs-hub
  discriminator, read out of two BEP files. Nothing else owns it; WP-12 builds
  the graph, this decides whether that graph is per-crate.

**A1's discriminator, and why it exists.** `bazel test //crates/...` twice, with
run 2 reporting everything cached, is consistent with two opposite worlds: a
graph that really skips per crate, and a graph that invalidates universally but
was handed an unchanged tree. The summary line cannot tell them apart. So both
halves are required (ADR § A1): touch a **leaf** (`ocx_exit`) and the re-run set
must be inside `bazel query 'rdeps(//crates/..., //crates/ocx_exit:ocx_exit)'`;
touch a **hub** (`ocx_util`) and a large set must re-run. **If the two re-run
sets are equal, the graph is not per-crate and A1 is NOT MET**, whatever run 2
reported.

**And the ADR picked the wrong two crates.** Over the repository's own
allowed-edge table (`scripts/crate_map.toml`), `rdeps(ocx_exit)` and
`rdeps(ocx_util)` are the same 16 crates — 17 of 20 each, jointly the largest
reverse closures in the workspace. `ocx_exit` is a leaf in the *dependency*
direction and a hub in the *reverse* one, which is the direction this test
reads, so the ADR's pair differs by exactly one target each and discriminates
nothing. `a1_verdict` refuses that shape rather than report MET on a one-label
margin, and names the small probes: `ocx_announce`, `ocx_script` and
`ocx_setup` re-run 3 of 20.

**Evidence is the BEP, never the terminal summary.** `--build_event_json_file`
emits one JSON object per line; a test target's verdict lives in a `TestResult`
event. The reader below asks for **field presence and truth**, not for a schema:
BEP carries no published stability guarantee, and proto3 JSON *omits* a false
boolean, so `cachedLocally` is simply absent on a target that ran. A reader
written as `payload.get("cachedLocally") is not False` therefore reports every
target cached on a build where nothing was — a green indistinguishable from the
check never having run. `--self-test` ships that wrong reader as a named control
and shows it green on the same bytes where the real one reports 33 re-runs.

**C-029.** No proof here reaches the remote cache realm, which is owner-gated.
That is asserted structurally rather than promised: `--self-test` parses this
file's own AST and reds on any `subprocess`/`socket`/`urllib`/`http`/`ssl`
import or any `os.environ` / `os.getenv` read. AST and not `grep`, because a
grep would match the banned spellings in this docstring and in the check's own
table — a detector that matches its own text answers the same in every state.

**Every mutation below is proven to have landed** before its result is trusted;
until then a surviving green is *unexplained*, not excused. Fixtures are
synthetic and live under the repo's own `.tmp/` (never `/tmp`), and nothing is
ever restored with `git checkout --`, which restores from the index and would
make the whole run vacuous.

Not wired into `taskfiles/scripts.taskfile.yml` — WP-17 owns that file. The one
line it owes the `self-test:` list is named in `--self-test`'s closing output.
"""

from __future__ import annotations

import argparse
import ast
import dataclasses
import json
import re
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------------------
# Counts.
#
# **Measured on the built graph, not derived from the ADR.** M-02 derived these
# downward from a sentence — "`ocx_shim` declares `[[bin]]` only, has no
# `src/lib.rs` and no `#[cfg(test)]`" — and three of the five numbers came out
# wrong. WP-12's BUILD files are the subject, `bazel query` is the instrument,
# and every value below is the answer one query gave on this tree:
#
#   kind(rust_library, //crates/...)          19
#   kind(rust_test, //crates/...)             34
#   kind(rust_binary, //crates/...)            0   (stages 1-2 build none)
#   kind("^rust_.*rule$", //crates/...)       53
#   kind(filegroup, //crates/...)              3
#   kind(rule, //crates/...)                  56
#   kind(rule, //crates/...) --output=package 20
#
# They stay written as sums, not literals, so a later correction to one summand
# cannot quietly leave a floor at the old total.
#
# These are *this file's* floors. They are not a substitute for WP-12's
# generated table: the plan's rule is that every gate floors on a generated
# table rather than a constant, so WP-15 and WP-16 must assert their reader
# counts against `crates/TEST_TARGET_MAP.toml` **and** against these — a
# disagreement between the two is the finding.
# ---------------------------------------------------------------------------

CRATE_PACKAGES = 20
"""`crates/*/BUILD.bazel`: one per workspace member, `ocx_shim` included.

M-02 subtracted `ocx_shim` on the ground that it builds no target.
`crates/ocx_shim/src/main.rs` opens a `#[cfg(test)] mod tests` holding 109
`#[test]` fns, and rules_rust compiles those as a `rust_test` over the `[[bin]]`
crate root without needing the `rust_binary` — so the package is there and so is
`//crates/ocx_shim:ocx_shim_bin_test`."""

EXTERNAL_PACKAGES = 0
"""`external/` contributes nothing to any universe a gate reads.

Two independent reasons, both measured. `external` is Bazel's reserved
workspace-root directory name, so `//...` never expands into it (the
`.bazelignore` comment records the same probe). And no first-party target
reaches one: the patched submodule crates arrive through crate_universe as
ordinary `@crates//oci-client-0.17.0:oci-client-0.17.0` aliases, not as
`//external/rust-oci-client:oci_client`. The three `external/*/BUILD.bazel`
that do exist on disk are crate_universe output, untracked, and depended on by
nothing."""

CRATES_LIB_TARGETS = 19
"""One `rust_library` per crate that has a lib — every member but `ocx_shim`."""

CRATES_TEST_TARGETS = 34
"""19 lib unit-test targets + 13 `crates/<name>/tests/*.rs` integration targets
+ 2 `[[bin]]` unit-test targets (`ocx_cli:ocx_cli_bin_test`,
`ocx_shim:ocx_shim_bin_test`).

M-02's 33 was 19 + 14 and wrong twice over in ways that nearly cancelled: there
are 14 integration *suites* but only 13 Bazel targets — `ocx::linux_self_contained`
needs `env!("CARGO_BIN_EXE_ocx")`, which needs a `rust_binary` in `data` — and
the two `[[bin]]` unit-test targets it did not expect make up the difference and
one more."""

CRATES_FILEGROUP_TARGETS = 3
"""`ocx_cli:api_data`, `ocx_index:index_wire_fixtures`, `ocx_test_support:data`
— fixture trees named as `data`/`compile_data` by a sibling target."""

CRATES_RULE_TARGETS = CRATES_LIB_TARGETS + CRATES_TEST_TARGETS  # 53
"""Every **Rust** rule target under `//crates/...` — the stage-1 tag-guard
floor, which reads `kind("^rust_.*rule$", ...)` and so does not see filegroups."""

DRIFT_PACKAGE_FLOOR = CRATE_PACKAGES + EXTERNAL_PACKAGES  # 20
DRIFT_TARGET_FLOOR = CRATES_RULE_TARGETS + CRATES_FILEGROUP_TARGETS  # 56
"""`bazel:build:drift` reads `kind(rule, //crates/...)`, which is every rule
target of every kind, so its target floor counts the filegroups too. The
package floor keeps `+ EXTERNAL_PACKAGES` rather than dropping the term: the
day an `external/` package becomes reachable, the floor moves with it."""

CAST_GENRULE_TARGETS = 40
"""`//test/doc_scripts`: 39 cast recordings + `:manifest_drift`.

Every one of them carries `no-sandbox` — measured,
`bazel query 'attr(tags, "no-sandbox", kind(rule, //...))'` answers 40 labels
and all 40 are in this package. They are the **only** `no-sandbox` targets in
the graph, which is why the tag guard's universe has to reach them: a floor that
stops at `//crates/...` leaves clause 1 with no subject anywhere it looks."""

CAST_SUPPORT_TARGETS = 5
"""`:cast_runner`, `:drift_probe`, `:manifest_expected` (all `written_file`) and
the `:casts` and `:doc_script_sources` filegroups. No tag, but they are rule
targets and `kind(rule, ...)` counts them, so the floor has to as well."""

CAST_RULE_TARGETS = CAST_GENRULE_TARGETS + CAST_SUPPORT_TARGETS  # 45

GIF_GENRULE_TARGETS = 39
"""One `gif_<script>` render per cast recording — WP-33b's `agg` pass.

Deliberately not tagged `no-sandbox`: measured,
`bazel query 'attr(tags, "no-sandbox", kind(rule, //...))'` still answers the
same 40 labels it did before these landed, and none of them is a `gif_*`. So
these 39 widen the *floor* and add nothing to clause 1's subject."""

GIF_SUPPORT_TARGETS = 3
"""`:gifs` (filegroup), `:gif_check_probe` (`written_file`) and `:gif_check`
(`sh_test`) — the collection and its drift probe."""

GIF_RULE_TARGETS = GIF_GENRULE_TARGETS + GIF_SUPPORT_TARGETS  # 42
"""What WP-33b added to `//test/doc_scripts/...`, taking that package from 45
rule targets to 87. The tag guard printed `143 rule targets read` against a
floor of 101 and passed: 42 of headroom is a floor that has stopped
discriminating, which is the state this constant exists to close."""

ACCEPTANCE_MODULE_TARGETS = 181
"""One `sh_test` per `test/tests/test_*.py` (C-024) — `bazel query
'kind(sh_test, //test:all)'` answers 181 and `ls test/tests/test_*.py` answers
181. The single home for that number: `bazel_accept_proofs.ACCEPTANCE_MODULES`
is an alias of this, and `test/BUILD.bazel` states no count at all — the `glob`
is the enumeration. Three spellings of one fact is how the tree carried 172,
181 and "188" simultaneously."""

ACCEPTANCE_SUPPORT_TARGETS = 4
"""`:acceptance_runner` (`_acceptance_runner`) and the three filegroups
`:suite_anchor`, `:suite_inputs` and `:recording_inputs`. No `sh_test`, but
`kind(rule, ...)` counts them, so the floor has to."""

ACCEPTANCE_RULE_TARGETS = ACCEPTANCE_MODULE_TARGETS + ACCEPTANCE_SUPPORT_TARGETS  # 185

GRAPH_TAIL_TARGETS = 4
"""What `//...` holds that no earlier stage's universe names: `//:all` (2 —
`buildifier` and `buildifier.check`) and `//website/...` (2). Measured:
`bazel query 'kind(rule, //...)'` answers 331 today and 56 + 45 + 42 + 184 + 4
is 331, with `:suite_inputs` taking the acceptance package to 185 and the
total to 332."""

# ---------------------------------------------------------------------------
# Two traps this graph has already sprung. Recorded here because both are
# invisible in a diff and both produce a GREEN result while doing less.
#
# 1. **`--skip` is substring-matched.** libtest's `--skip=NAME` filters every
#    testcase whose path *contains* NAME, not the one named. WP-12 measured it:
#    22 skip names in `//crates/ocx_test_support:workspace_structure` filtered
#    29 of that target's 35 testcases, so 13 executed became 6 — and the target
#    still passed, so nothing said a word. `--exact` restores name equality, and
#    every `--skip` WP-12 committed is paired with it. A skip added without
#    `--exact` silently widens, and `crates/TEST_TARGET_MAP.toml` (counts rise
#    only) is the one thing that would catch it afterwards.
#
# 2. **A `@crates//` label carries name AND version, in both halves.** The real
#    spelling is `@crates//<name>-<version>:<name>-<version>` —
#    `@crates//rustls-pki-types-1.15.1:rustls-pki-types-1.15.1`, not
#    `@crates//:rustls-pki-types` and never `@crates//:pki-types`. The `name`
#    is the **crate package name**, so WP-15's mapping table keys on it
#    directly and the Cargo-rename trap the plan recorded dissolves: this
#    workspace renames `rustls-pki-types` to `pki-types` in the root
#    `Cargo.toml`, and that rename appears nowhere in any Bazel label. What is
#    still owed is the mirror-image case on the FIRST-PARTY side, where the
#    rename runs the other way: `//crates/ocx_cli:ocx_cli` is cargo package
#    `ocx` (`crates/ocx_cli/Cargo.toml` names the package `ocx`, and the BUILD
#    file carries `crate_name = "ocx"`). `FIRST_PARTY_LABEL` below would answer
#    `ocx_cli`, so that one row must be in `label_map`, which `resolve_label`
#    consults first.
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Messages. Verbatim from the ADR's gate contracts where it gives one, so a
# reviewer can diff them; the two deliberate departures are marked.
# ---------------------------------------------------------------------------

PIN_DRIFT_MSG = (
    "bazel pin drift: .bazelversion says {pinned}, the resolved toolchain binary is "
    "{running} — ocx.lock is the pin authority; bump .bazelversion or relock"
)
PIN_FILE_MSG = ".bazelversion must be an exact three-component semver (BZL-FLAG-01), got: {text}"
PIN_READER_MSG = "bazel pin check could not read a version from the toolchain binary"

DRIFT_SET_MSG = "BUILD drift: {pkg} — in Cargo not in BUILD: {missing}; in BUILD not in Cargo: {extra}"
DRIFT_UNMAPPED_MSG = "BUILD drift: unmapped external label {label} — add a row to the mapping table"
DRIFT_FLOOR_MSG = (
    "BUILD drift check read {packages} packages / {targets} targets, expected "
    "{package_floor} / {target_floor} — the reader stopped early"
)
DRIFT_MAP_PARITY_MSG = (
    "BUILD drift: read {targets} crates/ rust_test targets, TEST_TARGET_MAP has {rows} rows"
)

#: Deliberately does not name the tag: which one is credited depends on whether
#: the action is a *test* action or a *build* action, and the two answers were
#: measured separately (`external` stops a test result being reused; no tag makes
#: a build action re-execute, so for those the credited tag is the one that keeps
#: an unsound result off other machines). `bazel_tag_guard.py` decides the kind
#: and emits `tag-cache-insufficient` beside this finding with the tag and the
#: measurement behind it. Naming one tag here is how this message spent a wave
#: instructing a fix that had been measured not to fix it.
TAG_NO_SANDBOX_MSG = (
    "bazel tag guard: {label} is no-sandbox without the cache-excluding tag its kind "
    "needs — a cache-key-unsound target (BZL-CACHE-34). The `tag-cache-insufficient` "
    "finding beside this one names the tag and the measurement it comes from"
)
#: Departs from the ADR, which wrote `--nostamp`. The plan's [R1] correction:
#: `--nostamp` is a *global command-line flag* no target can carry, and it
#: already defaults false, so asserting it is vacuous. The per-target escapes
#: are the `no-remote-cache` **tag** and `stamp = 0` as a **rule attribute** —
#: both of which name a label, which is exactly what a global flag cannot do.
TAG_STAMP_MSG = (
    "bazel tag guard: {label} is a rust_binary on the cacheable path without "
    "stamp = 0 or no-remote-cache (BZL-CACHE-20)"
)
TAG_FLOOR_MSG = (
    "bazel tag guard read {read} of an expected >= {minimum} rule targets in {universe} — "
    "the reader stopped early"
)
TAG_STAGE_MSG = (
    "bazel tag guard: no reader floor is declared for stage {stage!r} — a floorless run "
    "cannot fail, so it is refused. Declare the stage's universe and count in STAGE_FLOORS"
)

A1_FLOOR_MSG = (
    "A1: the {run} BEP yielded {read} test targets, expected >= {minimum} — the reader "
    "stopped early, or the build did not run the stage-1 universe"
)
A1_WARM_MSG = "A1: run 2 changed nothing yet {count} target(s) re-ran, first: {sample} — not a warm cache"
A1_EMPTY_MSG = (
    "A1: touching the {which} crate re-ran nothing. Either the touch did not land or the "
    "reader saw no re-runs; both make the discriminator below vacuous"
)
A1_WIDE_MSG = (
    "A1: the {which} re-run set is larger than the graph says — {extra} re-ran but is not "
    "in `bazel query 'rdeps(//crates/..., <{which}>)'`. The graph is wrong"
)
A1_SAME_MSG = (
    "A1 NOT MET: touching the leaf and touching the hub produced the same {count} re-run "
    "target(s). The graph is not per-crate — a universal cache hit and a universally "
    "invalidating graph are indistinguishable from run 2's summary line, which is why "
    "this comparison exists"
)
#: Measured, and it refutes the ADR's own choice of probes. Over the repository's
#: allowed-edge table (`scripts/crate_map.toml`), the transitive reverse closures
#: of `ocx_exit` and `ocx_util` are **the same 16 crates** — both probes re-run
#: 17 of 20, and the two re-run sets then differ by exactly one target each: the
#: probe's own. `ocx_exit` is a leaf in the dependency direction and the joint
#: *most*-depended-on crate in the reverse one, which is the direction this test
#: reads. A discriminator run on two interchangeable probes has no power in the
#: informative direction, so it is refused rather than reported as MET on a
#: one-label margin.
A1_ALIKE_MSG = (
    "A1: the leaf and hub re-run sets differ only by {delta} target(s) — no more than the "
    "two touched crates' own. Two causes, and they need different fixes. (1) The probe did "
    "not change EMITTED code: Bazel prunes on output identity, so an unused `pub const` "
    "appended to a lib.rs is dead-stripped, every dependent's test binary comes out "
    "byte-identical and its action is a cache hit — the graph can be perfectly per-crate "
    "and still answer this way. Check the probe first: touch something monomorphised or "
    "called into every consumer, not a dead item. (2) The probes are interchangeable, so "
    "this run cannot tell a per-crate graph from a universally invalidating one whichever "
    "way it comes out. Measured on scripts/crate_map.toml: rdeps(ocx_exit) and "
    "rdeps(ocx_util) are the same 16 crates, so the ADR's own pair is this case. Pick a "
    "probe with a small reverse closure — ocx_announce, ocx_script and ocx_setup re-run 3 "
    "of 20 — against ocx_util's 17"
)

SEMVER = re.compile(r"^\d+\.\d+\.\d+$")
#: Measured, not assumed: the pinned binary
#: (`~/.ocx/symlinks/ocx.sh/bazelbuild/bazel/candidates/9.2.0/content/bazel`)
#: prints exactly `bazel 9.2.0` on one line, rc 0. A development build prints
#: `bazel no_version`, which must reach the reader floor rather than compare.
BAZEL_VERSION_LINE = re.compile(r"^bazel (\d+\.\d+\.\d+)\b", re.MULTILINE)

#: `//crates/<name>:<name>` is the first-party convention and needs no mapping
#: row — with one exception, `//crates/ocx_cli:ocx_cli`, whose cargo package is
#: `ocx`; that row belongs in `label_map`, which `resolve_label` reads first.
#: Everything else is a `@crates//<name>-<version>:<name>-<version>` label and
#: must be declared, or it is an unmapped label and its own exit 1. There are no
#: `//external/...` edges to declare: the patched submodule crates reach
#: first-party targets as ordinary `@crates//` aliases (trap 2 above).
FIRST_PARTY_LABEL = re.compile(r"^//crates/([a-z0-9_]+):\1$")


@dataclasses.dataclass(frozen=True)
class Finding:
    """One exit-1 reason. `code` is what tests assert on; `message` is stderr.

    Separate on purpose: asserting a substring of an English sentence is one of
    the cheapest ways to write an assertion that also matches the opposite
    outcome.
    """

    code: str
    message: str


@dataclasses.dataclass(frozen=True)
class StageFloor:
    universe: str
    minimum: int


#: Stage-scoped and advancing, per the plan's [R1] correction to the ADR's flat
#: 274: a `//...` floor at wave 5 would demand targets that arrive in wave 10.
#: C-011 names the sequence — `//crates/...`, then `+//test/doc_scripts/...`,
#: then `//...` — and an undeclared stage is refused rather than run floorless,
#: which is what `stage-5` (a name no wave has) is used for in both self-tests.
#: Stage 4 was held back until WP-36's acceptance count existed; it does now
#: (181 modules, measured), and C-011's `>= 272` predates the 42 GIF targets and
#: the shared input group, so the declared number is the sum below and not it.
#:
#: `minimum` is **every rule target of every kind**, not the Rust subset. It was
#: the Rust subset while stage 1 was the only stage, and that reading does not
#: survive stage 3: `//test/doc_scripts/...` contains 0 Rust rule targets and all
#: 40 of the graph's `no-sandbox` targets, so a Rust-only floor greens over the
#: whole universe clause 1 exists to read. C-011's own numbers (`//...` >= 272)
#: were always all-kind counts.
STAGE_FLOORS: dict[str, StageFloor] = {
    "stage-1": StageFloor(universe="//crates/...", minimum=DRIFT_TARGET_FLOOR),
    "stage-3": StageFloor(
        universe="//crates/... + //test/doc_scripts/...",
        minimum=DRIFT_TARGET_FLOOR + CAST_RULE_TARGETS + GIF_RULE_TARGETS,
    ),
    # Stage 4, declared now that WP-36's count exists to declare it with. The
    # acceptance targets have to be in the guard's universe or the `no-sandbox`
    # exemption `bazel_tag_guard.py` grants them is a branch nothing can reach
    # — a clause whose red state is unreachable, which is the thing this whole
    # corpus is written against. C-011 wrote `//...` >= 272 against a graph that
    # had not grown the 42 GIF targets or the shared input group yet.
    "stage-4": StageFloor(
        universe="//...",
        minimum=(
            DRIFT_TARGET_FLOOR
            + CAST_RULE_TARGETS
            + GIF_RULE_TARGETS
            + ACCEPTANCE_RULE_TARGETS
            + GRAPH_TAIL_TARGETS
        ),
    ),
}


def codes(findings: list[Finding]) -> list[str]:
    return sorted({finding.code for finding in findings})


def report(findings: list[Finding]) -> int:
    """Exit 0 and silent, or exit 1 with one line per finding."""
    # stdout is block-buffered when piped and stderr is not, so without this
    # the findings land above the self-test lines that introduce them.
    sys.stdout.flush()
    for finding in findings:
        print(finding.message, file=sys.stderr)
    return 1 if findings else 0


# ---------------------------------------------------------------------------
# S-009 — pin drift (C-003). WP-14 supplies the two readings.
# ---------------------------------------------------------------------------


def read_bazelversion(path: Path) -> str | None:
    """`None` means absent — distinct from present-and-empty, which is a red too."""
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None


def pin_drift(
    bazelversion_text: str | None,
    version_stdout: str | None,
    version_rc: int | None,
) -> list[Finding]:
    """C-003's three exit-1 cases, plus the reader floor that makes them honest.

    The floor is the whole point: a run that read *zero* version sources must
    red, never pass. Absent file and absent binary each produce their own
    finding with its own code, so "both missing" is two loud reasons rather
    than one quiet agreement between two `None`s.
    """
    findings: list[Finding] = []

    pinned: str | None = None
    if bazelversion_text is None:
        findings.append(Finding("pin-file", PIN_FILE_MSG.format(text="<absent>")))
    else:
        stripped = bazelversion_text.strip()
        if SEMVER.match(stripped):
            pinned = stripped
        else:
            findings.append(Finding("pin-file", PIN_FILE_MSG.format(text=stripped or "<empty>")))

    running: str | None = None
    if version_rc != 0 or not version_stdout:
        findings.append(Finding("pin-reader-floor", PIN_READER_MSG))
    else:
        match = BAZEL_VERSION_LINE.search(version_stdout)
        if match is None:
            # `bazel no_version` lands here. Unparseable is not agreement.
            findings.append(Finding("pin-reader-floor", PIN_READER_MSG))
        else:
            running = match.group(1)

    if pinned is not None and running is not None and pinned != running:
        findings.append(Finding("pin-drift", PIN_DRIFT_MSG.format(pinned=pinned, running=running)))
    return findings


# ---------------------------------------------------------------------------
# S-010 — BUILD drift (C-010). WP-15 supplies the two dep tables.
# ---------------------------------------------------------------------------


def resolve_label(label: str, label_map: dict[str, str]) -> str | None:
    """Bazel label -> cargo package name. `None` means no row matched."""
    if label in label_map:
        return label_map[label]
    match = FIRST_PARTY_LABEL.match(label)
    return match.group(1) if match else None


def build_drift(
    *,
    cargo: dict[str, set[str]],
    build: dict[str, set[str]],
    label_map: dict[str, str],
    packages_read: int,
    targets_read: int,
    rust_test_targets_read: int,
    test_map_rows: int,
) -> list[Finding]:
    """The **full** edge set per package, third-party edges included.

    Keyword-only because five of the arguments are bare ints: a positional call
    that transposed two of them would compare the wrong things and stay green.

    `packages_read` / `targets_read` are the reader's own counts, deliberately
    *not* derived from the two dicts. A reader that collapsed 55 targets into 22
    packages still has to report 55, so the floor stays a statement about how
    much was read rather than about how much survived the collapse.

    An unmapped third-party label is its own exit 1 and is excluded from the
    comparison — skipping it silently is precisely what makes the widened scope
    vacuous for the edges it was widened to cover.
    """
    findings: list[Finding] = []

    if packages_read < DRIFT_PACKAGE_FLOOR or targets_read < DRIFT_TARGET_FLOOR:
        findings.append(
            Finding(
                "drift-reader-floor",
                DRIFT_FLOOR_MSG.format(
                    packages=packages_read,
                    targets=targets_read,
                    package_floor=DRIFT_PACKAGE_FLOOR,
                    target_floor=DRIFT_TARGET_FLOOR,
                ),
            )
        )

    if rust_test_targets_read != test_map_rows:
        findings.append(
            Finding(
                "drift-map-parity",
                DRIFT_MAP_PARITY_MSG.format(targets=rust_test_targets_read, rows=test_map_rows),
            )
        )

    for package in sorted(set(cargo) | set(build)):
        mapped: set[str] = set()
        for label in sorted(build.get(package, set())):
            name = resolve_label(label, label_map)
            if name is None:
                findings.append(
                    Finding("drift-unmapped", DRIFT_UNMAPPED_MSG.format(label=label))
                )
            else:
                mapped.add(name)
        declared = set(cargo.get(package, set()))
        missing = sorted(declared - mapped)
        extra = sorted(mapped - declared)
        if missing or extra:
            findings.append(
                Finding(
                    "drift-set",
                    DRIFT_SET_MSG.format(pkg=package, missing=missing, extra=extra),
                )
            )
    return findings


# ---------------------------------------------------------------------------
# S-012 — tag guard (C-011). WP-16 supplies the three query results.
# ---------------------------------------------------------------------------


def tag_guard(
    *,
    stage: str,
    no_sandbox: set[str],
    cache_excluded: set[str],
    rust_binaries: set[str],
    stamp_zero: set[str],
    rule_targets_read: int,
) -> list[Finding]:
    """Cache-key soundness, on a floor that advances with the stage.

    `stamp_zero` is a set of **labels**, not a flag: the escape a `rust_binary`
    needs is per-target (`stamp = 0` as a rule attribute, or the
    `no-remote-cache` tag). A `--nostamp` on the command line supplies no label,
    so it cannot green anything here — which is the point of the plan's
    correction to the ADR.

    `cache_excluded` is likewise a set of **labels the caller has already
    judged**, not a tag test. Which tag credits a target depends on its kind —
    measured, `external` is the only tag that stops a *test result* being reused
    and *no* tag makes a build action re-execute — and that classification needs
    the rule class, which lives in the reader. `rule_targets_read` is every rule
    target of every kind, matching `STAGE_FLOORS`.
    """
    findings: list[Finding] = []

    floor = STAGE_FLOORS.get(stage)
    if floor is None:
        findings.append(Finding("tag-stage-unknown", TAG_STAGE_MSG.format(stage=stage)))
    elif rule_targets_read < floor.minimum:
        findings.append(
            Finding(
                "tag-reader-floor",
                TAG_FLOOR_MSG.format(
                    read=rule_targets_read, minimum=floor.minimum, universe=floor.universe
                ),
            )
        )

    for label in sorted(no_sandbox - cache_excluded):
        findings.append(Finding("tag-no-sandbox", TAG_NO_SANDBOX_MSG.format(label=label)))

    for label in sorted(rust_binaries - cache_excluded - stamp_zero):
        findings.append(Finding("tag-stamp", TAG_STAMP_MSG.format(label=label)))

    return findings


# ---------------------------------------------------------------------------
# S-002 / S-003 — A1's leaf-vs-hub discriminator, read out of the BEP.
# ---------------------------------------------------------------------------


def _pick(mapping: object, *names: str) -> object:
    """First present key of `names`. Tolerates camelCase and snake_case alike."""
    if not isinstance(mapping, dict):
        return None
    for name in names:
        if name in mapping:
            return mapping[name]
    return None


def _truthy(value: object) -> bool:
    """`True` or the string `"true"`. Presence alone is never enough.

    Two encodings reach here and they disagree about absence: proto3 JSON omits
    a false boolean entirely, and a re-serializer may write it out. Reading
    presence would call an explicit `"cachedLocally": false` a cache hit.
    """
    return value is True or (isinstance(value, str) and value.lower() == "true")


def _cache_evidence(payload: object) -> str:
    """Why this `TestResult` counts as cached, or `""` for "it ran".

    A1 asks one binary question — *did this target re-run?* — so every tier
    counts, and the three signals below are each sufficient for "it did not".
    Measured across five real 9.2.0 runs of one workspace (the BEP is
    `test/fixtures/bep/cache_states.json`), they cover the tiers exactly:

    | run | `strategy` | `cachedLocally` | `cachedRemotely` |
    |---|---|---|---|
    | executed | `linux-sandbox` | absent | absent |
    | action cache (live server) | *`executionInfo: {}`* | **true** | absent |
    | `--disk_cache` hit | `disk cache hit` | absent | **true** |
    | remote cache hit | `remote cache hit` | absent | **true** |

    **`cachedRemotely` is read here, and must not be read for "was this a
    *remote* hit".** On 9.2.0 it is true for a `--disk_cache` hit with
    `--remote_cache=` explicitly empty — the disk cache is classified as a
    remote tier. That is sound for this question and false for S-016's, which
    is why S-016's predicate lives in `.claude/tests/test_workflows.py` keyed
    on the runner name and not here.

    The strategy arm accepts the disk runner as well as the remote one, so no
    tier's verdict rests on that surprising boolean alone: were a future Bazel
    to stop emitting it for disk hits, this reader would otherwise call a
    fully-disk-cached warm run a full re-run and red `a1-warm-not-cached` for
    a reason that has nothing to do with A1. Strategy is consulted first so the
    evidence string names the tier that actually served the target.
    """
    info = _pick(payload, "executionInfo", "execution_info")
    strategy = _pick(info, "strategy")
    if isinstance(strategy, str):
        lowered = strategy.lower()
        if "cache" in lowered and ("remote" in lowered or "disk" in lowered):
            return f"strategy={strategy}"
    if _truthy(_pick(payload, "cachedLocally", "cached_locally")):
        return "cached_locally"
    if _truthy(_pick(info, "cachedRemotely", "cached_remotely")):
        return "cached_remotely"
    return ""


def read_test_results(path: Path) -> tuple[dict[str, bool], list[Finding]]:
    """Newline-delimited BEP -> `{label: cached}`, plus any parse findings.

    Field presence and truth, never schema fixity — BEP publishes no stability
    guarantee. An event qualifies when its `id` carries a `testResult` with a
    `label` **and** the event carries a `testResult` payload; every other event
    kind (`progress`, `targetCompleted`, `buildFinished`) is ignored, which the
    self-test proves by planting one whose label would red the graph check if it
    were miscounted.

    A label with several attempts is cached only if every attempt was.
    """
    findings: list[Finding] = []
    outcomes: dict[str, bool] = {}
    malformed = 0

    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        return outcomes, [Finding("a1-bep-unreadable", f"A1: cannot read BEP {path}: {error}")]

    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            # A truncated last line is what a crashed build leaves behind. It
            # must be loud: a reader that shrugs reads fewer targets and the
            # floor below then fires for the wrong reason, or not at all.
            malformed += 1
            continue
        identifier = _pick(event, "id")
        test_id = _pick(identifier, "testResult", "test_result")
        payload = _pick(event, "testResult", "test_result")
        if test_id is None or payload is None:
            continue
        label = _pick(test_id, "label")
        if not isinstance(label, str) or not label:
            malformed += 1
            continue
        cached = bool(_cache_evidence(payload))
        outcomes[label] = outcomes.get(label, True) and cached

    if malformed:
        findings.append(
            Finding(
                "a1-bep-malformed",
                f"A1: {malformed} unparseable or label-less event(s) in {path} — a truncated "
                "BEP reads as a smaller universe, not as a clean one",
            )
        )
    return outcomes, findings


def rerun_set(outcomes: dict[str, bool]) -> set[str]:
    return {label for label, cached in outcomes.items() if not cached}


def a1_verdict(
    *,
    warm: dict[str, bool],
    leaf: dict[str, bool],
    hub: dict[str, bool],
    rdeps_leaf: set[str],
    rdeps_hub: set[str],
) -> list[Finding]:
    """A1 is met only when every one of these holds. Empty list == MET.

    **A re-run set measures change propagation, not reverse dependency.** The
    two are not the same relation and the difference is the whole failure mode
    of this check. Bazel prunes on *output identity*: a dependent re-runs only
    if some action input changed bytes, and a rustc invocation whose inputs are
    unchanged is a cache hit whether or not the graph knows it is a reverse
    dependency. So `leaf_rerun == hub_rerun` is evidence about the PROBES
    before it is evidence about the graph.

    Measured, WP-12: the first probe pair appended an unused `pub const` to each
    crate's `lib.rs`. It was dead-stripped, every dependent's test binary came
    out byte-identical, every test action was a cache hit — and this function
    returned `a1-probes-alike` on a graph that was per-crate the whole time. The
    same hub run executed 48 sandboxed compile actions against the leaf's 15,
    which is the per-crate answer the re-run sets could not see. A1 only reported
    MET once both probes changed *emitted* code: `ocx_util::ResultExt::ignore`, a
    generic monomorphised into every consumer, and
    `ocx_announce::forge::probe_git_binary`, which `ocx_cli` calls.

    `a1-probes-alike` therefore names that cause first. Read it as "the probe or
    the probe pair is wrong" until the probe is shown to change emitted code;
    only then is it a statement about the graph.

    Ordered so that the discriminator is reached with the cheap guards already
    satisfied: a run where the touch never landed, or where the reader stopped
    early, would otherwise produce two equal (empty) re-run sets and red on the
    interesting message for an uninteresting reason.
    """
    findings: list[Finding] = []

    for name, outcomes in (("warm", warm), ("leaf", leaf), ("hub", hub)):
        if len(outcomes) < CRATES_TEST_TARGETS:
            findings.append(
                Finding(
                    "a1-reader-floor",
                    A1_FLOOR_MSG.format(
                        run=name, read=len(outcomes), minimum=CRATES_TEST_TARGETS
                    ),
                )
            )

    warm_rerun = sorted(rerun_set(warm))
    if warm_rerun:
        findings.append(
            Finding(
                "a1-warm-not-cached",
                A1_WARM_MSG.format(count=len(warm_rerun), sample=warm_rerun[0]),
            )
        )

    leaf_rerun = rerun_set(leaf)
    hub_rerun = rerun_set(hub)
    if not leaf_rerun:
        findings.append(Finding("a1-touch-empty", A1_EMPTY_MSG.format(which="leaf")))
    if not hub_rerun:
        findings.append(Finding("a1-touch-empty", A1_EMPTY_MSG.format(which="hub")))

    for which, actual, expected in (("leaf", leaf_rerun, rdeps_leaf), ("hub", hub_rerun, rdeps_hub)):
        wide = sorted(actual - expected)
        if wide:
            findings.append(
                Finding("a1-graph-too-wide", A1_WIDE_MSG.format(which=which, extra=wide))
            )

    if leaf_rerun and hub_rerun:
        delta = leaf_rerun ^ hub_rerun
        if not delta:
            findings.append(Finding("a1-not-per-crate", A1_SAME_MSG.format(count=len(leaf_rerun))))
        elif len(delta) <= 2:
            # Two probes that invalidate the same closure differ by exactly
            # their own two targets. Not a statement about the graph — a
            # statement that this run cannot make one, and the first thing to
            # suspect is the probe rather than the pair: a probe whose edit is
            # dead-stripped produces byte-identical outputs, so every dependent
            # is a cache hit and lands here from a graph that is per-crate.
            findings.append(Finding("a1-probes-alike", A1_ALIKE_MSG.format(delta=len(delta))))

    return findings


def read_labels(path: Path) -> set[str]:
    """`bazel query` output: one label per line, blanks and comments dropped."""
    lines = path.read_text(encoding="utf-8").splitlines()
    return {line.strip() for line in lines if line.strip() and not line.startswith("#")}


def run_a1(warm: Path, leaf: Path, hub: Path, rdeps_leaf: Path, rdeps_hub: Path) -> int:
    findings: list[Finding] = []
    outcomes: dict[str, dict[str, bool]] = {}
    for name, path in (("warm", warm), ("leaf", leaf), ("hub", hub)):
        parsed, parse_findings = read_test_results(path)
        outcomes[name] = parsed
        findings.extend(parse_findings)
    findings.extend(
        a1_verdict(
            warm=outcomes["warm"],
            leaf=outcomes["leaf"],
            hub=outcomes["hub"],
            rdeps_leaf=read_labels(rdeps_leaf),
            rdeps_hub=read_labels(rdeps_hub),
        )
    )
    if findings:
        sys.stdout.flush()
        print("A1: NOT MET", file=sys.stderr)
        return report(findings)
    print(
        f"A1: MET — warm run fully cached over {len(outcomes['warm'])} test targets; "
        f"leaf touch re-ran {len(rerun_set(outcomes['leaf']))}, hub touch re-ran "
        f"{len(rerun_set(outcomes['hub']))}, and the two sets differ"
    )
    return 0


# ---------------------------------------------------------------------------
# C-029 — the corpus reaches no realm. Asserted, not promised.
# ---------------------------------------------------------------------------

AMBIENT_MODULES = frozenset({"subprocess", "socket", "urllib", "http", "ssl", "requests"})


def ambient_reads(source: str) -> list[str]:
    """Every import or env read in `source` that could leave this process.

    AST, never `grep`: the banned spellings appear in this file as data and in
    the docstring as prose, so a text scan would match itself in every state.
    """
    hits: list[str] = []
    for node in ast.walk(ast.parse(source)):
        if isinstance(node, ast.Import):
            hits.extend(
                f"import {alias.name}"
                for alias in node.names
                if alias.name.split(".")[0] in AMBIENT_MODULES
            )
        elif isinstance(node, ast.ImportFrom):
            root = (node.module or "").split(".")[0]
            if root in AMBIENT_MODULES:
                hits.append(f"from {node.module} import ...")
        elif isinstance(node, ast.Attribute):
            value = node.value
            if isinstance(value, ast.Name) and value.id == "os" and node.attr in {"environ", "getenv"}:
                hits.append(f"os.{node.attr}")
    return hits


# ---------------------------------------------------------------------------
# Fixtures. Synthetic by construction — the live tree has none of this yet.
# ---------------------------------------------------------------------------

LEAF = "ocx_exit"
HUB = "ocx_util"


def test_labels() -> list[str]:
    """`CRATES_TEST_TARGETS` stage-1 `rust_test` labels: the two the ADR names,
    then filler.

    The count comes from the constant rather than a literal `range(3, 34)`, so
    a correction to the constant reaches the fixture. Written as a literal it
    did not: the fixture kept generating 33 labels after the real graph grew to
    34, and `prove_parity` then balanced 33 against 36 - 3 and stayed green on
    two compensating errors.

    Filler is named `pkgNN` rather than borrowed from the real crate list on
    purpose: the real names would read as a claim about the real graph, and
    this fixture makes no claim about it.
    """
    labels = [f"//crates/{LEAF}:{LEAF}_test", f"//crates/{HUB}:{HUB}_test"]
    labels += [
        f"//crates/pkg{index:02d}:pkg{index:02d}_test"
        for index in range(3, CRATES_TEST_TARGETS + 1)
    ]
    return labels


def bep_line(label: str, *, cached: bool = False, remote: bool = False, attempt: int = 1) -> str:
    """One `TestResult` event. A cached target carries a marker; a re-run one
    carries **no** `cachedLocally` key at all, which is what Bazel emits."""
    execution: dict[str, object] = {"strategy": "linux-sandbox", "exitCode": 0}
    payload: dict[str, object] = {"testStatus": "PASSED"}
    if remote:
        execution = {"strategy": "remote cache hit", "cachedRemotely": True}
    elif cached:
        payload["cachedLocally"] = True
    payload["executionInfo"] = execution
    event = {
        "id": {"testResult": {"label": label, "run": 1, "shard": 1, "attempt": attempt}},
        "testResult": payload,
    }
    return json.dumps(event)


def noise_lines() -> list[str]:
    """Events the reader must ignore.

    The `targetCompleted` label is deliberately one that appears in **no**
    rdeps answer: if the reader ever counted non-`TestResult` events, the graph
    check would red on it, so the green half proves the filter rather than
    assuming it.
    """
    return [
        json.dumps({"id": {"progress": {"opaqueCount": 3}}, "progress": {"stderr": "INFO: ..."}}),
        json.dumps(
            {
                "id": {"targetCompleted": {"label": "//crates/not_a_test:not_a_test"}},
                "completed": {"success": True},
            }
        ),
        json.dumps({"id": {"buildFinished": {}}, "finished": {"exitCode": {"code": 0}}}),
    ]


def write_bep(path: Path, rerunning: set[str], *, remote_hits: set[str] | None = None) -> None:
    """A BEP over all 33 test targets; `rerunning` carries no cache marker."""
    remote_hits = remote_hits or set()
    lines = noise_lines()[:1]
    for label in test_labels():
        lines.append(
            bep_line(
                label,
                cached=label not in rerunning,
                remote=label in remote_hits,
            )
        )
    lines.extend(noise_lines()[1:])
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


#: Label spellings borrowed verbatim from `bazel query 'deps(...)'` on this
#: tree. The version is part of the package segment AND of the target name, and
#: the name is the crate's real package name — `rustls-pki-types`, never the
#: workspace's `pki-types` rename.
TOKIO = "@crates//tokio-1.53.1:tokio-1.53.1"
SERDE = "@crates//serde-1.0.229:serde-1.0.229"
OCI_CLIENT = "@crates//oci-client-0.17.0:oci-client-0.17.0"
#: The one first-party label whose cargo package name is not its directory name.
OCX_CLI_LABEL = "//crates/ocx_cli:ocx_cli"


def sample_drift_universe() -> tuple[dict[str, set[str]], dict[str, set[str]], dict[str, str]]:
    """`CRATE_PACKAGES` packages whose cargo and Bazel edge sets agree.

    No `external/` package and no `//external/...` edge: measured, the patched
    submodule crates reach first-party targets as ordinary `@crates//` aliases,
    and `//...` does not expand into Bazel's reserved `external` directory at
    all. `EXTERNAL_PACKAGES` is 0 for both reasons.
    """
    label_map = {
        TOKIO: "tokio",
        SERDE: "serde",
        OCI_CLIENT: "oci-client",
        # The first-party rename. `FIRST_PARTY_LABEL` would answer `ocx_cli`;
        # `resolve_label` reads `label_map` first, which is what makes this row
        # the fix rather than a second opinion.
        OCX_CLI_LABEL: "ocx",
    }

    cargo: dict[str, set[str]] = {LEAF: set(), HUB: {"tokio"}}
    build: dict[str, set[str]] = {LEAF: set(), HUB: {TOKIO}}
    for index in range(3, CRATE_PACKAGES + 1):
        package = f"pkg{index:02d}"
        cargo[package] = {HUB, LEAF, "tokio", "serde"}
        build[package] = {
            f"//crates/{HUB}:{HUB}",
            f"//crates/{LEAF}:{LEAF}",
            TOKIO,
            SERDE,
        }
    # One package reaches a patched submodule crate, so that mapping row is
    # exercised by the green half rather than only by the red one.
    cargo["pkg03"].add("oci-client")
    build["pkg03"].add(OCI_CLIENT)
    # And one reaches the crate whose cargo package name is not its label's.
    cargo["pkg04"].add("ocx")
    build["pkg04"].add(OCX_CLI_LABEL)
    return cargo, build, label_map


# ---------------------------------------------------------------------------
# Self-test.
# ---------------------------------------------------------------------------


def expect(condition: bool, problem: str) -> None:
    """A loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not condition:
        raise SystemExit(f"bazel gate proofs self-test: {problem}")


def _presence_reader(path: Path) -> set[str]:
    """The wrong reader, kept as a named control, used nowhere else.

    `payload.get("cachedLocally") is not False` is the natural spelling and it
    is inverted: proto3 JSON omits a false boolean, so a target that *ran*
    carries no key, `.get` returns `None`, and `None is not False` is `True`.
    It reports a universally re-running build as universally cached.
    """
    reruns: set[str] = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        event = json.loads(line)
        payload = event.get("testResult")
        label = event.get("id", {}).get("testResult", {}).get("label")
        if payload is None or not label:
            continue
        if payload.get("cachedLocally") is False:
            reruns.add(label)
    return reruns


def prove_pin(scratch: Path) -> int:
    """S-009 — `.bazelversion` vs the resolved binary."""
    checks = 0
    pin = scratch / ".bazelversion"
    good = "9.2.0\n"
    pin.write_text(good, encoding="utf-8")

    green = pin_drift(read_bazelversion(pin), "bazel 9.2.0\n", 0)
    expect(green == [], f"the agreeing pair must be silent, got {green}")
    print("S-009 GREEN: .bazelversion 9.2.0 vs `bazel 9.2.0` — no findings, both sources read")
    checks += 1

    pin.write_text("9.1.0\n", encoding="utf-8")
    expect(pin.read_text(encoding="utf-8").strip() == "9.1.0", "the 9.1.0 mutation did not land")
    drift = pin_drift(read_bazelversion(pin), "bazel 9.2.0\n", 0)
    expect(codes(drift) == ["pin-drift"], f"expected only pin-drift, got {codes(drift)}")
    expect("9.1.0" in drift[0].message and "9.2.0" in drift[0].message, "the line names one version")
    print(f"S-009 RED  : {drift[0].message}")
    checks += 1

    pin.unlink()
    expect(not pin.exists(), "the deletion of .bazelversion did not land")
    absent = pin_drift(read_bazelversion(pin), "bazel 9.2.0\n", 0)
    expect(codes(absent) == ["pin-file"], f"expected only pin-file, got {codes(absent)}")
    print(f"S-009 RED  : {absent[0].message}")
    checks += 1

    # Restore from our own bytes. Never `git checkout --`, which restores from
    # the index and would make every result above unattributable.
    pin.write_text(good, encoding="utf-8")
    expect(pin.read_text(encoding="utf-8") == good, "the restore did not land")
    expect(pin_drift(read_bazelversion(pin), "bazel 9.2.0\n", 0) == [], "restoring must green again")

    missing_binary = pin_drift(read_bazelversion(pin), "", 1)
    expect(codes(missing_binary) == ["pin-reader-floor"], f"got {codes(missing_binary)}")
    print(f"S-009 RED  : {missing_binary[0].message} (rc 1, no output)")
    checks += 1

    unparseable = pin_drift(read_bazelversion(pin), "bazel no_version\n", 0)
    expect(codes(unparseable) == ["pin-reader-floor"], f"got {codes(unparseable)}")
    print("S-009 RED  : `bazel no_version` reaches the reader floor — unparseable is not agreement")
    checks += 1

    nothing = pin_drift(None, None, None)
    expect(
        codes(nothing) == ["pin-file", "pin-reader-floor"],
        f"a run that read zero sources must red on both, got {codes(nothing)}",
    )
    print("S-009 RED  : zero version sources read — two distinct findings, never a silent agreement")
    checks += 1
    return checks


def prove_drift() -> int:
    """S-010 — a dropped dep, in both halves, plus the unmapped label."""
    checks = 0
    cargo, build, label_map = sample_drift_universe()
    baseline = {
        "packages_read": DRIFT_PACKAGE_FLOOR,
        "targets_read": DRIFT_TARGET_FLOOR,
        "rust_test_targets_read": CRATES_TEST_TARGETS,
        "test_map_rows": CRATES_TEST_TARGETS,
    }

    green = build_drift(cargo=cargo, build=build, label_map=label_map, **baseline)
    expect(green == [], f"the agreeing universe must be silent, got {[f.message for f in green]}")
    expect(
        len(build) == DRIFT_PACKAGE_FLOOR,
        f"fixture has {len(build)} packages, expected {DRIFT_PACKAGE_FLOOR}",
    )
    expect(
        resolve_label(OCX_CLI_LABEL, label_map) == "ocx",
        "the first-party rename row is not being consulted before FIRST_PARTY_LABEL",
    )
    print(
        f"S-010 GREEN: {DRIFT_PACKAGE_FLOOR} packages, {DRIFT_TARGET_FLOOR} targets, "
        "first-party and @crates//<name>-<version> edges agree"
    )
    checks += 1

    # --- first-party edge dropped from BUILD.
    mutated = {key: set(value) for key, value in build.items()}
    dropped = f"//crates/{HUB}:{HUB}"
    mutated["pkg05"].discard(dropped)
    expect(dropped in build["pkg05"], "the green fixture never had the edge being dropped")
    expect(dropped not in mutated["pkg05"], "the first-party drop did not land in the mutated table")
    first_party = build_drift(cargo=cargo, build=mutated, label_map=label_map, **baseline)
    expect(codes(first_party) == ["drift-set"], f"got {codes(first_party)}")
    expect("pkg05" in first_party[0].message, "the finding does not name the package")
    expect(
        f"in Cargo not in BUILD: ['{HUB}']" in first_party[0].message,
        f"the finding does not name the direction: {first_party[0].message}",
    )
    print(f"S-010 RED  : {first_party[0].message}")
    checks += 1

    # --- the half a first-party-only scope cannot see: a `@crates//` edge.
    mutated = {key: set(value) for key, value in build.items()}
    mutated["pkg06"].discard(TOKIO)
    expect(TOKIO in build["pkg06"], f"the green fixture never had {TOKIO}")
    expect(TOKIO not in mutated["pkg06"], "the third-party drop did not land")
    third_party = build_drift(cargo=cargo, build=mutated, label_map=label_map, **baseline)
    expect(codes(third_party) == ["drift-set"], f"got {codes(third_party)}")
    expect(
        "in Cargo not in BUILD: ['tokio']" in third_party[0].message,
        f"the third-party finding does not name tokio: {third_party[0].message}",
    )
    print(f"S-010 RED  : {third_party[0].message}")
    checks += 1

    # --- the other direction: an edge BUILD has and Cargo does not.
    mutated = {key: set(value) for key, value in build.items()}
    mutated["pkg07"].add(SERDE)
    mutated_cargo = {key: set(value) for key, value in cargo.items()}
    mutated_cargo["pkg07"].discard("serde")
    expect("serde" not in mutated_cargo["pkg07"], "the cargo-side removal did not land")
    added = build_drift(cargo=mutated_cargo, build=mutated, label_map=label_map, **baseline)
    expect(codes(added) == ["drift-set"], f"got {codes(added)}")
    expect(
        "in BUILD not in Cargo: ['serde']" in added[0].message,
        f"the added-direction finding is wrong: {added[0].message}",
    )
    print(f"S-010 RED  : {added[0].message}")
    checks += 1

    # --- an unmapped third-party label is its own exit 1, never a silent skip.
    #     Spelled as a VERSION bump rather than a rename, because that is the
    #     way this label form actually goes unmapped: the version is in both
    #     halves of `@crates//<name>-<version>:<name>-<version>`, so every
    #     `cargo update` that moves a pin retires a mapping row.
    bumped = "@crates//tokio-1.54.0:tokio-1.54.0"
    mutated = {key: set(value) for key, value in build.items()}
    mutated["pkg08"].discard(TOKIO)
    mutated["pkg08"].add(bumped)
    expect(bumped in mutated["pkg08"], "the version bump did not land")
    expect(bumped not in label_map, "the bumped label must have no row")
    unmapped = build_drift(cargo=cargo, build=mutated, label_map=label_map, **baseline)
    expect("drift-unmapped" in codes(unmapped), f"got {codes(unmapped)}")
    named = [f for f in unmapped if f.code == "drift-unmapped"]
    expect(bumped in named[0].message, "the finding does not name the label")
    print(f"S-010 RED  : {named[0].message}")
    checks += 1

    # --- reader floors. The query pointed at an empty directory reads as a
    #     clean tree to every check above; only the floor tells them apart.
    empty = build_drift(
        cargo={}, build={}, label_map=label_map,
        packages_read=0, targets_read=0, rust_test_targets_read=0,
        test_map_rows=CRATES_TEST_TARGETS,
    )
    expect(
        codes(empty) == ["drift-map-parity", "drift-reader-floor"],
        f"an empty read must red on the floor, got {codes(empty)}",
    )
    floor = next(f for f in empty if f.code == "drift-reader-floor")
    print(f"S-010 RED  : {floor.message}")
    checks += 1

    one_short = build_drift(
        cargo=cargo, build=build, label_map=label_map,
        packages_read=DRIFT_PACKAGE_FLOOR, targets_read=DRIFT_TARGET_FLOOR - 1,
        rust_test_targets_read=CRATES_TEST_TARGETS, test_map_rows=CRATES_TEST_TARGETS,
    )
    expect(codes(one_short) == ["drift-reader-floor"], f"got {codes(one_short)}")
    print(
        f"S-010 RED  : {DRIFT_TARGET_FLOOR - 1} of {DRIFT_TARGET_FLOOR} targets read — one "
        "target short is still a reader that stopped early"
    )
    checks += 1

    parity = build_drift(
        cargo=cargo, build=build, label_map=label_map,
        packages_read=DRIFT_PACKAGE_FLOOR, targets_read=DRIFT_TARGET_FLOOR,
        rust_test_targets_read=CRATES_TEST_TARGETS - 1, test_map_rows=CRATES_TEST_TARGETS,
    )
    expect(codes(parity) == ["drift-map-parity"], f"got {codes(parity)}")
    print(f"S-010 RED  : {next(f for f in parity if f.code == 'drift-map-parity').message}")
    checks += 1
    return checks


def prove_tags() -> int:
    """S-012 — cache-key soundness, and a floor that cannot be left behind."""
    checks = 0
    cast = "//test/doc_scripts:contributing__bazel-first-build"
    binary = "//crates/ocx_cli:ocx"
    baseline = {
        "stage": "stage-1",
        "no_sandbox": {cast},
        "cache_excluded": {cast},
        "rust_binaries": {binary},
        "stamp_zero": {binary},
        "rule_targets_read": DRIFT_TARGET_FLOOR,
    }

    green = tag_guard(**baseline)
    expect(green == [], f"the sound set must be silent, got {[f.message for f in green]}")
    print(
        f"S-012 GREEN: {cast} is no-sandbox + credited, {binary} carries stamp = 0, "
        f"{DRIFT_TARGET_FLOOR} rule targets read in //crates/..."
    )
    checks += 1

    unsound = tag_guard(**{**baseline, "cache_excluded": {binary}})
    expect(codes(unsound) == ["tag-no-sandbox"], f"got {codes(unsound)}")
    expect(cast in unsound[0].message, "the finding does not name the label")
    print(f"S-012 RED  : {unsound[0].message}")
    checks += 1

    # The `--nostamp` correction, shown rather than asserted: the only inputs
    # that can green a rust_binary here name a **label**. A global command-line
    # flag supplies no label, so no value of it reaches this comparison.
    stamped = tag_guard(**{**baseline, "stamp_zero": set()})
    expect(codes(stamped) == ["tag-stamp"], f"got {codes(stamped)}")
    expect(binary in stamped[0].message, "the finding does not name the binary")
    print(f"S-012 RED  : {stamped[0].message}")
    checks += 1
    greened_by_label = tag_guard(**{**baseline, "stamp_zero": set(), "cache_excluded": {cast, binary}})
    expect(greened_by_label == [], "the no-remote-cache tag must green the binary")
    print("S-012 GREEN: the same binary greens only via a per-target escape — no flag can supply one")
    checks += 1

    short = tag_guard(**{**baseline, "rule_targets_read": DRIFT_TARGET_FLOOR - 1})
    expect(codes(short) == ["tag-reader-floor"], f"got {codes(short)}")
    expect("//crates/..." in short[0].message, "the floor does not name the stage's universe")
    print(f"S-012 RED  : {short[0].message}")
    checks += 1

    empty_universe = tag_guard(
        stage="stage-1",
        no_sandbox=set(),
        cache_excluded=set(),
        rust_binaries=set(),
        stamp_zero=set(),
        rule_targets_read=0,
    )
    expect(codes(empty_universe) == ["tag-reader-floor"], f"got {codes(empty_universe)}")
    print("S-012 RED  : an empty query universe — every tag check is vacuous, the floor is not")
    checks += 1

    # `stage-4` — `//...`, C-011's last step — is the undeclared one now that
    # stage 3 has a measured count. The red is the same shape and still real; it
    # moved because declaring a stage retires it as a subject for this proof.
    # `stage-5`, a name no wave has: stage 4 carries a declared floor now, and
    # this case is about a stage that does not, so it needs a name that will
    # stay undeclared rather than the next one due to be declared.
    unknown = tag_guard(**{**baseline, "stage": "stage-5"})
    expect(codes(unknown) == ["tag-stage-unknown"], f"got {codes(unknown)}")
    print(f"S-012 RED  : {unknown[0].message}")
    checks += 1

    # Stage 3's floor, and the reason it is not the Rust subset: 143 rule targets
    # over the union universe, of which the 87 in //test/doc_scripts/... are all
    # non-Rust and carry every no-sandbox tag in the graph.
    stage3 = STAGE_FLOORS["stage-3"]
    expect(
        stage3.minimum == DRIFT_TARGET_FLOOR + CAST_RULE_TARGETS + GIF_RULE_TARGETS,
        f"stage-3's floor is {stage3.minimum}, not {DRIFT_TARGET_FLOOR} + "
        f"{CAST_RULE_TARGETS} + {GIF_RULE_TARGETS}",
    )
    wide = tag_guard(**{**baseline, "stage": "stage-3", "rule_targets_read": stage3.minimum})
    expect(wide == [], f"the stage-3 floor must be clearable, got {codes(wide)}")
    rust_only = tag_guard(
        **{**baseline, "stage": "stage-3", "rule_targets_read": CRATES_RULE_TARGETS}
    )
    expect(
        codes(rust_only) == ["tag-reader-floor"],
        "a stage-3 run that read only the Rust subset must red — that is the state where "
        f"every no-sandbox target in the graph went unread; got {codes(rust_only)}",
    )
    print(f"S-012 RED  : {rust_only[0].message}")
    checks += 2
    return checks


def prove_a1(scratch: Path) -> int:
    """S-002 / S-003 — the leaf-vs-hub discriminator, on two BEP pairs."""
    checks = 0
    labels = test_labels()
    leaf_label, hub_label = labels[0], labels[1]
    expect(len(labels) == CRATES_TEST_TARGETS, f"fixture has {len(labels)} test targets")

    warm = scratch / "warm.json"
    leaf = scratch / "leaf.json"
    hub = scratch / "hub.json"

    # --- the per-crate pair.
    leaf_reruns = {leaf_label, *labels[2:5]}
    hub_reruns = {hub_label, *labels[2:26]}
    rdeps_leaf = set(leaf_reruns) | {f"//crates/{LEAF}:{LEAF}"}
    rdeps_hub = set(hub_reruns) | {f"//crates/{HUB}:{HUB}"}
    write_bep(warm, set())
    write_bep(leaf, leaf_reruns)
    # One remotely cached target in the hub run, so the remote evidence path is
    # exercised by the green half too, not only by the docstring.
    write_bep(hub, hub_reruns, remote_hits={labels[30]})

    warm_read, warm_parse = read_test_results(warm)
    leaf_read, leaf_parse = read_test_results(leaf)
    hub_read, hub_parse = read_test_results(hub)
    expect(warm_parse + leaf_parse + hub_parse == [], "the fixtures must parse cleanly")
    expect(len(warm_read) == CRATES_TEST_TARGETS, f"warm read {len(warm_read)} labels, expected 33")
    expect(
        "//crates/not_a_test:not_a_test" not in warm_read,
        "the reader counted a targetCompleted event as a test target",
    )
    expect(rerun_set(leaf_read) == leaf_reruns, "the leaf BEP did not round-trip")
    expect(rerun_set(hub_read) == hub_reruns, "the hub BEP did not round-trip")

    per_crate = a1_verdict(
        warm=warm_read, leaf=leaf_read, hub=hub_read, rdeps_leaf=rdeps_leaf, rdeps_hub=rdeps_hub
    )
    expect(per_crate == [], f"the per-crate pair must be MET, got {[f.message for f in per_crate]}")
    print(
        f"S-002 GREEN: leaf touch re-ran {len(leaf_reruns)} of 33, all inside rdeps(ocx_exit); "
        "warm run fully cached; a targetCompleted event outside rdeps was correctly ignored"
    )
    checks += 1
    print(f"S-003 GREEN: hub touch re-ran {len(hub_reruns)} of 33 — a different set, so A1 is MET")
    checks += 1

    # --- the universally-invalidating pair. Every other guard is satisfied on
    #     purpose: the rdeps answers are the pessimistic whole universe, so the
    #     subset checks pass and the discriminator is the only thing that reds.
    everything = set(labels)
    write_bep(leaf, everything)
    write_bep(hub, everything)
    leaf_all, _ = read_test_results(leaf)
    hub_all, _ = read_test_results(hub)
    expect(rerun_set(leaf_all) == everything, "the universal leaf mutation did not land")
    expect(rerun_set(hub_all) == everything, "the universal hub mutation did not land")
    expect(
        "cachedLocally" not in leaf.read_text(encoding="utf-8"),
        "the universal BEP still carries a cache marker — the mutation did not land",
    )
    universal = a1_verdict(
        warm=warm_read, leaf=leaf_all, hub=hub_all, rdeps_leaf=everything, rdeps_hub=everything
    )
    expect(
        codes(universal) == ["a1-not-per-crate"],
        f"the discriminator must be the only red, got {codes(universal)}",
    )
    print(f"S-003 RED  : {universal[0].message}")
    checks += 1

    # --- two probes that invalidate the same closure. This is not a made-up
    #     shape: over scripts/crate_map.toml the reverse closures of ocx_exit
    #     and ocx_util are the same 16 crates, so the ADR's own pair produces
    #     exactly this — two sets differing by each probe's own target.
    #     Written to their own files so the universal pair above stays on disk
    #     for the control reader below, which is asserted against those bytes.
    shared = set(labels[2:19])
    leaf_alike_path = scratch / "leaf_alike.json"
    hub_alike_path = scratch / "hub_alike.json"
    write_bep(leaf_alike_path, {leaf_label, *shared})
    write_bep(hub_alike_path, {hub_label, *shared})
    leaf_alike, _ = read_test_results(leaf_alike_path)
    hub_alike, _ = read_test_results(hub_alike_path)
    delta = rerun_set(leaf_alike) ^ rerun_set(hub_alike)
    expect(delta == {leaf_label, hub_label}, f"the alike mutation did not land: {sorted(delta)}")
    alike = a1_verdict(
        warm=warm_read,
        leaf=leaf_alike,
        hub=hub_alike,
        rdeps_leaf={leaf_label, hub_label, *shared},
        rdeps_hub={leaf_label, hub_label, *shared},
    )
    expect(codes(alike) == ["a1-probes-alike"], f"got {codes(alike)}")
    print(f"S-003 RED  : {alike[0].message}")
    checks += 1

    # The control: the natural-but-wrong reader calls that same file clean.
    naive = _presence_reader(leaf)
    expect(naive == set(), f"the control reader was expected to see 0 re-runs, saw {len(naive)}")
    print(
        "S-003 RED  : on those same bytes the presence-reader control reports 0 re-runs — "
        "proto3 omits a false boolean, which is why this reader asks for truth, not presence"
    )
    checks += 1

    # --- a graph wider than the query says.
    stray = labels[32]
    write_bep(leaf, {leaf_label, *labels[2:5], stray})
    leaf_wide, _ = read_test_results(leaf)
    expect(stray in rerun_set(leaf_wide), "the stray re-run did not land")
    expect(stray not in rdeps_leaf, "the stray label must be outside rdeps(ocx_exit)")
    wide = a1_verdict(
        warm=warm_read, leaf=leaf_wide, hub=hub_read, rdeps_leaf=rdeps_leaf, rdeps_hub=rdeps_hub
    )
    expect(codes(wide) == ["a1-graph-too-wide"], f"got {codes(wide)}")
    expect(stray in wide[0].message, "the finding does not name the offending label")
    print(f"S-002 RED  : {wide[0].message}")
    checks += 1

    # --- the warm half: run 2 re-ran everything, and nothing says `cachedLocally`.
    write_bep(warm, everything)
    warm_cold, _ = read_test_results(warm)
    expect(
        "cachedLocally" not in warm.read_text(encoding="utf-8"),
        "the warm-run mutation did not land",
    )
    cold = a1_verdict(
        warm=warm_cold, leaf=leaf_read, hub=hub_read, rdeps_leaf=rdeps_leaf, rdeps_hub=rdeps_hub
    )
    expect(codes(cold) == ["a1-warm-not-cached"], f"got {codes(cold)}")
    print(f"S-002 RED  : {cold[0].message}")
    checks += 1

    # --- the touch that never landed. Equal (empty) sets must not read as
    #     "not per-crate"; that would pin the wrong cause on a harness bug.
    write_bep(leaf, set())
    leaf_none, _ = read_test_results(leaf)
    expect(rerun_set(leaf_none) == set(), "the no-op leaf mutation did not land")
    nothing = a1_verdict(
        warm=warm_read, leaf=leaf_none, hub=hub_read, rdeps_leaf=rdeps_leaf, rdeps_hub=rdeps_hub
    )
    expect(codes(nothing) == ["a1-touch-empty"], f"got {codes(nothing)}")
    print(f"S-002 RED  : {nothing[0].message}")
    checks += 1

    # --- reader floors: a short BEP, and a truncated one.
    short = scratch / "short.json"
    short.write_text("\n".join(bep_line(label, cached=True) for label in labels[:10]) + "\n", encoding="utf-8")
    short_read, _ = read_test_results(short)
    expect(len(short_read) == 10, f"the short fixture read {len(short_read)} labels")
    floored = a1_verdict(
        warm=short_read, leaf=leaf_read, hub=hub_read, rdeps_leaf=rdeps_leaf, rdeps_hub=rdeps_hub
    )
    expect(codes(floored) == ["a1-reader-floor"], f"got {codes(floored)}")
    print(f"S-002 RED  : {floored[0].message}")
    checks += 1

    truncated = scratch / "truncated.json"
    body = warm.read_text(encoding="utf-8")
    truncated.write_text(body[: len(body) - 40], encoding="utf-8")
    expect(
        not truncated.read_text(encoding="utf-8").endswith("}\n"),
        "the truncation did not land — the last line still closes",
    )
    _, parse_findings = read_test_results(truncated)
    expect(codes(parse_findings) == ["a1-bep-malformed"], f"got {codes(parse_findings)}")
    print(f"S-003 RED  : {parse_findings[0].message}")
    checks += 1

    # --- and the whole thing through the shipped entry point, not just the
    #     comparator: a green run and a red run of `run_a1` itself.
    write_bep(warm, set())
    write_bep(leaf, leaf_reruns)
    write_bep(hub, hub_reruns)
    rdeps_leaf_file = scratch / "rdeps_leaf.txt"
    rdeps_hub_file = scratch / "rdeps_hub.txt"
    rdeps_leaf_file.write_text("\n".join(sorted(rdeps_leaf)) + "\n", encoding="utf-8")
    rdeps_hub_file.write_text("\n".join(sorted(rdeps_hub)) + "\n", encoding="utf-8")
    expect(
        run_a1(warm, leaf, hub, rdeps_leaf_file, rdeps_hub_file) == 0,
        "the shipped --a1 entry point must exit 0 on the per-crate pair",
    )
    write_bep(leaf, everything)
    write_bep(hub, everything)
    rdeps_leaf_file.write_text("\n".join(sorted(everything)) + "\n", encoding="utf-8")
    rdeps_hub_file.write_text("\n".join(sorted(everything)) + "\n", encoding="utf-8")
    expect(
        run_a1(warm, leaf, hub, rdeps_leaf_file, rdeps_hub_file) == 1,
        "the shipped --a1 entry point must exit 1 on the universally-invalidating pair",
    )
    checks += 1
    return checks


def prove_c029(scratch: Path) -> int:
    """C-029 — no proof here can reach the owner-gated remote realm."""
    checks = 0
    source = Path(__file__).read_text(encoding="utf-8")
    hits = ambient_reads(source)
    expect(hits == [], f"this file reaches outside the process: {hits}")
    print("C-029 GREEN: no subprocess/socket/urllib/http/ssl import and no env read in this file")
    checks += 1

    mutated_path = scratch / "mutated_gate_proofs.py"
    mutated = source + "\nimport subprocess\n\n\ndef _leak() -> object:\n    return os.getenv('BAZEL_CACHE_READ_AUTH')\n"
    mutated_path.write_text(mutated, encoding="utf-8")
    landed = mutated_path.read_text(encoding="utf-8")
    expect(landed != source and landed.endswith("')\n"), "the ambient-read mutation did not land")
    found = ambient_reads(landed)
    expect(
        found == ["import subprocess", "os.getenv"],
        f"the mutated copy must red on both, got {found}",
    )
    print(f"C-029 RED  : {found} — an added realm reach is visible to the AST check")
    checks += 1
    return checks


def prove_counts() -> int:
    """The floors are sums, so a half-applied correction cannot hide in a total.

    Each literal here is one `bazel query` answer on this tree, named in the
    constants block above. They are asserted rather than computed so that a
    correction to one summand that forgot the total cannot pass.
    """
    expect(CRATE_PACKAGES == 20, f"crate packages is {CRATE_PACKAGES}, the tree has 20")
    expect(EXTERNAL_PACKAGES == 0, f"external packages is {EXTERNAL_PACKAGES}, the tree has 0")
    expect(CRATES_LIB_TARGETS == 19, f"crates lib targets is {CRATES_LIB_TARGETS}, the tree has 19")
    expect(CRATES_TEST_TARGETS == 34, f"crates test targets is {CRATES_TEST_TARGETS}, the tree has 34")
    expect(
        CRATES_FILEGROUP_TARGETS == 3,
        f"crates filegroups is {CRATES_FILEGROUP_TARGETS}, the tree has 3",
    )
    expect(CRATES_RULE_TARGETS == 53, f"crates rule targets is {CRATES_RULE_TARGETS}, the tree has 53")
    expect(DRIFT_PACKAGE_FLOOR == 20, f"package floor is {DRIFT_PACKAGE_FLOOR}, the tree has 20")
    expect(DRIFT_TARGET_FLOOR == 56, f"target floor is {DRIFT_TARGET_FLOOR}, the tree has 56")
    expect(
        CAST_GENRULE_TARGETS == 40,
        f"cast genrules is {CAST_GENRULE_TARGETS}, the tree has 40 (39 casts + manifest_drift)",
    )
    expect(CAST_SUPPORT_TARGETS == 5, f"cast support targets is {CAST_SUPPORT_TARGETS}, the tree has 5")
    expect(CAST_RULE_TARGETS == 45, f"cast rule targets is {CAST_RULE_TARGETS}, the tree has 45")
    expect(
        GIF_GENRULE_TARGETS == CAST_GENRULE_TARGETS - 1,
        f"gif genrules is {GIF_GENRULE_TARGETS}; WP-33b renders one GIF per cast recording, and "
        f"there are {CAST_GENRULE_TARGETS} cast genrules of which one (`manifest_drift`) is not "
        f"a recording — so this is exactly {CAST_GENRULE_TARGETS - 1}",
    )
    expect(GIF_SUPPORT_TARGETS == 3, f"gif support targets is {GIF_SUPPORT_TARGETS}, the tree has 3")
    expect(GIF_RULE_TARGETS == 42, f"gif rule targets is {GIF_RULE_TARGETS}, the tree has 42")
    expect(
        STAGE_FLOORS["stage-3"].minimum == 143,
        f"stage-3's floor is {STAGE_FLOORS['stage-3'].minimum}, the union universe has 143",
    )
    expect(
        ACCEPTANCE_MODULE_TARGETS == 181,
        f"acceptance modules is {ACCEPTANCE_MODULE_TARGETS}, test/tests/ holds 181",
    )
    expect(
        ACCEPTANCE_RULE_TARGETS == 185,
        f"acceptance rule targets is {ACCEPTANCE_RULE_TARGETS}, //test:all holds 185",
    )
    expect(
        STAGE_FLOORS["stage-4"].minimum == 332,
        f"stage-4's floor is {STAGE_FLOORS['stage-4'].minimum}, `//...` has 332",
    )
    expect(
        STAGE_FLOORS["stage-4"].minimum
        > STAGE_FLOORS["stage-3"].minimum + ACCEPTANCE_MODULE_TARGETS,
        "stage-4's floor must exceed stage-3's by more than the module count, or a run that "
        "read the acceptance modules and nothing else would clear it",
    )
    print(
        "counts  OK : 20 = 20 + 0, 53 = 19 + 34, 56 = 53 + 3, 45 = 40 + 5, 42 = 39 + 3, "
        "143 = 56 + 45 + 42, 185 = 181 + 4, 332 = 143 + 185 + 4 — internal consistency only; "
        "WP-15/WP-16 must assert these against WP-12's generated table, which is the reality "
        "check"
    )
    return 1


def self_test() -> int:
    """All five red/green pairs, on fixtures this file builds."""
    scratch = REPO_ROOT / ".tmp"
    scratch.mkdir(exist_ok=True)
    checks = 0
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        work = Path(directory)
        checks += prove_pin(work)
        checks += prove_drift()
        checks += prove_tags()
        checks += prove_a1(work)
        checks += prove_c029(work)
        checks += prove_counts()
    print(
        f"bazel gate proofs self-test: {checks} checks passed — S-002, S-003, S-009, S-010, S-012 "
        "each shown red and green, plus C-029 and the floor arithmetic"
    )
    print(
        "  wired into taskfiles/scripts.taskfile.yml `self-test:` (WP-17): "
        "`- python3 scripts/bazel_gate_proofs.py --self-test`"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--self-test", action="store_true", help="prove all five pairs red and green")
    mode.add_argument("--a1", action="store_true", help="decide A1 from a warm/leaf/hub BEP triple")
    for name in ("warm", "leaf", "hub", "rdeps-leaf", "rdeps-hub"):
        parser.add_argument(f"--{name}", type=Path, help=f"--a1: the {name} input")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    inputs = (args.warm, args.leaf, args.hub, args.rdeps_leaf, args.rdeps_hub)
    if any(path is None for path in inputs):
        parser.error("--a1 needs --warm, --leaf, --hub, --rdeps-leaf and --rdeps-hub")
    return run_a1(*inputs)


if __name__ == "__main__":
    raise SystemExit(main())
