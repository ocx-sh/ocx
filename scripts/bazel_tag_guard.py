#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`bazel:tag:guard` — C-011, the compensating control for the plan's one BZL-CORE-01 deviation.

    scripts/bazel_tag_guard.py                       # the gate: stage-4, //...
    scripts/bazel_tag_guard.py --stage stage-1 [--bazel bazel] [--universe //crates/...]
    scripts/bazel_tag_guard.py --query-json FILE     # judge a captured query instead of running one
    scripts/bazel_tag_guard.py --self-test

The plan adds `local`, `external`, `no-sandbox`, `requires-network` and
`no-remote-cache` to targets wholesale, which BZL-CORE-01 (**MUST**) names as
the shape of reaching green by weakening the check. The deviation is accepted
on one condition, and this file is it: every such tag is watched, and every
later tag change is a BZL-CORE-01 violation by default. So this gate is the
thing standing between a cache-key-unsound target and every machine that reads
the shared cache — it is written to over-red rather than to under-red, and
every place it could have been lenient says why it is not.

**The comparator is imported, not re-spelled.** WP-13 shipped `tag_guard`,
`STAGE_FLOORS` and the reader-floor idiom in `bazel_gate_proofs.py` and
red-proved them there on inputs built by hand. This file owns the I/O WP-13
left for it: one `bazel query ... --output=streamed_jsonproto`, its parse, and
the classification of each target's tags into the three sets the comparator
compares. The measured tag vocabulary is imported too, from WP-35.

**Three clauses, and the stage decides how many have a subject — measured, not
assumed.** Over `//crates/...` alone the answer was "none": 56 rule targets, 53
of them Rust (19 `rust_library` + 34 `rust_test` + 3 `filegroup`), **0
`rust_binary`, and not one of the 56 carrying any tag at all** (`tags` is present
on every record with no `stringListValue`, which is how proto3 JSON writes an
empty list). Stage 3 adds `//test/doc_scripts/...` — 87 rule targets, 79 of them
genrules, **and 40 of those carry `no-sandbox`**. They are the only `no-sandbox`
targets in the whole graph (`bazel query 'attr(tags, "no-sandbox", kind(rule,
//...))'` answers 40 labels, all in that package), so until the stage advanced,
clause 1's subject existed and the gate was not looking at it. Clause 2 is still
vacuous by absence: 0 `rust_binary` anywhere. A clause that is vacuous today and
untested is a clause that will be vacuous forever, so `--self-test` reds every
one of them on the **live capture**, one named mutation at a time, rather than
on a synthetic fixture that could drift from what `bazel query` emits.

**Clause 1 credits exactly one tag per kind, and both are measured.** A *test*
action and a *build* action were measured separately and the answers differ, so
one allowlist for both would be wrong for one of them.

*Test actions* — WP-35, on the pinned binary with controls
(`bazel_accept_proofs.py --prove-s015`: four `sh_test` targets, three runs —
cold, warm same server, warm fresh server, because two runs cannot separate the
action cache from `--disk_cache`): **`external` is the only tag that stops a test
result being reused.** `no-cache` — the spelling a reader reaches for first —
still reports `(cached)` on a second run in the same output base, and `local`
suppresses only the disk-cache hit, which hides exactly the developer-machine
case.

*Build actions* — WP-33, on a genrule digesting an undeclared file with that file
changed between runs, under `no-cache`, `external`, `external, no-cache` and
`local, external, no-cache`, warm server and fresh: **none re-executed**, and
`aquery` confirmed the requirement reached the action (`executionInfo: [{"key":
"no-cache"}]`). Skyframe decides a build action is up to date from its *declared
inputs* alone; a cache-excluding tag governs only whether the result may be
shared. So `external` on a genrule is a decoration, and the version of this gate
that demanded it was instructing a fix measured not to fix anything — the shape
BZL-CORE-01 calls reaching green by weakening the check, arrived at from the
other side. What a tag can still buy a build action is the machine boundary, and
the credited tag is `no-remote-cache`: no upload to and no download from the
remote cache. It does **not** exclude `--disk_cache` (measured on 9.2.0: a
`no-remote-cache` genrule whose key moved and came back was served with
`runnerCount: {"disk cache hit": 1}`), so the property enforced for a build
action is exactly *"an action that may read undeclared inputs does not offer its
result to another machine"* and nothing wider. Keeping a *stale* result off the
producing host is the declared-input set's job, which is
`bazel_hermeticity_proofs.py --check-declared`, not a tag.

Both allowlists have one member and each is refused for the other kind, proven
both ways in `--self-test`. Anything outside them — including a tag nobody has
measured — is **refused, never greened**: an unmeasured tag reaches no green path
by construction rather than by a branch someone could add a case to.

**Clause 2 is `rust_binary`, and `--nostamp` is not one of its answers.** The
ADR wrote `--nostamp`; the plan's [R1] correction is that a global command-line
flag can name no label and already defaults false, so asserting it is vacuous.
The per-target escapes are `stamp = 0` as a rule attribute and the
`no-remote-cache` tag. Measured in the pinned rules_rust: `_common_attrs` gives
`stamp` a default of **0** (`rust/private/rust.bzl:900`) but `_rust_binary_attrs`
**overrides it to -1** (`rust.bzl:1321`) — "follow `--[no]stamp`", which is the
unsound state. So a `rust_binary` written with no `stamp` attribute reads back
`-1` and reds here. `stamp = 0` is credited **only when explicitly specified**:
that is what "as a rule attribute" means, and it is also what keeps the clause
alive if rules_rust ever flips the default, which would otherwise green every
binary in silence.

**The two escapes are routed to different clauses, on purpose.** `tag_guard`
subtracts `cache_excluded` from both clauses but `stamp_zero` from clause 2
only, and the sets stay separate even where they now agree: a `rust_binary` is a
build rule, so `no-remote-cache` is its clause-1 answer *as well as* its clause-2
escape. The pair that still separates them is an explicit `stamp = 0`, which
discharges clause 2 and buys clause 1 nothing — a `rust_binary` with
`stamp = 0` tagged `no-sandbox` is green on clause 2 and red on clause 1, which
is the correct answer to both. Collapsing the two into one set would green it
twice, and an unsound cache key is not a stamping question. `--self-test` proves
that exact pair rather than describing it.

**Clause 2 narrowed with the kind split, and deliberately.** `external` used to
excuse an unstamped `rust_binary` through `cache_excluded`; it no longer does,
because a `rust_binary` is a build action and `external` is measured inert
there. The escapes are `stamp = 0` and `no-remote-cache`, which is exactly what
C-011 wrote.

**Clause 1 has one narrowed subject, and narrowing is not holing.** The
acceptance package (`//test:`, 181 `sh_test` targets) runs with its results
**cached** — the ADR's stage-4 ruling as amended — so `external`, the tag the
blanket rule demands of a `no-sandbox` test action, is precisely the tag it must
not carry. A blanket rule would red all 181, and the two ways out of that are a
hole (skip the package) and a narrowing (demand something else of it). This file
narrows: an acceptance target is credited without `external` **only while its
input closure declares `//test:docker-compose.yml` and `//test:suite_anchor`** —
the compose definition, which pins every service image and port, and the group
carrying the binary under test. Those are the two inputs whose change a cached
green would otherwise hide, which is what makes the declaration a compensating
control rather than a formality. Lose either and `tag-acceptance-undeclared`
fires beside the blanket finding; carry `external` after all and the blanket
escape still works. `--self-test`'s mode 4 shows both, plus the case a one-hop
reader would miss (a source leaving the shared group, which reds all 181 at
once) and an impostor outside `//test:` declaring the same two labels and still
being refused.

**The closure is one query, not two.** `ruleInput` is one hop — a target whose
`data` names `:suite_inputs` lists the *group's* label — and every filegroup in
the universe is itself a `RULE` record with its own `ruleInput`, so
`input_closure` expands them breadth-first out of the same stream. A `deps(...)`
query would be a second reading of a second universe, and two readings of one
fact are how they stop agreeing.

**The floor is stage-scoped and advances — and this is the default stage.**
`STAGE_FLOORS` (WP-13) declares stage 1 (`//crates/...` >= 56), stage 3
(`//crates/... + //test/doc_scripts/...` >= 143, WP-33's 45 and WP-33b's 42
added to WP-13's 56 — the 42 GIF renders took that package from 45 targets to
87, and until the floor moved with them it cleared by 42 and discriminated
nothing) and now stage 4 (`//...` >= 332: 143 + the acceptance package's 185 +
the 4 root and website targets no earlier universe names). `--stage` defaults to
stage 4, which is the single place the adoption's stage advances:
`bazel:tag:guard` passes no `--stage` on purpose, so a second spelling cannot go
stale. **Stage 4 had to be declared in the same change as the exemption above**:
the acceptance targets are not in stage 3's universe at all, so a narrowed
clause left at stage 3 would be a branch nothing could reach — a clause whose
red state is unreachable, which is the shape this corpus exists to refuse. An
undeclared stage (`stage-5`, the name both self-tests use for the case) is a
**hard red** (`tag-stage-unknown`) refused before any query runs, so a floorless
run is not reachable.

The floor counts **every rule target of every kind**, not the Rust subset. It
counted the Rust subset while stage 1 was the only stage, and that reading does
not survive the advance: `//test/doc_scripts/...` has 0 Rust rule targets and all
40 of the graph's `no-sandbox` ones, so a Rust-only floor is satisfied by a run
that read none of clause 1's subjects. C-011's own numbers were all-kind counts
all along.

**And a zero-match `bazel query` exits 0.** Measured while writing this file: a
`kind(...)` query over a universe that fails to load prints a Starlark
traceback on stderr, writes nothing to stdout, and the pipeline reading it sees
"0 targets" — with stderr redirected away, an ERROR is indistinguishable from a
clean, empty answer. Every floor here is therefore a **count**, never an exit
code; the non-zero rc is reported as well, but it is the count that gates.

**C-029.** This gate reaches no cache realm. `bazel query` is a loading-phase
command: it takes no build options, and `.bazelrc`'s `build --remote_cache=...`
line does not apply to it (that file's own header notes the per-line prefix is
load-bearing). No action is executed and no result is fetched or uploaded.

Every mutation in `--self-test` is proven to have landed before its result is
trusted; until then a surviving green is *unexplained*, not excused. Fixtures
are the live query's own bytes, mutated under this repository's `.tmp/` (never
`/tmp`, a reaped tmpfs on the development host), and nothing is restored with
`git checkout --`, which restores from the index and would make the run vacuous.

Not wired into `taskfiles/scripts.taskfile.yml` — WP-17 owns that file. The one
line it owes the `self-test:` list is named in `--self-test`'s closing output.
"""

from __future__ import annotations

import argparse
import ast
import dataclasses
import fnmatch
import json
import subprocess
import tempfile
from pathlib import Path

from bazel_accept_proofs import CACHE_DEFEATING_TAG, INSUFFICIENT_TAGS
from bazel_gate_proofs import (
    ACCEPTANCE_MODULE_TARGETS,
    ACCEPTANCE_RULE_TARGETS,
    CAST_GENRULE_TARGETS,
    CAST_RULE_TARGETS,
    CRATES_RULE_TARGETS,
    DRIFT_TARGET_FLOOR,
    GIF_RULE_TARGETS,
    GRAPH_TAIL_TARGETS,
    REPO_ROOT,
    STAGE_FLOORS,
    Finding,
    codes,
    report,
    tag_guard,
)

# ---------------------------------------------------------------------------
# Tag vocabulary. Every set here is imported or derived from WP-35's measured
# table — none is an intuition about what a tag ought to do.
# ---------------------------------------------------------------------------

NO_SANDBOX_TAG = "no-sandbox"
"""Clause 1's subject: the target whose action may read undeclared inputs, so
whose cache key does not cover everything it reads."""

CACHE_EXCLUDING_TAGS = frozenset({CACHE_DEFEATING_TAG})
"""Clause 1's allowlist **for a test action**, and it has one member (`external`).

An allowlist rather than a denylist on purpose: a tag nobody has measured must
reach no green path, and with a denylist it would reach one the day somebody
adds a spelling. `INSUFFICIENT_TAGS` below is used only to say *why* a target
reds — never to decide whether it does."""

BUILD_ACTION_EXCLUDING_TAGS = frozenset({"no-remote-cache"})
"""Clause 1's allowlist **for a build action**, and it is a different tag.

WP-35 measured `external` on *test* targets, where test-result caching is its
own mechanism. WP-33 then measured a build action — a genrule digesting an
undeclared file, that file changed between runs — under `no-cache`, `external`,
`external, no-cache` and `local, external, no-cache`, warm server and fresh:
**none re-executed**, and `aquery` confirmed the requirement reached the action.
Skyframe decides a build action is up to date from its declared inputs alone; a
tag governs only whether the result may be *shared*. So demanding `external` of
a genrule demands a decoration, and a gate whose output instructs a fix measured
not to fix it is worse than a silent one.

What a tag can still buy for a build action is the machine boundary, and
`no-remote-cache` is the tag that buys it: no upload to and no download from the
remote cache. It does **not** exclude `--disk_cache` — measured on 9.2.0, a
`no-remote-cache` genrule whose key moved and came back was served with
`runnerCount: {"disk cache hit": 1}` — so the property this clause enforces for
a build action is bounded and stated as such: *an action that may read
undeclared inputs must not have its result offered to another machine.* What
keeps a stale result off the producing host is the declared-input set, which is
check (a) in `bazel_hermeticity_proofs.py`, not a tag."""

STAMP_ESCAPE_TAG = "no-remote-cache"
"""Clause 2's tag escape, the same spelling as `BUILD_ACTION_EXCLUDING_TAGS`'s
one member and routed separately on purpose: see `check_tags`. A `rust_binary`
is a build rule, so the two now agree for that kind — which is the coincidence
the routing in `check_tags` keeps from becoming a dependency."""

TEST_RULE_SUFFIX = "_test"
"""How a test rule is told from a build rule, off `ruleClass` alone.

`bazel query --output=streamed_jsonproto` publishes no "is a test rule" bit, and
`tests(...)` would be a second query whose answer could disagree with this one.
Every test rule class in this graph ends in `_test` (`rust_test`, `sh_test`).
A test rule that did not would be judged as a build action — which asks it for a
*different* tag, never for none, so the failure mode is a wrong instruction and
not a silent green."""

RUST_RULE_PREFIX = "rust_"
"""A query kind string is `"<ruleClass> rule"`, so `kind("^rust_.*rule$", ...)`
is this prefix over `ruleClass`. Reported beside the verdict; the reader floor
counts every rule target of every kind, because stage 3's universe contains 87
non-Rust ones and all 40 of the graph's `no-sandbox` targets."""

ACCEPTANCE_PACKAGE = "//test:"
"""Clause 1's one narrowed subject: the acceptance `sh_test` package."""

ACCEPTANCE_DECLARED_INPUTS = frozenset(
    {
        "//test:docker-compose.yml",
        "//test:suite_anchor",
    }
)
"""What an acceptance target must declare to be credited without `external`.

`docker-compose.yml` carries every service image reference and every host
port, so a bumped registry or a moved port re-keys the suite. `:suite_anchor` is the group that
carries `conftest.py` and `bin/ocx*` — the binary under test, which is A4 red
half 3's whole subject and the input whose staleness a cached green would
otherwise hide.

**Why the anchor label and not `//test:bin/ocx` itself.** That file is a
gitignored `cargo` output: `test/BUILD.bazel` globs it with
`allow_empty = True` because a fresh clone has none, and `task verify` runs
this gate *before* the step that builds it. A required `//test:bin/ocx` would
therefore red on every clean checkout — a red for a state that is fine, which
is the way a gate stops being read. So this clause floors on the *declaration*
(the group is in the target's `data`) and the binary's *bytes* are floored
where they can be: `scripts/bazel_accept_proofs.py --check-s015` compares the
built and under-test digests and proves the whole suite re-executes when they
move."""

ACCEPTANCE_SUPPRESSING_TAGS = INSUFFICIENT_TAGS | {CACHE_DEFEATING_TAG}
"""Tags an acceptance target must NOT carry, now that its results are cached.

The inverse of `CACHE_EXCLUDING_TAGS`, and the clause that makes the amended
stage-4 ruling enforceable from the graph: `external` stops the result being
reused at all, and `local`/`no-remote` suppress the `--disk_cache` tier, which
is the only tier a fresh CI server has. Re-adding any of them would silently
recompute all 181 targets on every CI run while a laptop still reported
`(cached)` — the exact asymmetry `bazel.bzl`'s five-row table measured — and,
until now, nothing reddened: `scripts/bazel_accept_proofs.py --check-s015` is
the run-side reader of that fact and it is invoked by no lane."""

SUITE_INPUTS_LABEL = "//test:suite_inputs"
"""The shared group whose CONTENTS this file floors.

The per-target clause above requires two labels and would stay green while the
glob *inside* this group was narrowed to nothing — which is how `bench/**` and
`taskfile.yml` went undeclared for a whole wave with every gate green. So the
group is floored on what it actually contains, from three directions at once."""

SUITE_INPUTS_REQUIRED_FILES = frozenset(
    {
        "//test:SKIP_CEILING",
        "//test:SUITE_FLOOR",
        "//test:XFAIL_CEILING",
        "//test:conftest.py",
        "//test:docker-compose.yml",
        "//test:ocx.lock",
        "//test:ocx.toml",
        "//test:pyproject.toml",
        "//test:taskfile.yml",
        "//test:uv.lock",
        "//test:zot-config.json",
    }
)
"""Named files the group must carry — each one read by some module, none of
them matched by a directory glob, so a dropped `srcs` line takes one out
without moving any count far enough to notice."""

SUITE_INPUTS_REQUIRED_DIRS = frozenset(
    {
        "bench/",
        "docker/",
        "recordings/",
        "scenarios/",
        "scripts/",
        "sigstore/",
        "specs/",
        "src/",
        "tests/",
    }
)
"""Every directory `test/BUILD.bazel`'s glob names, as a `//test:` path prefix.
A glob deleted outright takes its whole directory out of the closure, which a
count floor alone would only catch for the big ones."""

SUITE_INPUTS_FLOOR = 156
"""How many labels the group's expanded closure must carry, `bin/**` excluded.

Measured on this tree (bazel 9.2.0): 158 labels, of which `bin/ocx` and
`bin/ocx-shim` are the two a fresh clone has not built yet — `test/BUILD.bazel`
globs them `allow_empty = True` for that reason, so they are excluded here and
the number is the same on a clean checkout as on a built one. A floor, so it
only ever rises: a narrowed glob that still covers every directory above is
what this number is for."""

ACCEPTANCE_TESTS_DIR = REPO_ROOT / "test" / "tests"
"""Where the derivation below reads its subject. Source, not the graph: what a
module *reads* is a fact about its Python, and the graph is where the answer is
then checked."""

SWEEP_MODULE_GLOB = "test_*.py"

MODULE_SHAPED_PATTERN_SUFFIX = ".py"
MODULE_SHAPED_PATTERN_PREFIX = "test_"
"""Which `glob`/`rglob` patterns count as reading sibling MODULES.

A pattern is module-shaped when it constrains to Python (`*.py`,
`test_shell*.py`) or to the module naming convention (`test_*`). A bare `"*"`
is deliberately NOT module-shaped, and that is the bound this derivation has:
fifteen modules in this suite call `some_tmp_dir.rglob("*")` over an arena or an
output tree they just wrote, and crediting those would hand the whole 181-module
set to fifteen targets that read none of it — turning every module edit into a
sixteen-target re-run to catch a shape nobody has written. A future
`(root / "test" / "tests").glob("*")` would slip past; it is the one hole, and
it is here rather than in a commit message.
"""

# ---------------------------------------------------------------------------
# Messages. `code` is what the self-test asserts on; the message is stderr.
# Asserting a substring of an English sentence is one of the cheapest ways to
# write an assertion that also matches the opposite outcome (WP-13).
# ---------------------------------------------------------------------------

TAG_QUERY_MSG = (
    "bazel tag guard: `bazel query 'kind(rule, {universe})' --output=streamed_jsonproto` "
    "exited {rc} — nothing was read, so every clause is vacuous. If this is a fresh "
    "checkout or a linked worktree, crate_universe's `Cargo.bazel.lock.json` is gitignored "
    "and absent; the reader floor below is what actually fails the run. stderr: {detail}"
)
TAG_PARSE_MSG = (
    "bazel tag guard: line {line} of the query output is not JSON ({error}) — the reader "
    "stopped there, and a target it never saw carries no tag"
)
TAG_INSUFFICIENT_TEST_MSG = (
    "bazel tag guard: {label} is a TEST action, no-sandbox, and {carries}. The tag to add is "
    "`{credited}`, measured on bazel 9.2.0 (scripts/bazel_accept_proofs.py --prove-s015, four "
    "sh_test targets over cold / warm-same-server / warm-fresh-server, because two runs cannot "
    "separate the action cache from --disk_cache) as the only one that stops a test result "
    "being reused. A target tagged no-cache still reports (cached) on a second run in the same "
    "output base, and `local` suppresses only the disk-cache hit — which is exactly the "
    "developer-machine case"
)
TAG_INSUFFICIENT_BUILD_MSG = (
    "bazel tag guard: {label} is a BUILD action, no-sandbox, and {carries}. The tag to add is "
    "`{credited}`. NOT `external`: measured on bazel 9.2.0, no tag makes a build action "
    "re-execute — no-cache, external, external+no-cache and local+external+no-cache were each "
    "run warm and on a fresh server against a genrule digesting a changed undeclared file and "
    "none re-observed, with aquery confirming the requirement reached the action. Skyframe "
    "decides a build action from its declared inputs alone, so what a tag buys here is the "
    "machine boundary and nothing narrower. Declaring the inputs is the other half and it is "
    "not this gate's: that is bazel_hermeticity_proofs.py --check-declared"
)
TAG_ACCEPTANCE_MSG = (
    "bazel tag guard: {label} is a no-sandbox acceptance test that carries neither "
    "`{credited}` nor the input declaration that stands in for it. Missing from its input "
    "closure: {missing}. The acceptance package runs with its results CACHED "
    "(adr_bazel_build_adoption.md § Stage 4, as amended), which is why `{credited}` is "
    "absent by design — and the exemption is only defensible while every one of these "
    "targets declares the binary under test and the compose definition, because those are "
    "the two inputs whose change a cached green would otherwise hide. Add "
    "`:suite_inputs` back to the target's `data` in test/bazel.bzl, or re-add the missing "
    "source to //test:suite_inputs / //test:suite_anchor"
)
TAG_SUPPRESSED_MSG = (
    "bazel tag guard: {label} is an acceptance target and carries {present} — the acceptance "
    "stage runs with its results CACHED (adr_bazel_build_adoption.md § Stage 4, as amended), "
    "and every one of {forbidden} was measured on bazel 9.2.0 to suppress a cache hit for a "
    "test action (scripts/bazel_accept_proofs.py --prove-s015, five sh_test targets over cold "
    "/ warm-same-server / warm-fresh-server). `external` stops the result being reused at all; "
    "`local` and `no-remote` suppress only the --disk_cache tier, which reads as cached on a "
    "laptop and recomputes the whole suite on a fresh CI server. Remove the tag from "
    "ACCEPTANCE_TAGS in test/bazel.bzl"
)
TAG_UNSWEPT_MSG = (
    "bazel tag guard: {label} reads its SIBLING modules' source ({patterns}) but {count} of "
    "them are not among its declared inputs, first {sample}. Its result would then be served "
    "from cache over a tree it has never read — a `@pytest.mark.smoke` marker deleted from one "
    "module re-runs that module's target and leaves this one CACHED, so the sweep cannot "
    "observe what it sweeps. Add the module to this target's row in `extra_data` in "
    "test/BUILD.bazel (`_SWEEP_ALL` for a whole-suite sweep, `_SWEEP_SHELL` for `test_shell*`)"
)
TAG_SUITE_INPUTS_ABSENT_MSG = (
    "bazel tag guard: {label} is not a rule target in this universe, so the group every "
    "acceptance target declares was never read and its floor is vacuous"
)
TAG_SUITE_INPUTS_FILE_MSG = (
    "bazel tag guard: {label} does not carry {missing} — each is a file some module reads and "
    "none is matched by a directory glob, so a dropped `srcs` line removes it while both "
    "labels the per-target clause requires stay in place"
)
TAG_SUITE_INPUTS_DIR_MSG = (
    "bazel tag guard: {label} carries no file under {missing} — a glob was deleted or narrowed "
    "out of test/BUILD.bazel. The per-target clause cannot see this: it requires "
    "`//test:suite_anchor` and `//test:docker-compose.yml`, both of which survive it"
)
TAG_SUITE_INPUTS_FLOOR_MSG = (
    "bazel tag guard: {label} expands to {read} label(s) (bin/** excluded) against a floor of "
    "{floor} — the glob narrowed. A narrower shared group is a smaller cache key, and every "
    "acceptance target then reports cached over files it still reads"
)
TAG_CARRIES_SOME = "carries {present}, none of which excludes it from a cache"
TAG_CARRIES_NONE = "carries no cache-excluding tag at all"
TAG_UNIVERSE_MSG = (
    "bazel tag guard read {queried}, but stage {stage!r} declares {declared} — a verdict "
    "from another universe is not this stage's gate"
)


@dataclasses.dataclass(frozen=True)
class RuleRecord:
    """One `RULE` record of `--output=streamed_jsonproto`, reduced to what C-011 judges."""

    label: str
    rule_class: str
    tags: frozenset[str]
    inputs: frozenset[str]
    """Every label this rule names in an attribute — jsonproto's `ruleInput`.

    One hop only, which is why `input_closure` exists: a `data = [":group"]`
    puts the *group's* label here and not the files inside it."""

    stamp_zero: bool
    """`stamp = 0` **explicitly specified**. rules_rust defaults `rust_binary`'s
    `stamp` to -1 and every other rust rule's to 0, so reading the value alone
    would be correct today and silently green every binary the day that default
    moves."""


# ---------------------------------------------------------------------------
# The I/O WP-13 left for this file.
# ---------------------------------------------------------------------------


def query_argv(bazel: str, universe: str) -> list[str]:
    """`kind(rule, ...)`, not `kind("^rust_.*rule$", ...)`.

    Cache-key soundness is a property of every rule, not only the Rust ones, so
    the classification reads the wider set; the *floor* is then counted over the
    Rust subset, which is what `STAGE_FLOORS` declares. One query, both numbers.
    """
    return [bazel, "query", f"kind(rule, {universe})", "--output=streamed_jsonproto"]


def run_query(bazel: str, universe: str) -> tuple[str, int, str]:
    """`(stdout, rc, stderr)`. An unreachable binary reads as rc 127 with a reason.

    No exception escapes: a missing `bazel` and a failed load must both arrive
    at the reader floor as "nothing was read", which is the state that gates.
    """
    try:
        result = subprocess.run(
            query_argv(bazel, universe),
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
            cwd=REPO_ROOT,
            timeout=900,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return "", 127, f"{type(error).__name__}: {error}"
    return result.stdout, result.returncode, result.stderr


def read_rules(text: str) -> tuple[list[RuleRecord], list[Finding]]:
    """Newline-delimited jsonproto -> records, plus any parse findings.

    Field presence and truth, never a schema: `bazel query`'s proto-to-JSON
    printer emits scalar defaults (`"intValue":0`, `"explicitlySpecified":false`)
    but omits an empty repeated field, so a target with no tags has a `tags`
    attribute carrying no `stringListValue` at all. A reader that took the key's
    absence for "attribute missing" would be right about the tags and wrong
    about everything else.
    """
    records: list[RuleRecord] = []
    findings: list[Finding] = []
    for number, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if not stripped:
            continue
        try:
            parsed = json.loads(stripped)
        except json.JSONDecodeError as error:
            findings.append(
                Finding("tag-query-unreadable", TAG_PARSE_MSG.format(line=number, error=error))
            )
            continue
        if not isinstance(parsed, dict) or parsed.get("type") != "RULE":
            continue
        rule = parsed.get("rule")
        if not isinstance(rule, dict):
            continue
        tags: set[str] = set()
        stamp_zero = False
        for attribute in rule.get("attribute", []):
            if not isinstance(attribute, dict):
                continue
            if attribute.get("name") == "tags":
                tags = {str(tag) for tag in attribute.get("stringListValue", [])}
            elif attribute.get("name") == "stamp":
                stamp_zero = bool(attribute.get("explicitlySpecified")) and (
                    attribute.get("intValue") == 0
                )
        records.append(
            RuleRecord(
                label=str(rule.get("name", "<unnamed>")),
                rule_class=str(rule.get("ruleClass", "<unknown>")),
                tags=frozenset(tags),
                inputs=frozenset(str(label) for label in rule.get("ruleInput", [])),
                stamp_zero=stamp_zero,
            )
        )
    return records, findings


def input_closure(records: list[RuleRecord]) -> dict[str, frozenset[str]]:
    """Each label's declared inputs, expanded through the rule targets among them.

    `ruleInput` is one hop: a target whose `data` names `:suite_inputs` lists
    that filegroup's label and not the 400-odd files inside it. Every filegroup
    in the universe is itself a `RULE` record with its own `ruleInput`, so the
    files are reachable without a second query — which matters, because a
    `deps(...)` query would be a different reading of a different universe and
    the two could disagree.

    Breadth-first with a visited set, so a cycle (which Bazel refuses anyway)
    cannot hang the gate, and so nesting depth is not a number this file has to
    know. The result includes the direct inputs as well as the expanded ones.
    """
    by_label = {record.label: record for record in records}
    closure: dict[str, frozenset[str]] = {}
    for record in records:
        seen: set[str] = set()
        queue = list(record.inputs)
        while queue:
            label = queue.pop()
            if label in seen:
                continue
            seen.add(label)
            nested = by_label.get(label)
            if nested is not None:
                queue.extend(nested.inputs)
        closure[record.label] = frozenset(seen)
    return closure


# ---------------------------------------------------------------------------
# Classification — the only judgement this file makes that WP-13 does not.
# ---------------------------------------------------------------------------


def is_test_rule(rule_class: str) -> bool:
    """A test action or a build action — the two have different credited tags."""
    return rule_class.endswith(TEST_RULE_SUFFIX)


def credited_tags(rule_class: str) -> frozenset[str]:
    """The allowlist this rule class's kind of action is judged against."""
    return CACHE_EXCLUDING_TAGS if is_test_rule(rule_class) else BUILD_ACTION_EXCLUDING_TAGS


def cache_excluding(tags: frozenset[str], rule_class: str) -> bool:
    """Does this tag set keep this kind of action's result off another machine?

    True for the one tag measured to do it *for that kind* — `external` on a test
    action, `no-remote-cache` on a build action. Everything else is False, so
    refusal is structural rather than a branch that could grow an exception, and
    a tag nobody has measured reaches no green path in either allowlist.

    Both allowlists have exactly one member, which is why this is two frozensets
    rather than a lookup table: the day a second tag is measured, it joins the
    set for the kind it was measured on and nowhere else.
    """
    return bool(tags & credited_tags(rule_class))


def module_shaped(pattern: str) -> bool:
    """Can this `glob`/`rglob` pattern only be matching acceptance modules?

    See `MODULE_SHAPED_PATTERN_PREFIX` for what this deliberately does not
    catch and why.
    """
    return pattern.endswith(MODULE_SHAPED_PATTERN_SUFFIX) or pattern.startswith(
        MODULE_SHAPED_PATTERN_PREFIX
    )


def sweeping_globs(source: str) -> list[str]:
    """Every module-shaped `glob`/`rglob` pattern literal in one module's source.

    `ast`, not a regex: the subject is a call with a string literal argument,
    and a regex over the text `.glob(` would also match it inside a docstring or
    a comment — a module that merely *documents* the sweep would then be handed
    181 inputs it never reads.
    """
    patterns: list[str] = []
    for node in ast.walk(ast.parse(source)):
        if not isinstance(node, ast.Call):
            continue
        function = node.func
        if not isinstance(function, ast.Attribute) or function.attr not in ("glob", "rglob"):
            continue
        if not node.args:
            continue
        first = node.args[0]
        if not (isinstance(first, ast.Constant) and isinstance(first.value, str)):
            continue
        if module_shaped(first.value):
            patterns.append(first.value)
    return sorted(set(patterns))


def sweep_requirements(sources: dict[str, str]) -> dict[str, frozenset[str]]:
    """`{module basename: the sibling module basenames its own source reads}`.

    Pure over already-read text, so the self-test can hand it a synthetic sixth
    sweeper and watch the clause fire without writing a file into `test/tests/`.
    A module with no module-shaped glob is absent from the result rather than
    present with an empty set — "reads nothing" and "reads nothing yet" must not
    be the same key.
    """
    names = sorted(sources)
    swept: dict[str, frozenset[str]] = {}
    for name in names:
        matched: set[str] = set()
        for pattern in sweeping_globs(sources[name]):
            matched |= {other for other in names if fnmatch.fnmatch(other, pattern)}
        if matched:
            swept[name] = frozenset(matched)
    return swept


def sweep_patterns(sources: dict[str, str]) -> dict[str, list[str]]:
    """The patterns behind `sweep_requirements`, for the finding's message."""
    return {name: sweeping_globs(text) for name, text in sources.items()}


def read_acceptance_sources() -> dict[str, str]:
    """`test/tests/test_*.py` as `{basename: text}`.

    An unreadable module is skipped rather than raising: this gate's job is the
    graph, and a source tree it cannot read must not turn a tag verdict into a
    traceback. The floor that keeps that from being silent is the caller's — a
    derivation over zero modules requires nothing of anybody.
    """
    sources: dict[str, str] = {}
    for path in sorted(ACCEPTANCE_TESTS_DIR.glob(SWEEP_MODULE_GLOB)):
        try:
            sources[path.name] = path.read_text(encoding="utf-8")
        except (OSError, SyntaxError):
            continue
    return sources


def module_label(basename: str) -> str:
    """`test_install.py` -> `//test:tests/test_install.py`, as `ruleInput` spells it."""
    return f"{ACCEPTANCE_PACKAGE}tests/{basename}"


def sweep_findings(
    *,
    acceptance: set[str],
    closure: dict[str, frozenset[str]],
    sweeps: dict[str, frozenset[str]],
    patterns: dict[str, list[str]],
) -> list[Finding]:
    """A sweeping module's target must declare the siblings its source reads."""
    findings: list[Finding] = []
    for basename in sorted(sweeps):
        label = ACCEPTANCE_PACKAGE + basename[: -len(MODULE_SHAPED_PATTERN_SUFFIX)]
        if label not in acceptance:
            continue
        declared = closure.get(label, frozenset())
        missing = sorted(module_label(other) for other in sweeps[basename]) 
        missing = [name for name in missing if name not in declared]
        if not missing:
            continue
        findings.append(
            Finding(
                "tag-acceptance-unswept",
                TAG_UNSWEPT_MSG.format(
                    label=label,
                    patterns=patterns.get(basename, []),
                    count=len(missing),
                    sample=missing[0],
                ),
            )
        )
    return findings


def suite_inputs_findings(
    *, records: list[RuleRecord], closure: dict[str, frozenset[str]]
) -> list[Finding]:
    """What the shared group itself must contain — three readings, one subject."""
    if SUITE_INPUTS_LABEL not in {record.label for record in records}:
        return [
            Finding(
                "tag-suite-inputs-absent",
                TAG_SUITE_INPUTS_ABSENT_MSG.format(label=SUITE_INPUTS_LABEL),
            )
        ]
    findings: list[Finding] = []
    members = closure.get(SUITE_INPUTS_LABEL, frozenset())

    missing_files = sorted(SUITE_INPUTS_REQUIRED_FILES - members)
    if missing_files:
        findings.append(
            Finding(
                "tag-suite-inputs-file",
                TAG_SUITE_INPUTS_FILE_MSG.format(label=SUITE_INPUTS_LABEL, missing=missing_files),
            )
        )

    covered = {
        directory
        for directory in SUITE_INPUTS_REQUIRED_DIRS
        for member in members
        if member.startswith(ACCEPTANCE_PACKAGE + directory)
    }
    missing_dirs = sorted(SUITE_INPUTS_REQUIRED_DIRS - covered)
    if missing_dirs:
        findings.append(
            Finding(
                "tag-suite-inputs-directory",
                TAG_SUITE_INPUTS_DIR_MSG.format(label=SUITE_INPUTS_LABEL, missing=missing_dirs),
            )
        )

    # `bin/**` is globbed `allow_empty = True` because `cargo` writes it and a
    # fresh clone has none, so counting it would make this floor two lower on a
    # clean checkout than on a built one — a number that means two things is not
    # a floor.
    counted = len({member for member in members if not member.startswith(ACCEPTANCE_PACKAGE + "bin/")})
    if counted < SUITE_INPUTS_FLOOR:
        findings.append(
            Finding(
                "tag-suite-inputs-floor",
                TAG_SUITE_INPUTS_FLOOR_MSG.format(
                    label=SUITE_INPUTS_LABEL, read=counted, floor=SUITE_INPUTS_FLOOR
                ),
            )
        )
    return findings


def check_tags(
    *,
    stage: str,
    records: list[RuleRecord],
    queried_universe: str | None,
    sweeps: dict[str, frozenset[str]] | None = None,
    patterns: dict[str, list[str]] | None = None,
) -> list[Finding]:
    """The three clauses, through the imported comparator, plus one correction.

    The correction: `tag_guard`'s own `no-sandbox` message names
    `no-remote-cache/no-remote/local` as the escapes, and WP-35 has since
    measured all three insufficient. A gate whose output instructs a fix that
    has been measured not to fix it is worse than a silent one, so **every**
    clause-1 red carries a second finding naming the tag that was measured to
    work — not only the ones that already carry a refuted tag, because the bare
    case is where the wrong advice is the only advice. The patch to WP-13's
    constant is owed separately; this file may not edit it.
    """
    findings: list[Finding] = []
    floor = STAGE_FLOORS.get(stage)

    if floor is not None and queried_universe is not None and queried_universe != floor.universe:
        findings.append(
            Finding(
                "tag-universe-override",
                TAG_UNIVERSE_MSG.format(
                    queried=queried_universe, stage=stage, declared=floor.universe
                ),
            )
        )

    tags_of = {record.label: record.tags for record in records}
    class_of = {record.label: record.rule_class for record in records}
    no_sandbox = {label for label, tags in tags_of.items() if NO_SANDBOX_TAG in tags}
    cache_excluded = {
        label for label, tags in tags_of.items() if cache_excluding(tags, class_of[label])
    }
    # The one narrowed clause, and the reason it is narrowed rather than holed.
    # The acceptance package runs with its results CACHED by decision, so it
    # carries no `external` and the blanket rule above would red all 181 of its
    # targets. What replaces `external` for them is a *declaration*: the binary
    # under test and the compose definition among their inputs. That is the
    # compensating control — the day a target loses either, it stops being
    # exempt and the blanket finding fires again.
    closure = input_closure(records)
    acceptance = {
        label
        for label in no_sandbox
        if label.startswith(ACCEPTANCE_PACKAGE) and is_test_rule(class_of[label])
    }
    cache_excluded |= {
        label
        for label in acceptance
        if ACCEPTANCE_DECLARED_INPUTS <= closure.get(label, frozenset())
    }
    # The three acceptance-only clauses, and each one closes a hole the clause
    # above cannot see. They run only where the acceptance package is in the
    # universe: at stage 1 and stage 3 it is not, and a clause with no subject
    # must say nothing rather than red.
    if acceptance:
        if sweeps is None or patterns is None:
            sources = read_acceptance_sources()
            sweeps = sweep_requirements(sources) if sweeps is None else sweeps
            patterns = sweep_patterns(sources) if patterns is None else patterns
        findings.extend(
            sweep_findings(
                acceptance=acceptance, closure=closure, sweeps=sweeps, patterns=patterns
            )
        )
        findings.extend(suite_inputs_findings(records=records, closure=closure))
        for label in sorted(acceptance):
            present = sorted(tags_of[label] & ACCEPTANCE_SUPPRESSING_TAGS)
            if present:
                findings.append(
                    Finding(
                        "tag-acceptance-suppressed",
                        TAG_SUPPRESSED_MSG.format(
                            label=label,
                            present=present,
                            forbidden=sorted(ACCEPTANCE_SUPPRESSING_TAGS),
                        ),
                    )
                )

    rust_binaries = {r.label for r in records if r.rule_class == "rust_binary"}
    # Clause 2's escapes, and clause 2's only. `rust_binary` is a build rule, so
    # `cache_excluded` now credits `no-remote-cache` for it anyway and this set
    # looks redundant — it is not. Clause 2's contract is "excused from the stamp
    # clause", clause 1's is "the result stays off other machines", and an
    # explicit `stamp = 0` discharges the first without touching the second.
    # Keeping them separate is what makes `stamp = 0` alone green clause 2 and
    # still red clause 1 on a no-sandbox binary.
    stamp_excused = {
        r.label for r in records if r.stamp_zero or STAMP_ESCAPE_TAG in r.tags
    }

    findings.extend(
        tag_guard(
            stage=stage,
            no_sandbox=no_sandbox,
            cache_excluded=cache_excluded,
            rust_binaries=rust_binaries,
            stamp_zero=stamp_excused,
            rule_targets_read=len(records),
        )
    )

    for label in sorted(no_sandbox - cache_excluded):
        rule_class = class_of[label]
        credited = min(credited_tags(rule_class))
        if label in acceptance:
            missing = sorted(ACCEPTANCE_DECLARED_INPUTS - closure.get(label, frozenset()))
            findings.append(
                Finding(
                    "tag-acceptance-undeclared",
                    TAG_ACCEPTANCE_MSG.format(label=label, credited=credited, missing=missing),
                )
            )
            continue
        # Safe for both kinds: a target reaching this loop does not carry its own
        # credited tag, so `no-remote-cache` can never be listed as insufficient
        # for the build action it is the answer for.
        present = sorted(tags_of[label] & INSUFFICIENT_TAGS)
        carries = TAG_CARRIES_SOME.format(present=present) if present else TAG_CARRIES_NONE
        template = (
            TAG_INSUFFICIENT_TEST_MSG if is_test_rule(rule_class) else TAG_INSUFFICIENT_BUILD_MSG
        )
        findings.append(
            Finding(
                "tag-cache-insufficient",
                template.format(label=label, carries=carries, credited=credited),
            )
        )
    return findings


@dataclasses.dataclass(frozen=True)
class Census:
    """What the run read, and what each clause had to judge in it.

    `quality-core.md` § "A green is only as wide as what ran": the gate used to
    print the reader's scope and omit the two numbers that were zero, so a green
    over two clauses with no subject read exactly like a green over two clauses
    that passed. Both counts are now on the line `main()` prints.
    """

    rule_targets: int
    """Every rule target of every kind — what `STAGE_FLOORS` floors."""

    rust_targets: int
    """The Rust subset, reported because `//crates/...` is where it lives."""

    no_sandbox: int
    """Clause 1's subject count."""

    rust_binaries: int
    """Clause 2's subject count."""

    acceptance: int
    """The narrowed clause's subject count: `no-sandbox` test targets in
    `//test:`. Printed for the reason the other two are — a run whose universe
    stopped short of the acceptance package would judge zero of them and read
    exactly like a run where all 181 passed."""

    def line(self) -> str:
        return (
            f"{self.rule_targets} rule targets read ({self.rust_targets} Rust) — "
            f"{self.no_sandbox} no-sandbox, of those {self.acceptance} acceptance, "
            f"{self.rust_binaries} rust_binary, so that is what the clauses judged"
        )


def census_of(records: list[RuleRecord]) -> Census:
    return Census(
        rule_targets=len(records),
        rust_targets=len([r for r in records if r.rule_class.startswith(RUST_RULE_PREFIX)]),
        no_sandbox=len([r for r in records if NO_SANDBOX_TAG in r.tags]),
        rust_binaries=len([r for r in records if r.rule_class == "rust_binary"]),
        acceptance=len(
            [
                r
                for r in records
                if NO_SANDBOX_TAG in r.tags
                and r.label.startswith(ACCEPTANCE_PACKAGE)
                and is_test_rule(r.rule_class)
            ]
        ),
    )


def run_check(
    *,
    stage: str,
    bazel: str,
    universe: str | None = None,
    query_json: Path | None = None,
    sweeps: dict[str, frozenset[str]] | None = None,
    patterns: dict[str, list[str]] | None = None,
) -> tuple[list[Finding], str | None, Census]:
    """`(findings, universe actually read, the census of what was read)`.

    An undeclared stage never reaches the query: there is no floor to read it
    against, so running one would produce a verdict nothing could fail.
    """
    floor = STAGE_FLOORS.get(stage)
    if floor is None and universe is None:
        return (
            tag_guard(
                stage=stage,
                no_sandbox=set(),
                cache_excluded=set(),
                rust_binaries=set(),
                stamp_zero=set(),
                rule_targets_read=0,
            ),
            None,
            census_of([]),
        )

    queried = universe or (floor.universe if floor is not None else "//...")
    findings: list[Finding] = []

    if query_json is not None:
        try:
            text = query_json.read_text(encoding="utf-8")
        except OSError as error:
            text = ""
            findings.append(
                Finding(
                    "tag-query-failed",
                    TAG_QUERY_MSG.format(universe=queried, rc="-", detail=str(error)),
                )
            )
    else:
        text, rc, stderr = run_query(bazel, queried)
        if rc != 0:
            findings.append(
                Finding(
                    "tag-query-failed",
                    TAG_QUERY_MSG.format(
                        universe=queried, rc=rc, detail=" | ".join(stderr.strip().splitlines()[-3:])
                    ),
                )
            )

    records, parse_findings = read_rules(text)
    findings.extend(parse_findings)
    findings.extend(
        check_tags(
            stage=stage,
            records=records,
            queried_universe=queried,
            sweeps=sweeps,
            patterns=patterns,
        )
    )
    return findings, queried, census_of(records)


# ---------------------------------------------------------------------------
# Self-test.
# ---------------------------------------------------------------------------


def expect(condition: bool, problem: str) -> None:
    """A loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not condition:
        raise SystemExit(f"bazel tag guard self-test: {problem}")


def _no_prefix_reader(tags: frozenset[str]) -> bool:
    """The wrong tag reader, kept as a named control and called from nowhere else.

    "Any `no-*` tag means it will not be cached" is the rule a reader reaches
    for, and measurement refutes it: `bazel_accept_proofs.py --prove-s015` shows
    a target tagged `local, exclusive, no-cache` reporting `(cached)` on the
    second run in the same output base. This reader calls that target safe on
    the exact bytes `cache_excluding` reds, which is what makes the measurement
    load-bearing rather than decorative.
    """
    return any(tag.startswith("no-") for tag in tags) or "local" in tags


def _only(findings: list[Finding], code: str) -> Finding:
    """The one finding carrying `code`. Selected by code, never by position —
    `codes()` is sorted and the findings list is not, and a proof that printed
    `findings[0]` would label whichever happened to be emitted first."""
    matches = [finding for finding in findings if finding.code == code]
    expect(len(matches) == 1, f"expected exactly one {code} finding, got {len(matches)}")
    return matches[0]


def _load(path: Path) -> list[dict]:
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def _dump(path: Path, records: list[dict]) -> None:
    path.write_text(
        "".join(json.dumps(record) + "\n" for record in records), encoding="utf-8"
    )


def _attr(record: dict, name: str) -> dict:
    for attribute in record["rule"]["attribute"]:
        if attribute.get("name") == name:
            return attribute
    raise SystemExit(f"bazel tag guard self-test: {record['rule']['name']} has no {name!r} attribute")


def _mutate(
    source: list[dict],
    label: str,
    *,
    tags: list[str] | None = None,
    rule_class: str | None = None,
    stamp: tuple[int, bool] | None = None,
    rename: str | None = None,
    drop_inputs: set[str] | None = None,
) -> list[dict]:
    """A deep copy of the live capture with one named target rewritten."""
    records = json.loads(json.dumps(source))
    hits = 0
    for record in records:
        if record.get("rule", {}).get("name") != label:
            continue
        hits += 1
        if tags is not None:
            attribute = _attr(record, "tags")
            attribute["stringListValue"] = list(tags)
            attribute["explicitlySpecified"] = True
        if rule_class is not None:
            record["rule"]["ruleClass"] = rule_class
        if stamp is not None:
            attribute = _attr(record, "stamp")
            attribute["intValue"], attribute["explicitlySpecified"] = stamp
        if drop_inputs is not None:
            record["rule"]["ruleInput"] = [
                name for name in record["rule"].get("ruleInput", []) if name not in drop_inputs
            ]
        if rename is not None:
            record["rule"]["name"] = rename
    expect(hits == 1, f"{label} appears {hits} times in the capture, expected exactly 1")
    return records


def _judge(
    path: Path,
    records: list[dict],
    *,
    stage: str = "stage-4",
    sweeps: dict[str, frozenset[str]] | None = None,
    patterns: dict[str, list[str]] | None = None,
) -> list[Finding]:
    """Write a mutated stream, re-read it from disk, and judge it through `run_check`.

    Through the shipped entry point, never through the imported comparator: the
    reader is half of what this file owns, and a proof that skips it proves the
    other WP's function.
    """
    _dump(path, records)
    findings, _, _ = run_check(
        stage=stage, bazel="", query_json=path, sweeps=sweeps, patterns=patterns
    )
    return findings


def _landed(path: Path, label: str) -> RuleRecord:
    """The mutation as the *reader* sees it, re-read from disk. A mutation whose
    result is trusted before it is proven to have landed is the defect this
    whole corpus is written against."""
    records, parse = read_rules(path.read_text(encoding="utf-8"))
    expect(parse == [], f"the mutated capture does not parse: {[f.message for f in parse]}")
    for record in records:
        if record.label == label:
            return record
    raise SystemExit(f"bazel tag guard self-test: {label} is absent from the mutated capture")


def capture_live(bazel: str, scratch: Path) -> tuple[Path, list[dict]]:
    """The one live reading every proof below is built on: `bazel query` over the
    **stage-4** universe, on this tree, right now.

    The stage advances with the gate's default, and for the same reason it did
    at stage 3: a self-test whose capture cannot contain its subject can only
    ever judge synthetic bytes. Stage 1 (`//crates/...`) held no `no-sandbox`
    target and no `rust_binary`; stage 3 added the 40 `no-sandbox` genrules but
    not one acceptance target, so the narrowed acceptance clause would have had
    nothing live to be proven against."""
    universe = STAGE_FLOORS["stage-4"].universe
    text, rc, stderr = run_query(bazel, universe)
    expect(
        rc == 0,
        f"`bazel query 'kind(rule, {universe})'` exited {rc}. This gate's subject is the live "
        "graph, so a self-test that greened without reading it would be the vacuous green "
        "this plan exists to prevent. On a fresh checkout or a linked worktree the cause is "
        "crate_universe's gitignored Cargo.bazel.lock.json — repin, or copy it in. stderr: "
        + " | ".join(stderr.strip().splitlines()[-4:]),
    )
    path = scratch / "live_capture.json"
    path.write_text(text, encoding="utf-8")
    records = _load(path)
    expect(len(records) > 0, "the live query produced no records but exited 0")
    return path, records


def prove_no_sandbox(scratch: Path, live: list[dict]) -> int:
    """Mode 1 (C-011 clause 1) — a `no-sandbox` target without a cache-excluding tag."""
    checks = 0
    path = scratch / "mode1.json"
    victim = next(r["rule"]["name"] for r in live if r["rule"]["ruleClass"] == "rust_test")

    tagged = [r for r in live if _attr(r, "tags").get("stringListValue")]
    no_sandbox = [r for r in live if NO_SANDBOX_TAG in set(_attr(r, "tags").get("stringListValue", []))]
    build_actions = [r for r in no_sandbox if not is_test_rule(r["rule"]["ruleClass"])]
    print(
        f"S-012 mode 1 — the live graph carries {len(tagged)} tagged target(s) of {len(live)}, "
        f"{len(no_sandbox)} of them no-sandbox: {len(build_actions)} build actions "
        "(`prove_build_action` judges those as themselves) and the acceptance package's test "
        "actions, which are the narrowed clause's subject (`prove_acceptance`). So the "
        "BLANKET test half has no live subject at all, and this proof mutates a real "
        "`rust_test` to reach it"
    )

    green = _judge(path, live)
    expect(
        "tag-no-sandbox" not in codes(green),
        f"the unmutated live capture must not red clause 1, got {codes(green)}",
    )
    print(f"S-012 mode 1 GREEN: the live capture, unmutated — no clause-1 finding ({len(live)} targets)")
    checks += 1

    bare = _mutate(live, victim, tags=[NO_SANDBOX_TAG])
    findings = _judge(path, bare)
    landed = _landed(path, victim)
    expect(landed.tags == frozenset({NO_SANDBOX_TAG}), f"the no-sandbox mutation did not land: {landed.tags}")
    expect(
        codes(findings) == ["tag-cache-insufficient", "tag-no-sandbox"],
        f"got {codes(findings)}",
    )
    expect(victim in _only(findings, "tag-no-sandbox").message, "the finding does not name the label")
    print(f"S-012 mode 1 RED  : {_only(findings, 'tag-no-sandbox').message}")
    print(f"S-012 mode 1 RED  : {_only(findings, 'tag-cache-insufficient').message}")
    checks += 1

    weak = _mutate(live, victim, tags=[NO_SANDBOX_TAG, "no-cache", "local", "no-remote-cache"])
    findings = _judge(path, weak)
    landed = _landed(path, victim)
    expect("no-cache" in landed.tags and "local" in landed.tags, f"the weak-tag mutation did not land: {landed.tags}")
    expect(
        codes(findings) == ["tag-cache-insufficient", "tag-no-sandbox"],
        f"a tag set measured insufficient must be refused, not greened — got {codes(findings)}",
    )
    expect(
        _no_prefix_reader(landed.tags) and not cache_excluding(landed.tags, landed.rule_class),
        "the control reader must call these same bytes safe, or the measurement is decorative",
    )
    print(f"S-012 mode 1 RED  : {_only(findings, 'tag-cache-insufficient').message}")
    print(
        "S-012 mode 1 CONTROL: `_no_prefix_reader` (any no-* tag, or local) calls that exact "
        "target safe — the tag set is refused because it was measured, not because it looks wrong"
    )
    checks += 2

    credited = _mutate(live, victim, tags=[NO_SANDBOX_TAG, CACHE_DEFEATING_TAG])
    findings = _judge(path, credited)
    landed = _landed(path, victim)
    expect(CACHE_DEFEATING_TAG in landed.tags, f"the external-tag mutation did not land: {landed.tags}")
    expect(findings == [], f"the measured tag must green the same target, got {codes(findings)}")
    print(
        f"S-012 mode 1 GREEN: the same TEST target tagged `{NO_SANDBOX_TAG}, {CACHE_DEFEATING_TAG}` "
        "is silent — clause 1 is not simply always red"
    )
    checks += 1

    # The kind split, from the test side: the build action's credited tag does
    # not credit a test action. Without this the split could have been written as
    # "either tag will do", which is a union, not a split.
    wrong_kind = _mutate(live, victim, tags=[NO_SANDBOX_TAG, STAMP_ESCAPE_TAG])
    findings = _judge(path, wrong_kind)
    landed = _landed(path, victim)
    expect(landed.tags == frozenset({NO_SANDBOX_TAG, STAMP_ESCAPE_TAG}), f"did not land: {landed.tags}")
    expect(
        codes(findings) == ["tag-cache-insufficient", "tag-no-sandbox"],
        f"a TEST action tagged `{STAMP_ESCAPE_TAG}` must still red — that tag is the BUILD "
        f"action's answer and WP-35 measured it insufficient for a test result; got {codes(findings)}",
    )
    expect(
        "TEST action" in _only(findings, "tag-cache-insufficient").message,
        "the advice does not name the kind it was written for",
    )
    print(f"S-012 mode 1 RED  : {_only(findings, 'tag-cache-insufficient').message}")
    checks += 1
    return checks


def prove_build_action(scratch: Path, live: list[dict]) -> int:
    """Mode 1, BUILD half — the 40 live `no-sandbox` genrules, judged as themselves.

    This is the half that had no gate at all until stage 3 was declared: the
    guard's universe stopped at `//crates/...`, and every `no-sandbox` target in
    the graph is in `//test/doc_scripts/...`. It is also the half whose *credited
    tag* was wrong — clause 1 demanded `external` of them, and `external` is
    measured inert on a build action, so the only fix the gate would have
    accepted was a decoration.
    """
    checks = 0
    path = scratch / "mode1b.json"
    genrules = [r for r in live if r["rule"]["ruleClass"] == "genrule"]
    expect(
        len(genrules) >= CAST_GENRULE_TARGETS,
        f"the live capture holds {len(genrules)} genrules, below the measured "
        f"{CAST_GENRULE_TARGETS} — this proof's subject is the real cast targets",
    )
    victim = genrules[0]["rule"]["name"]
    tags = frozenset(_attr(genrules[0], "tags").get("stringListValue", []))
    expect(
        NO_SANDBOX_TAG in tags and STAMP_ESCAPE_TAG in tags,
        f"{victim} carries {sorted(tags)} — this proof assumed a no-sandbox build action "
        f"already carrying {STAMP_ESCAPE_TAG}",
    )

    green = _judge(path, live)
    expect(green == [], f"the unmutated live capture must be silent, got {[f.message for f in green]}")
    no_sandbox = [
        r for r in genrules if NO_SANDBOX_TAG in set(_attr(r, "tags").get("stringListValue", []))
    ]
    print(
        f"S-012 mode 1b GREEN: {len(genrules)} live genrules, {len(no_sandbox)} of them "
        f"no-sandbox and all of those carrying `{STAMP_ESCAPE_TAG}` — silent, and they are read "
        "at all only because stage 3 exists. The rest are WP-33b's GIF renders, which carry no "
        "tag and so are not clause 1's subject"
    )
    checks += 1

    stripped = _mutate(live, victim, tags=[NO_SANDBOX_TAG, "requires-network"])
    findings = _judge(path, stripped)
    landed = _landed(path, victim)
    expect(STAMP_ESCAPE_TAG not in landed.tags, f"the tag-stripping mutation did not land: {landed.tags}")
    expect(
        codes(findings) == ["tag-cache-insufficient", "tag-no-sandbox"],
        f"a no-sandbox BUILD action with no cache-excluding tag must red, got {codes(findings)}",
    )
    message = _only(findings, "tag-cache-insufficient").message
    expect("BUILD action" in message, "the advice does not name the kind it was written for")
    expect(
        f"`{STAMP_ESCAPE_TAG}`" in message and "NOT `external`" in message,
        "the advice must name the measured tag and refuse the one measured inert",
    )
    print(f"S-012 mode 1b RED  : {_only(findings, 'tag-no-sandbox').message}")
    print(f"S-012 mode 1b RED  : {message}")
    checks += 1

    decorated = _mutate(live, victim, tags=[NO_SANDBOX_TAG, CACHE_DEFEATING_TAG])
    findings = _judge(path, decorated)
    landed = _landed(path, victim)
    expect(landed.tags == frozenset({NO_SANDBOX_TAG, CACHE_DEFEATING_TAG}), f"did not land: {landed.tags}")
    expect(
        codes(findings) == ["tag-cache-insufficient", "tag-no-sandbox"],
        f"`{CACHE_DEFEATING_TAG}` on a BUILD action must NOT green clause 1 — WP-33 measured "
        f"it inert there, over four tag sets, warm and on a fresh server; got {codes(findings)}",
    )
    print(
        f"S-012 mode 1b RED  : the same genrule tagged `{CACHE_DEFEATING_TAG}` is still refused "
        "— crediting it would be the decoration BZL-CORE-01 names, not a fix"
    )
    checks += 1

    restored = _judge(path, live)
    expect(restored == [], "restoring the live capture from our own bytes must green again")
    print("S-012 mode 1b GREEN: the capture restored from this file's own bytes is silent again")
    checks += 1
    return checks


def prove_rust_binary(scratch: Path, live: list[dict]) -> int:
    """Mode 2 (C-011 clause 2) — a `rust_binary` with neither per-target escape.

    Zero `rust_binary` targets exist at stage 1, so the clause is vacuous by
    absence. It is kept alive the only way an absent subject can be: the live
    capture's own bytes, with one real record retyped to the kind that does not
    exist yet and its `stamp` set to the default rules_rust measurably gives a
    `rust_binary` (-1). The day a real one arrives, `run_check` reads it through
    this same path with no change.
    """
    checks = 0
    path = scratch / "mode2.json"
    victim = next(r["rule"]["name"] for r in live if r["rule"]["ruleClass"] == "rust_library")

    binaries = [r for r in live if r["rule"]["ruleClass"] == "rust_binary"]
    expect(
        binaries == [],
        f"the live graph now has {len(binaries)} rust_binary target(s); this proof assumed 0 "
        "and must be re-read against the real ones",
    )
    print(
        "S-012 mode 2 — the live graph has 0 rust_binary targets (measured, not assumed), so "
        "clause 2 is vacuous by absence; every state below is one real record retyped"
    )

    green = _judge(path, live)
    expect("tag-stamp" not in codes(green), f"the unmutated live capture must not red clause 2, got {codes(green)}")
    print("S-012 mode 2 GREEN: the live capture, unmutated — no clause-2 finding")
    checks += 1

    unstamped = _mutate(live, victim, rule_class="rust_binary", stamp=(-1, False))
    findings = _judge(path, unstamped)
    landed = _landed(path, victim)
    expect(landed.rule_class == "rust_binary", f"the retype did not land: {landed.rule_class}")
    expect(not landed.stamp_zero, "the stamp = -1 mutation did not land")
    expect(codes(findings) == ["tag-stamp"], f"got {codes(findings)}")
    expect(victim in findings[0].message, "the finding does not name the binary")
    print(f"S-012 mode 2 RED  : {_only(findings, 'tag-stamp').message}")
    checks += 1

    stamped = _mutate(live, victim, rule_class="rust_binary", stamp=(0, True))
    findings = _judge(path, stamped)
    landed = _landed(path, victim)
    expect(landed.stamp_zero, "the explicit stamp = 0 mutation did not land")
    expect(findings == [], f"an explicit stamp = 0 must green the binary, got {codes(findings)}")
    print("S-012 mode 2 GREEN: the same target with `stamp = 0` explicitly specified is silent")
    checks += 1

    implicit = _mutate(live, victim, rule_class="rust_binary", stamp=(0, False))
    findings = _judge(path, implicit)
    landed = _landed(path, victim)
    expect(
        not landed.stamp_zero,
        "the reader credits a stamp = 0 that arrived as a rules_rust default — the "
        "explicitlySpecified requirement is gone, and every binary greens the day that "
        "default moves",
    )
    expect(
        codes(findings) == ["tag-stamp"],
        f"a defaulted stamp = 0 is not the rule attribute the contract names — got {codes(findings)}",
    )
    print(
        "S-012 mode 2 RED  : the same value arriving as a rules_rust default rather than as a "
        "rule attribute is refused — rust_binary's measured default is -1 (rust.bzl:1321), and "
        "crediting an unspecified 0 would green every binary the day that default moves"
    )
    checks += 1

    tagged = _mutate(live, victim, rule_class="rust_binary", stamp=(-1, False), tags=[STAMP_ESCAPE_TAG])
    findings = _judge(path, tagged)
    landed = _landed(path, victim)
    expect(landed.tags == frozenset({STAMP_ESCAPE_TAG}), f"the no-remote-cache mutation did not land: {landed.tags}")
    expect(findings == [], f"the no-remote-cache tag must green clause 2, got {codes(findings)}")
    print(f"S-012 mode 2 GREEN: the same unstamped binary tagged `{STAMP_ESCAPE_TAG}` is silent")
    checks += 1

    # The routing proof: one target, both clauses, opposite answers. The pair is
    # `stamp = 0` now rather than `no-remote-cache` — a rust_binary is a BUILD
    # action, so that tag is its credited clause-1 answer too and the two clauses
    # agree on it. An explicit `stamp = 0` still discharges only clause 2, which
    # is what keeps the two escapes from collapsing into one set.
    both = _mutate(
        live,
        victim,
        rule_class="rust_binary",
        stamp=(0, True),
        tags=[NO_SANDBOX_TAG],
    )
    findings = _judge(path, both)
    landed = _landed(path, victim)
    expect(landed.stamp_zero and NO_SANDBOX_TAG in landed.tags, "the paired mutation did not land")
    expect(
        codes(findings) == ["tag-cache-insufficient", "tag-no-sandbox"],
        f"clause 2 must green and clause 1 must red on this target — got {codes(findings)}",
    )
    print(
        "S-012 mode 2 RED  : a rust_binary with `stamp = 0` tagged `no-sandbox` is green on "
        "clause 2 and red on clause 1 — feeding `stamp_zero` through `cache_excluded` would "
        "have greened both, and an unsound cache key is not a stamping question"
    )
    checks += 1

    # The kind split reaching clause 2: `external` is a test action's answer and
    # credits a rust_binary with neither escape nowhere. Before the split it did.
    mislabelled = _mutate(
        live,
        victim,
        rule_class="rust_binary",
        stamp=(-1, False),
        tags=[CACHE_DEFEATING_TAG],
    )
    findings = _judge(path, mislabelled)
    landed = _landed(path, victim)
    expect(landed.tags == frozenset({CACHE_DEFEATING_TAG}), f"did not land: {landed.tags}")
    expect(
        codes(findings) == ["tag-stamp"],
        f"`{CACHE_DEFEATING_TAG}` on an unstamped rust_binary must still red clause 2 — it is "
        f"the TEST action's tag and buys a build action nothing; got {codes(findings)}",
    )
    print(f"S-012 mode 2 RED  : {_only(findings, 'tag-stamp').message}")
    checks += 1

    print(
        "S-012 mode 2 NOTE : no input to `run_check` is a command-line flag, so no value of "
        "`--nostamp` reaches either finding — which is the plan's [R1] correction, shown"
    )
    return checks


def prove_acceptance(scratch: Path, live: list[dict]) -> int:
    """The narrowed clause — the acceptance package's `no-sandbox` exemption.

    The blanket rule is "a `no-sandbox` target carries the cache-excluding tag
    its kind was measured to need". The acceptance package violates it on
    purpose: its results are cached by decision, so `external` is exactly the
    tag it must not carry. What stands in for the tag is a *declaration* — the
    binary under test and the compose definition among the target's inputs —
    and that is only a compensating control if losing it reds. Every case below
    runs on the live capture's own bytes, because a synthetic acceptance record
    would prove the fixture rather than the graph.
    """
    checks = 0
    path = scratch / "mode4.json"
    acceptance = [
        r
        for r in live
        if r["rule"]["name"].startswith(ACCEPTANCE_PACKAGE)
        and is_test_rule(r["rule"]["ruleClass"])
        and NO_SANDBOX_TAG in set(_attr(r, "tags").get("stringListValue", []))
    ]
    expect(
        len(acceptance) == ACCEPTANCE_MODULE_TARGETS,
        f"the live capture carries {len(acceptance)} no-sandbox acceptance target(s), "
        f"{ACCEPTANCE_MODULE_TARGETS} expected — a clause with no subject cannot be proven",
    )
    victim = acceptance[0]["rule"]["name"]
    group = "//test:suite_inputs"

    green = _judge(path, live)
    expect(
        "tag-acceptance-undeclared" not in codes(green) and "tag-no-sandbox" not in codes(green),
        f"the unmutated live capture must not red the narrowed clause, got {codes(green)}",
    )
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    closure = input_closure(reread)
    expect(
        ACCEPTANCE_DECLARED_INPUTS <= closure[victim],
        f"{victim}'s input closure is missing "
        f"{sorted(ACCEPTANCE_DECLARED_INPUTS - closure[victim])} — the green above is about a "
        "target that never satisfied the clause",
    )
    print(
        f"S-012 mode 4 GREEN: {len(acceptance)} no-sandbox acceptance target(s) carry no "
        f"`{CACHE_DEFEATING_TAG}` and are silent — each declares {sorted(ACCEPTANCE_DECLARED_INPUTS)} "
        "through its input closure"
    )
    checks += 1

    # One target drops the shared group: exactly one red, and it names the two
    # labels that went with it.
    dropped = _mutate(live, victim, drop_inputs={group})
    findings = _judge(path, dropped)
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    expect(
        group not in {r.label: r.inputs for r in reread}[victim],
        "the input-drop mutation did not land",
    )
    expect(
        codes(findings) == ["tag-acceptance-undeclared", "tag-no-sandbox"],
        f"got {codes(findings)}",
    )
    undeclared = [f for f in findings if f.code == "tag-acceptance-undeclared"]
    expect(len(undeclared) == 1, f"{len(undeclared)} targets red, expected exactly the one mutated")
    expect(victim in undeclared[0].message, "the finding does not name the label")
    expect(
        "['//test:docker-compose.yml', '//test:suite_anchor']" in undeclared[0].message,
        "the finding does not name what is missing",
    )
    print(f"S-012 mode 4 RED  : {undeclared[0].message}")
    checks += 1

    # The group keeps its label and loses one source. This is the case a
    # one-hop reader cannot see, and it reds all 181 rather than one: the
    # compose definition left the closure of every acceptance target at once.
    thinned = _mutate(live, group, drop_inputs={"//test:docker-compose.yml"})
    findings = _judge(path, thinned)
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    expect(
        "//test:docker-compose.yml" not in {r.label: r.inputs for r in reread}[group],
        "the group-thinning mutation did not land",
    )
    undeclared = [f for f in findings if f.code == "tag-acceptance-undeclared"]
    expect(
        len(undeclared) == ACCEPTANCE_MODULE_TARGETS,
        f"{len(undeclared)} acceptance target(s) red, expected all {ACCEPTANCE_MODULE_TARGETS} — "
        "the reader stopped at one hop and never expanded the group",
    )
    # On the `missing` list, never on the sentence: the advice text names both
    # groups by construction, so a substring assertion over the message would
    # pass whichever label had actually gone.
    expect(
        "['//test:docker-compose.yml']" in undeclared[0].message,
        "the finding must name the one missing label and not the one still declared",
    )
    print(f"S-012 mode 4 RED  : {undeclared[0].message}")
    print(
        f"S-012 mode 4 RED  : and {len(undeclared) - 1} more — one source left one group and "
        "every acceptance target lost the declaration, which a one-hop reader would have missed"
    )
    checks += 1

    # The blanket escape still silences clause 1 on the same target — and no
    # longer buys a green, because `prove_suppressing_tags` below reds the tag
    # itself. Both halves are asserted here so neither can be read as the other:
    # the clause-1 codes are gone, and the acceptance-suppression code is there.
    tagged = _mutate(live, victim, tags=[NO_SANDBOX_TAG, "exclusive", CACHE_DEFEATING_TAG])
    tagged = _mutate(tagged, victim, drop_inputs={group})
    findings = _judge(path, tagged)
    landed = _landed(path, victim)
    expect(
        CACHE_DEFEATING_TAG in landed.tags and group not in landed.inputs,
        f"the tag-plus-drop mutation did not land: {sorted(landed.tags)}",
    )
    expect(
        codes(findings) == ["tag-acceptance-suppressed", "tag-acceptance-unswept"]
        or codes(findings) == ["tag-acceptance-suppressed"],
        f"got {codes(findings)}",
    )
    expect(
        "tag-no-sandbox" not in codes(findings)
        and "tag-acceptance-undeclared" not in codes(findings),
        f"`{CACHE_DEFEATING_TAG}` must still silence clause 1, got {codes(findings)}",
    )
    print(
        f"S-012 mode 4 RED  : the same target with `{CACHE_DEFEATING_TAG}` back silences clause 1 "
        "and reds the acceptance-suppression clause instead — the tag is the defect here, not "
        "the escape it is everywhere else in the graph"
    )
    checks += 1

    # The scope control. A `no-sandbox` test target OUTSIDE `//test:` carrying
    # the identical declaration must still red: the exemption is granted to the
    # acceptance package, never to whatever names those two labels.
    elsewhere = _mutate(live, victim, rename="//crates/ocx_exit:impostor_test")
    findings = _judge(path, elsewhere)
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    moved = {r.label: r for r in reread}["//crates/ocx_exit:impostor_test"]
    expect(
        ACCEPTANCE_DECLARED_INPUTS <= input_closure(reread)[moved.label],
        "the renamed target lost the declaration, so this case is not the one it says it is",
    )
    expect(
        codes(findings) == ["tag-cache-insufficient", "tag-no-sandbox"],
        f"a no-sandbox test target outside {ACCEPTANCE_PACKAGE} must be judged by the blanket "
        f"rule however it declares its inputs, got {codes(findings)}",
    )
    expect(
        moved.label in _only(findings, "tag-cache-insufficient").message,
        "the finding does not name the impostor",
    )
    print(f"S-012 mode 4 CONTROL: {_only(findings, 'tag-cache-insufficient').message}")
    checks += 1
    return checks


def prove_suppressing_tags(scratch: Path, live: list[dict]) -> int:
    """The amended stage-4 ruling, enforced from the graph.

    `external`, `local` and `no-cache` were each measured to suppress a cache
    hit for a test action, and the acceptance stage exists to reuse those hits.
    Until this clause there was no lane in which re-adding one reddened:
    `scripts/bazel_accept_proofs.py --check-s015` is the run-side reader of the
    same fact and it is invoked by nothing.
    """
    checks = 0
    path = scratch / "mode5.json"
    acceptance = [
        r
        for r in live
        if r["rule"]["name"].startswith(ACCEPTANCE_PACKAGE)
        and is_test_rule(r["rule"]["ruleClass"])
        and NO_SANDBOX_TAG in set(_attr(r, "tags").get("stringListValue", []))
    ]
    expect(len(acceptance) == ACCEPTANCE_MODULE_TARGETS, f"{len(acceptance)} acceptance targets")
    victim = acceptance[0]["rule"]["name"]

    green = _judge(path, live)
    expect(
        "tag-acceptance-suppressed" not in codes(green),
        f"the real tag set must be silent, got {codes(green)}",
    )
    live_tags = sorted(_attr(acceptance[0], "tags").get("stringListValue", []))
    print(
        f"S-012 mode 5 GREEN: all {len(acceptance)} acceptance targets carry {live_tags} — none "
        f"of {sorted(ACCEPTANCE_SUPPRESSING_TAGS)}"
    )
    checks += 1

    for tag in (CACHE_DEFEATING_TAG, "local", "no-cache"):
        mutated = _mutate(live, victim, tags=[NO_SANDBOX_TAG, "exclusive", tag])
        findings = _judge(path, mutated)
        landed = _landed(path, victim)
        expect(tag in landed.tags, f"the {tag} mutation did not land: {sorted(landed.tags)}")
        suppressed = [f for f in findings if f.code == "tag-acceptance-suppressed"]
        expect(len(suppressed) == 1, f"{len(suppressed)} targets red on {tag}, expected 1")
        expect(victim in suppressed[0].message, "the finding does not name the label")
        expect(f"'{tag}'" in suppressed[0].message, f"the finding does not name {tag}")
        print(f"S-012 mode 5 RED  : {suppressed[0].message}")
        checks += 1
    return checks


def prove_suite_inputs(scratch: Path, live: list[dict]) -> int:
    """The floor on what the SHARED group contains.

    The per-target clause requires `//test:suite_anchor` and
    `//test:docker-compose.yml` and nothing else, so narrowing the glob *inside*
    `//test:suite_inputs` leaves both in place and stays green. That is not a
    hypothetical: `bench/**` and `taskfile.yml` were read by acceptance modules
    and absent from the group for a whole wave, with every gate green. Each case
    below removes something the per-target clause cannot see.
    """
    checks = 0
    path = scratch / "mode6.json"
    group = SUITE_INPUTS_LABEL
    reread, _ = read_rules(json.dumps(live[0]) and "\n".join(json.dumps(r) for r in live))
    members = input_closure(reread)[group]

    green = _judge(path, live)
    expect(
        not [f for f in codes(green) if f.startswith("tag-suite-inputs")],
        f"the live group must be silent, got {codes(green)}",
    )
    counted = len({m for m in members if not m.startswith(ACCEPTANCE_PACKAGE + "bin/")})
    print(
        f"S-012 mode 6 GREEN: {group} expands to {counted} label(s) (bin/** excluded) against a "
        f"floor of {SUITE_INPUTS_FLOOR}, covering all {len(SUITE_INPUTS_REQUIRED_DIRS)} required "
        f"directories and all {len(SUITE_INPUTS_REQUIRED_FILES)} required files"
    )
    checks += 1

    # A whole directory glob deleted. `bench/**` by name, because that is the
    # one this case was written after: `test_bench_smoke.py` imports
    # `bench.scenarios` and asserts on its matrix, and the group did not carry
    # a single file of it.
    bench = {m for m in members if m.startswith(ACCEPTANCE_PACKAGE + "bench/")}
    expect(len(bench) > 0, "the live group carries no bench/ file, so this case proves nothing")
    findings = _judge(path, _mutate(live, group, drop_inputs=bench))
    landed = _landed(path, group)
    expect(not (bench & landed.inputs), "the bench drop did not land")
    directory = _only(findings, "tag-suite-inputs-directory")
    expect("bench/" in directory.message, "the finding does not name the directory")
    print(f"S-012 mode 6 RED  : {directory.message}")
    checks += 1

    # A single named file dropped from `srcs`. `taskfile.yml` by name, for the
    # same reason: `test_doc_scripts_publish.py` reads it and it was not there.
    findings = _judge(path, _mutate(live, group, drop_inputs={"//test:taskfile.yml"}))
    landed = _landed(path, group)
    expect("//test:taskfile.yml" not in landed.inputs, "the taskfile.yml drop did not land")
    named = _only(findings, "tag-suite-inputs-file")
    expect("//test:taskfile.yml" in named.message, "the finding does not name the file")
    print(f"S-012 mode 6 RED  : {named.message}")
    checks += 1

    # A glob narrowed rather than deleted: every directory still contributes,
    # so only the count can see it. Enough `tests/**` fixtures go to cross the
    # floor, and one is kept so the directory clause stays silent and this case
    # is about the number alone.
    fixtures = sorted(m for m in members if m.startswith(ACCEPTANCE_PACKAGE + "tests/"))
    surplus = counted - SUITE_INPUTS_FLOOR
    drop = set(fixtures[: surplus + 1])
    expect(
        len(drop) < len(fixtures),
        f"dropping {len(drop)} of {len(fixtures)} tests/ labels would empty the directory, "
        "which is the other clause's case",
    )
    findings = _judge(path, _mutate(live, group, drop_inputs=drop))
    landed = _landed(path, group)
    expect(not (drop & landed.inputs), "the narrowing mutation did not land")
    floor = _only(findings, "tag-suite-inputs-floor")
    expect(
        "tag-suite-inputs-directory" not in codes(findings),
        "this case must be decided by the count alone",
    )
    print(f"S-012 mode 6 RED  : {floor.message}")
    checks += 1
    return checks


def prove_sweep(scratch: Path, live: list[dict]) -> int:
    """The modules that read their SIBLINGS' source, derived and then checked.

    Two halves, and they fail differently. The derivation is pure over source
    text and is shown here detecting a sweeper and sparing an arena walk; the
    graph clause is shown reddening both when a declared sweeper loses a sibling
    and when the derivation finds a sweeper the BUILD file does not know about —
    the case that would otherwise ship a target served a cached green over a
    tree it has never read.
    """
    checks = 0
    path = scratch / "mode7.json"

    # (a) the derivation, on text this function owns.
    synthetic = {
        "test_sweeper.py": "for p in TESTS_DIR.glob('test_*.py'): read(p)",
        "test_arena.py": "for p in tmp_path.rglob('*'): read(p)",
        "test_shellish.py": "for p in d.glob('test_shell*.py'): read(p)",
        "test_shell_one.py": "pass",
        "test_plain.py": "x = 1",
    }
    derived = sweep_requirements(synthetic)
    expect(
        derived["test_sweeper.py"] == frozenset(synthetic),
        f"the whole-suite pattern must match every module, got {sorted(derived['test_sweeper.py'])}",
    )
    expect(
        derived["test_shellish.py"] == frozenset({"test_shellish.py", "test_shell_one.py"}),
        f"got {sorted(derived['test_shellish.py'])}",
    )
    expect("test_arena.py" not in derived, "a bare `*` over an arena must not count as a sweep")
    expect("test_plain.py" not in derived, "a module with no glob must not count as a sweep")
    print(
        "S-012 mode 7 GREEN: the derivation reads a sweep off source — `test_*.py` matched all "
        f"{len(synthetic)}, `test_shell*.py` matched 2, and `tmp_path.rglob('*')` matched none"
    )
    checks += 1

    # (b) the live tree's own derivation, against the live graph.
    sources = read_acceptance_sources()
    expect(
        len(sources) == ACCEPTANCE_MODULE_TARGETS,
        f"read {len(sources)} acceptance modules, expected {ACCEPTANCE_MODULE_TARGETS} — a "
        "derivation over a short read requires nothing of anybody",
    )
    sweeps = sweep_requirements(sources)
    expect(len(sweeps) > 0, "the live tree derived no sweeping module at all")
    green = _judge(path, live)
    expect(
        "tag-acceptance-unswept" not in codes(green),
        f"the live graph must satisfy every derived sweep, got {codes(green)}",
    )
    print(
        "S-012 mode 7 GREEN: "
        + ", ".join(f"{name} reads {len(swept)}" for name, swept in sorted(sweeps.items()))
        + " — each set is in that target's declared inputs"
    )
    checks += 1

    # A declared sweeper loses one sibling.
    sweeper = max(sweeps, key=lambda name: len(sweeps[name]))
    label = ACCEPTANCE_PACKAGE + sweeper[: -len(MODULE_SHAPED_PATTERN_SUFFIX)]
    sibling = min(module_label(other) for other in sweeps[sweeper] if other != sweeper)
    findings = _judge(path, _mutate(live, label, drop_inputs={sibling}))
    landed = _landed(path, label)
    expect(sibling not in landed.inputs, "the sibling drop did not land")
    unswept = _only(findings, "tag-acceptance-unswept")
    expect(label in unswept.message and sibling in unswept.message, "the finding names neither")
    print(f"S-012 mode 7 RED  : {unswept.message}")
    checks += 1

    # A SIXTH sweeper appears — the case the BUILD file cannot know about. Fed
    # as a requirements map rather than by writing into `test/tests/`, so the
    # proof needs no file the suite would then collect.
    ordinary = min(name for name in sources if name not in sweeps and name != sweeper)
    invented = dict(sweeps)
    invented[ordinary] = frozenset(sources)
    findings = _judge(
        path,
        live,
        sweeps=invented,
        patterns={ordinary: ["test_*.py"]},
    )
    unswept = _only(findings, "tag-acceptance-unswept")
    expect(ordinary[: -len(MODULE_SHAPED_PATTERN_SUFFIX)] in unswept.message, "wrong target named")
    print(f"S-012 mode 7 RED  : {unswept.message}")
    checks += 1
    return checks


def prove_floor(scratch: Path, live: list[dict], bazel: str) -> int:
    """Mode 3 — the stage-scoped reader floor, and the undeclared stage it refuses."""
    checks = 0
    path = scratch / "mode3.json"
    floor = STAGE_FLOORS["stage-4"]
    rust = [r for r in live if r["rule"]["ruleClass"].startswith(RUST_RULE_PREFIX)]

    expect(
        len(live) >= floor.minimum,
        f"the live graph has {len(live)} rule targets, below the declared floor {floor.minimum}",
    )
    green = _judge(path, live)
    expect(green == [], f"the unmutated live capture must be silent, got {[f.message for f in green]}")
    print(
        f"S-012 mode 3 GREEN: {len(live)} rule targets read in {floor.universe} against a "
        f"declared floor of {floor.minimum} ({len(rust)} of them Rust)"
    )
    checks += 1

    # Drop enough to cross the floor, not a fixed one: the live graph sits at
    # or just above `floor.minimum`, so a one-target truncation reds only while
    # the two happen to be equal. Adding a target to the stage's universe made
    # this assertion green on a truncation the floor no longer refused.
    drop = len(live) - floor.minimum + 1
    short = live[:-drop]
    findings = _judge(path, short)
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    expect(len(reread) == len(live) - drop, f"the truncation did not land: {len(reread)} targets on disk")
    expect(codes(findings) == ["tag-reader-floor"], f"got {codes(findings)}")
    expect(floor.universe in findings[0].message, "the floor does not name the stage's universe")
    print(f"S-012 mode 3 RED  : {_only(findings, 'tag-reader-floor').message}")
    checks += 1

    # The stage-3-shaped reading, which is exactly the state where the narrowed
    # acceptance clause has no subject: every `//test:` target unread. It has to
    # be the loud one, because it is the failure mode of advancing the default
    # stage and leaving a caller's `--universe` behind.
    without_acceptance = [r for r in live if not r["rule"]["name"].startswith(ACCEPTANCE_PACKAGE)]
    findings = _judge(path, without_acceptance)
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    expect(
        len(reread) == len(without_acceptance)
        and not any(r.label.startswith(ACCEPTANCE_PACKAGE) for r in reread),
        "the acceptance truncation did not land",
    )
    expect(
        census_of(reread).acceptance == 0,
        "the truncated capture still carries acceptance targets, so this case is not the one "
        "it says it is",
    )
    expect(
        codes(findings) == ["tag-reader-floor"],
        f"a stage-4 run that read no //test: target must red, got {codes(findings)}",
    )
    print(f"S-012 mode 3 RED  : {_only(findings, 'tag-reader-floor').message}")
    checks += 1

    # And the same truncation one package further out: no `//test/doc_scripts`
    # either, which is what stage 1 read. Kept from the stage-3 wave because the
    # 40 build-action no-sandbox targets are still clause 1's other subject.
    crates_only = [r for r in live if not r["rule"]["name"].startswith("//test")]
    findings = _judge(path, crates_only)
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    expect(
        len(reread) == len(crates_only)
        and not any(r.label.startswith("//test") for r in reread),
        "the doc_scripts truncation did not land",
    )
    expect(
        codes(findings) == ["tag-reader-floor"],
        f"a stage-4 run that read no //test/... target must red, got {codes(findings)}",
    )
    print(f"S-012 mode 3 RED  : {_only(findings, 'tag-reader-floor').message}")
    checks += 1

    # The strongest red available: the real tool, a real narrower universe, no
    # byte mutation at all. A zero-match query exits 0, so the count is what
    # gates — and the override finding is what keeps a narrowed run from ever
    # being reported under the stage's name.
    narrow = "//crates/ocx_exit/..."
    findings, queried, census = run_check(stage="stage-4", bazel=bazel, universe=narrow)
    expect(queried == narrow, f"the live narrowed query ran over {queried}")
    expect(
        0 < census.rule_targets < floor.minimum,
        f"the narrowed universe read {census.rule_targets} rule targets",
    )
    expect(
        codes(findings) == ["tag-reader-floor", "tag-universe-override"],
        f"got {codes(findings)}",
    )
    print(
        f"S-012 mode 3 RED  : a live `bazel query` over {narrow} — {census.line()}; "
        f"{_only(findings, 'tag-reader-floor').message}"
    )
    print(f"S-012 mode 3 RED  : {_only(findings, 'tag-universe-override').message}")
    checks += 2

    # `stage-5` and not `stage-4`: stage 4 has a declared floor now (it is this
    # gate's default), and what this case is about is a stage that has none.
    findings, queried, census = run_check(stage="stage-5", bazel=bazel)
    expect(codes(findings) == ["tag-stage-unknown"], f"got {codes(findings)}")
    expect(
        queried is None and census.rule_targets == 0,
        "an undeclared stage must be refused before any query runs",
    )
    print(f"S-012 mode 3 RED  : {findings[0].message}")
    checks += 1

    restored = _judge(path, live)
    expect(restored == [], "restoring the live capture from our own bytes must green again")
    print("S-012 mode 3 GREEN: the capture restored from this file's own bytes is silent again")
    checks += 1
    return checks


def prove_counts() -> int:
    """The floors' provenance: sums imported from WP-13, never literals here."""
    stage1 = STAGE_FLOORS["stage-1"]
    stage3 = STAGE_FLOORS["stage-3"]
    expect(
        stage1.minimum == DRIFT_TARGET_FLOOR,
        f"stage-1's floor is {stage1.minimum}, not WP-13's DRIFT_TARGET_FLOOR ({DRIFT_TARGET_FLOOR})",
    )
    expect(
        stage3.minimum == DRIFT_TARGET_FLOOR + CAST_RULE_TARGETS + GIF_RULE_TARGETS,
        f"stage-3's floor is {stage3.minimum}, not {DRIFT_TARGET_FLOOR} + {CAST_RULE_TARGETS} "
        f"+ {GIF_RULE_TARGETS}",
    )
    expect(
        stage3.minimum > CRATES_RULE_TARGETS,
        "stage-3's floor must be a count of every rule kind, not of the Rust subset — the "
        "87 targets it was widened to reach are all non-Rust",
    )
    stage4 = STAGE_FLOORS["stage-4"]
    expect(
        stage4.minimum
        == DRIFT_TARGET_FLOOR
        + CAST_RULE_TARGETS
        + GIF_RULE_TARGETS
        + ACCEPTANCE_RULE_TARGETS
        + GRAPH_TAIL_TARGETS,
        f"stage-4's floor is {stage4.minimum}, not {DRIFT_TARGET_FLOOR} + {CAST_RULE_TARGETS} "
        f"+ {GIF_RULE_TARGETS} + {ACCEPTANCE_RULE_TARGETS} + {GRAPH_TAIL_TARGETS}",
    )
    expect(
        stage4.minimum > stage3.minimum + ACCEPTANCE_MODULE_TARGETS,
        "stage-4's floor must exceed stage-3's by more than the 181 modules, or a run that read "
        "the modules and skipped the groups they depend on would clear it",
    )
    expect(
        set(STAGE_FLOORS) == {"stage-1", "stage-3", "stage-4"},
        f"STAGE_FLOORS declares {sorted(STAGE_FLOORS)} — the adoption's three stages and no "
        "more; an undeclared stage is a hard red rather than a guessed number, which is what "
        "`stage-5` is used for in `prove_floor`",
    )
    print(
        f"S-012 counts: stage-1 floor {stage1.minimum} over {stage1.universe}, stage-3 floor "
        f"{stage3.minimum} over {stage3.universe}, stage-4 floor {stage4.minimum} over "
        f"{stage4.universe} ({ACCEPTANCE_RULE_TARGETS} of it the acceptance package), all "
        "imported from WP-13; an undeclared stage is refused"
    )
    return 1


def self_test(bazel: str) -> int:
    """All three modes, red and green, on the bytes the live graph produced."""
    scratch = REPO_ROOT / ".tmp"
    scratch.mkdir(exist_ok=True)
    checks = 0
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        work = Path(directory)
        _, live = capture_live(bazel, work)
        checks += prove_counts()
        checks += prove_no_sandbox(work, live)
        checks += prove_build_action(work, live)
        checks += prove_rust_binary(work, live)
        checks += prove_acceptance(work, live)
        checks += prove_suppressing_tags(work, live)
        checks += prove_suite_inputs(work, live)
        checks += prove_sweep(work, live)
        checks += prove_floor(work, live, bazel)
    print(
        f"bazel tag guard self-test: {checks} checks passed — C-011's clauses each shown "
        "red and green on the live graph's own query output: clause 1 in BOTH kinds (a test "
        "action without `external`, a build action without `no-remote-cache`, and each kind "
        "refusing the other's tag), the acceptance package's narrowed exemption (the "
        "declaration dropped at the target and at the shared group, `external` reddening the "
        "acceptance package rather than excusing it, and an impostor outside //test: refused), "
        "the three cache-suppressing tags each reddening an acceptance target, the shared "
        "group's own floor (a directory glob deleted, a named file dropped, a glob narrowed "
        "past the count), the derived sibling sweep (read off source, red at a lost sibling and "
        "red at a sweeper the BUILD file does not declare), clause 2 (a rust_binary with "
        "neither per-target escape), and the stage-4 reader floor"
    )
    print(
        "  wired into taskfiles/scripts.taskfile.yml `self-test:` (WP-17): "
        "`- python3 scripts/bazel_tag_guard.py --self-test`"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--self-test", action="store_true", help="prove all three clauses red and green")
    parser.add_argument(
        "--stage",
        default="stage-4",
        help="which reader floor applies (STAGE_FLOORS in scripts/bazel_gate_proofs.py). "
        "An undeclared stage is refused, never run floorless. The default is the single "
        "place the adoption's stage advances; `bazel:tag:guard` passes no --stage on purpose",
    )
    parser.add_argument(
        "--bazel",
        default="bazel",
        help="the bazel binary to query with (default: resolved from PATH, as the per-prompt "
        "hook and CI's setup-ocx action both leave it)",
    )
    parser.add_argument(
        "--universe",
        default=None,
        help="query a universe other than the stage's own. Can only narrow the verdict: the "
        "reader floor reds on a smaller one, and any override is its own finding, so no "
        "value here reaches a green that the stage's universe would not",
    )
    parser.add_argument(
        "--query-json",
        type=Path,
        default=None,
        help="judge a captured `bazel query --output=streamed_jsonproto` instead of running one",
    )
    args = parser.parse_args()

    if args.self_test:
        return self_test(args.bazel)

    findings, queried, census = run_check(
        stage=args.stage, bazel=args.bazel, universe=args.universe, query_json=args.query_json
    )
    if queried is not None:
        # Said out loud on every run: a green is only as wide as what ran, and an
        # unstated scope is read as the name's. The two clause subject counts are
        # on the line too, because those are the numbers that can be zero while
        # the reader count looks healthy.
        print(f"bazel tag guard: stage {args.stage} over {queried} — {census.line()}")
    return report(findings)


if __name__ == "__main__":
    raise SystemExit(main())
