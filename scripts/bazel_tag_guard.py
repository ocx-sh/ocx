#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`bazel:tag:guard` — C-011, the compensating control for the plan's one BZL-CORE-01 deviation.

    scripts/bazel_tag_guard.py                       # the gate: stage-4, //...
    scripts/bazel_tag_guard.py --stage stage-1 [--bazel bazel] [--universe //crates/...]
    scripts/bazel_tag_guard.py --query-json FILE     # judge a captured query instead of running one

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
untested is a clause that will be vacuous forever, so the tests reds every
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
both ways in the tests. Anything outside them — including a tag nobody has
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
twice, and an unsound cache key is not a stamping question. the tests proves
that exact pair rather than describing it.

**Clause 2 narrowed with the kind split, and deliberately.** `external` used to
excuse an unstamped `rust_binary` through `cache_excluded`; it no longer does,
because a `rust_binary` is a build action and `external` is measured inert
there. The escapes are `stamp = 0` and `no-remote-cache`, which is exactly what
C-011 wrote.

**Clause 1 has one narrowed subject, and narrowing is not holing.** The
acceptance package (`//test:`, one `sh_test` target per acceptance module) runs with its results
**cached** — the ADR's stage-4 ruling as amended — so `external`, the tag the
blanket rule demands of a `no-sandbox` test action, is precisely the tag it must
not carry. A blanket rule would red every one, and the two ways out of that are a
hole (skip the package) and a narrowing (demand something else of it). This file
narrows: an acceptance target is credited without `external` **only while its
input closure declares `//test:docker-compose.yml`, `//test:suite_anchor` and
`//crates/ocx_cli:ocx`** — the compose definition, which pins every service
image and port, the group carrying the runner's anchor, and the binary under
test. The compose definition and the binary are the inputs whose change a
cached green would otherwise hide, which is what makes the declaration a
compensating control rather than a formality. Lose either and `tag-acceptance-undeclared`
fires beside the blanket finding; carry `external` after all and the blanket
escape still works. the tests' mode 4 shows both, plus the case a one-hop
reader would miss (a source leaving the shared group, which reds every target at
once) and an impostor outside `//test:` declaring the same two labels and still
being refused.

**The closure is one query, not two.** `ruleInput` is one hop — a target whose
`data` names `:suite_inputs` lists the *group's* label — and every filegroup in
the universe is itself a `RULE` record with its own `ruleInput`, so
`input_closure` expands them breadth-first out of the same stream. A `deps(...)`
query would be a second reading of a second universe, and two readings of one
fact are how they stop agreeing.

**The floor is stage-scoped and advances — and this is the default stage.**
`STAGE_FLOORS` (WP-13) declares stage 1 (`//crates/...` >= 62), stage 3
(`//crates/... + //test/doc_scripts/...` >= 149, WP-33's 45 and WP-33b's 42
added to `//crates/...`'s 62 — the 42 GIF renders took that package from 45 targets to
87, and until the floor moved with them it cleared by 42 and discriminated
nothing) and now stage 4 (`//...` >= 337: 149 + the acceptance package's 181 +
the 7 root and website targets no earlier universe names). `--stage` defaults to
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

**C-019 (plan_test_speed_tiers.md), the uncached list.** An acceptance
module whose source — or an imported helper, or a `conftest.py` above it —
names a path its target's input closure does not cover can be served a stale
cached green. `named_repo_paths` finds those paths by AST, `declared` judges
them against the closure, and `uncached_findings` requires the set of such
modules to equal `test/bazel.bzl` `UNCACHED_MODULES` exactly, each carrying
`external`; `external` anywhere else, and `no-cache`/`local` anywhere, stay
red. Named residuals of the predicate: a path built from a value it cannot
trace (`os.environ`, a function argument), a subprocess that inherits the
runner's `test/` cwd and reads the checkout implicitly (`git`), a helper
starting a subprocess at the bare root, and a walk *of* `test/` itself.

Its proofs (the `prove_*` functions) run as pytest, from
`scripts/tests/test_bazel_tag_guard.py`. Every mutation in them is proven to
have landed before its result is trusted; until then a surviving green is
*unexplained*, not excused. Fixtures are the live query's own bytes, mutated
under the test's own `tmp_path`, and nothing is restored with `git checkout --`,
which restores from the index and would make the run vacuous.
"""

from __future__ import annotations

import argparse
import ast
import collections
import dataclasses
import fnmatch
import functools
import json
import posixpath
import subprocess
from pathlib import Path

from bazel_accept_proofs import (
    CACHE_DEFEATING_TAG,
    INSUFFICIENT_TAGS,
    SERIALISING_TAG,
    rc_serialisation_findings,
    read_uncached_modules,
    runner_lock_findings,
)
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
        "//crates/ocx_cli:ocx",
        "//test:docker-compose.yml",
        "//test:suite_anchor",
    }
)
"""What an acceptance target must declare to be credited without `external`.

`docker-compose.yml` carries every service image reference and every host
port, so a bumped registry or a moved port re-keys the suite.
`//crates/ocx_cli:ocx` is the binary under test — A4 red half 3's whole subject
and the input whose staleness a cached green would otherwise hide. It is a
Bazel output, so declaring it keys the target on the binary Bazel built, and
the runner executes that runfile rather than a copy. `:suite_anchor` carries
`conftest.py`, the runner's anchor into the source tree."""

ACCEPTANCE_SUPPRESSING_TAGS = INSUFFICIENT_TAGS | {CACHE_DEFEATING_TAG}
"""Tags an acceptance target must NOT carry, now that its results are cached.

The inverse of `CACHE_EXCLUDING_TAGS`, and the clause that makes the amended
stage-4 ruling enforceable from the graph: `external` stops the result being
reused at all, and `local`/`no-remote` suppress the `--disk_cache` tier, which
is the only tier a fresh CI server has. Re-adding any of them would silently
recompute every acceptance target on every CI run while a laptop still reported
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
        "//test:conftest.py",
        "//test:docker-compose.yml",
        "//test:ocx.lock",
        "//test:ocx.toml",
        "//test:pyproject.toml",
        "//test:uv.lock",
        "//test:zot-config.json",
    }
)
"""Named files the group must carry — each one read by (nearly) every module,
none of them matched by a directory glob, so a dropped `srcs` line takes one
out without moving any count far enough to notice. A file only a few modules
read is not here: it is declared on those modules' targets (`test/BUILD.bazel`
`module_data`), and `uncached_findings` reds a reader that loses it."""

SUITE_INPUTS_REQUIRED_DIRS = frozenset(
    {
        "sigstore/",
        "src/",
        "tests/",
    }
)
"""Every directory `test/BUILD.bazel`'s glob names, as a `//test:` path prefix.
A glob deleted outright takes its whole directory out of the closure, which a
count floor alone would only catch for the big ones."""

SUITE_INPUTS_FLOOR = 100
"""How many labels the group's expanded closure must carry.

Measured on this tree (bazel 9.2.0): 100 labels once the per-module files
(`bench/`, `recordings/`, `scenarios/`, `scripts/`, `specs/`, `taskfile.yml`)
left for `module_data` and the unread ones (`docker/`, the floor and ceiling
files) left altogether — it was 156 before. No binary is in the group (the
acceptance targets declare `//crates/ocx_cli:ocx` themselves), so the number
is the same on a clean checkout as on a built one. A floor, so it
only rises except when a file moves to its readers' targets, which the
per-module derivation (`uncached_findings`) then guards: a narrowed glob that
still covers every directory above is what this number is for."""

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
output tree they just wrote, and crediting those would hand the whole module
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
    "`:suite_inputs` and `_OCX` back to the target's `data` in test/bazel.bzl, or re-add the "
    "missing source to //test:suite_inputs / //test:suite_anchor"
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
TAG_SWEEPER_MSG = (
    "bazel tag guard: {label} reads its SIBLING modules' source ({patterns}, {count} modules). "
    "An acceptance target is keyed on its own module plus the shared group, so a cached verdict "
    "over every sibling is stale the moment any one of them changes — and there is no per-target "
    "declaration that fixes that without re-running it on every module's edit. A sweep is a lint "
    "test: move it to test/lint/, where `task test:lint:structure` runs it uncached "
    "(plan_test_speed_tiers.md C-LINT)"
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
TAG_UNCACHED_UNLISTED_MSG = (
    "bazel tag guard: {label} reads {reads}, which its input closure does not cover, and it is "
    "not in test/bazel.bzl `UNCACHED_MODULES`. Its result is CACHED, so a change to any of those "
    "paths leaves a stale green (plan C-019). Declare the input, or list the module so its target "
    "carries `external`"
)
TAG_UNCACHED_ROT_MSG = (
    "bazel tag guard: {label} is listed in test/bazel.bzl `UNCACHED_MODULES` but reads no "
    "undeclared path any more — delete the entry, or the list rots into a permanent cache opt-out"
)
TAG_UNCACHED_UNTAGGED_MSG = (
    "bazel tag guard: {label} is listed in test/bazel.bzl `UNCACHED_MODULES` but its target does "
    "not carry `{tag}`, the one tag measured to stop a test result being reused — `no-cache` and "
    "`local` are not substitutes"
)
TAG_UNCACHED_UNKNOWN_MSG = (
    "bazel tag guard: test/bazel.bzl `UNCACHED_MODULES` lists {name}, which is no acceptance "
    "target in this universe"
)
TAG_UNCACHED_UNREAD_MSG = (
    "bazel tag guard: {label}'s module source was never read, so whether it reads an undeclared "
    "path is unknown — a module the reader skipped is not a clean one"
)
TAG_CARRIES_SOME = "carries {present}, none of which excludes it from a cache"
TAG_CARRIES_NONE = "carries no cache-excluding tag at all"
TAG_SLOT_UNDECLARED_MSG = (
    "bazel tag guard: {label} names the xdist group(s) {groups}, and its target's "
    "`OCX_ACCEPTANCE_SLOTS` is {declared!r}. Every acceptance target is its own pytest process, "
    "and runs from sibling checkouts on one stack overlap, so a group — shared with another "
    "module or not — is serialised only by the runner's per-group host lock; xdist would have "
    "put it on one worker. Set test/BUILD.bazel `module_slots` for this module to {groups}"
)
TAG_SLOT_UNRESOLVED_MSG = (
    "bazel tag guard: {label} spells an xdist group this derivation cannot resolve to a string "
    "literal ({spelling}). Whether another module shares it — and so whether its target needs a "
    "slot lock — is then unknowable. Use a literal, or a subscript of a module-level dict of "
    "literals (test_doc_scripts.py's SHARED_SLOT_GROUPS)"
)
TAG_SLOT_FOREIGN_MSG = (
    "bazel tag guard: {path} spells `xdist_group` ({spelling}) outside a `tests/test_*.py` "
    "module. The slot derivation reads each module's own source only, so a group applied from a "
    "helper, a conftest or an `add_marker` hook reaches no target's `OCX_ACCEPTANCE_SLOTS` and "
    "its members run unserialised. Move the mark into the modules it applies to"
)
TAG_HAND_READ_MSG = (
    "bazel tag guard: {label} does not declare {missing}, which it reads where no AST can see "
    "(`HAND_DECLARED_READS` / `IMPLIED_READS` say how). Its result is CACHED, so a change to "
    "that input would leave a stale green. Restore it in test/BUILD.bazel `module_data`"
)
TAG_SERIALISED_MSG = (
    "bazel tag guard: {label} is an acceptance target and carries `{tag}`, which runs it with "
    "nothing else. The acceptance targets run concurrently behind the runner's host locks "
    "(test/bazel.bzl § Concurrency, ADR AM-9), so one `{tag}` target puts the suite back on a "
    "single lane. A module that must not overlap another names an xdist group instead "
    "(`module_slots` in test/BUILD.bazel)"
)
TAG_HAND_READ_UNREVIEWED_MSG = (
    "bazel tag guard: {label} (or a helper it imports) {hints} — a read the AST derivation "
    "cannot follow, into a `test/` directory `:suite_inputs` no longer carries or through a "
    "go-task recipe. Its result is CACHED, so give it a `HAND_DECLARED_READS` entry naming what "
    "it reads (an empty set records that it reads nothing), and declare those in `module_data`"
)
TAG_HAND_READ_UNKNOWN_MSG = (
    "bazel tag guard: `HAND_DECLARED_READS` names {name}, which is no acceptance target in this "
    "universe — delete the entry, or it rots into a requirement nothing is held to"
)
TAG_SLOT_READER_MSG = (
    "bazel tag guard: the xdist-group derivation found {found} as group(s), and {known!r} is "
    "not among them — the reader stopped matching, and an empty requirement is satisfied by "
    "every target"
)
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

    env: tuple[tuple[str, str], ...] = ()
    """The rule's `env` attribute, sorted `(name, value)` pairs — where an
    acceptance target carries `OCX_ACCEPTANCE_SLOTS` (`slot_findings`)."""


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
        env: dict[str, str] = {}
        for attribute in rule.get("attribute", []):
            if not isinstance(attribute, dict):
                continue
            if attribute.get("name") == "tags":
                tags = {str(tag) for tag in attribute.get("stringListValue", [])}
            elif attribute.get("name") == "env":
                env = {
                    str(entry.get("key")): str(entry.get("value", ""))
                    for entry in attribute.get("stringDictValue", [])
                    if isinstance(entry, dict)
                }
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
                env=tuple(sorted(env.items())),
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
    visiting: set[str] = set()

    def reach(label: str) -> frozenset[str]:
        # A rule's closure is its inputs plus each input rule's closure, so a
        # shared group (`:suite_inputs`, 400-odd files) is expanded once, not
        # once per target that names it.
        if label in closure:
            return closure[label]
        if label in visiting:
            raise _Cycle
        visiting.add(label)
        seen = set(by_label[label].inputs)
        for dep in by_label[label].inputs:
            if dep in by_label:
                seen |= reach(dep)
        visiting.discard(label)
        closure[label] = frozenset(seen)
        return closure[label]

    try:
        for record in records:
            reach(record.label)
    except _Cycle:
        # Bazel refuses a cycle, so this is a hand-built capture. Fall back to
        # one breadth-first walk per record, which a cycle cannot hang.
        closure = {}
        for record in records:
            seen_bfs: set[str] = set()
            queue = list(record.inputs)
            while queue:
                label = queue.pop()
                if label in seen_bfs:
                    continue
                seen_bfs.add(label)
                nested = by_label.get(label)
                if nested is not None:
                    queue.extend(nested.inputs)
            closure[record.label] = frozenset(seen_bfs)
    return closure


class _Cycle(Exception):
    """`input_closure` met a cycle and must take the breadth-first path."""


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
    return list(_sweeping_globs(source))


@functools.cache
def _sweeping_globs(source: str) -> tuple[str, ...]:
    """Every module-shaped `glob`/`rglob` pattern literal in one module's source.

    `ast`, not a regex: the subject is a call with a string literal argument,
    and a regex over the text `.glob(` would also match it inside a docstring or
    a comment — a module that merely *documents* the sweep would then be handed
    every sibling module as inputs it never reads.
    """
    patterns: list[str] = []
    for node in ast.walk(_parse(source)):
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
    return tuple(sorted(set(patterns)))


def sweep_requirements(sources: dict[str, str]) -> dict[str, frozenset[str]]:
    """`{module basename: the sibling module basenames its own source reads}`.

    Pure over already-read text, so the self-test can plant a sweeper and watch
    the clause fire without writing a file into `test/tests/`.
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
    sweeps: dict[str, frozenset[str]],
    patterns: dict[str, list[str]],
) -> list[Finding]:
    """No acceptance module may read its siblings' source — declared or not."""
    findings: list[Finding] = []
    for basename in sorted(sweeps):
        label = ACCEPTANCE_PACKAGE + basename[: -len(MODULE_SHAPED_PATTERN_SUFFIX)]
        if label not in acceptance:
            continue
        findings.append(
            Finding(
                "tag-acceptance-sweeper",
                TAG_SWEEPER_MSG.format(
                    label=label, patterns=patterns.get(basename, []), count=len(sweeps[basename])
                ),
            )
        )
    return findings


_PATH_CONSTRUCTORS = frozenset({"Path", "PurePath", "PosixPath", "PurePosixPath", "str", "fspath"})
"""Calls that pass a path through unchanged: `Path(x)`, `str(x)`, `os.fspath(x)`."""

_PATH_IDENTITIES = frozenset({"resolve", "absolute", "expanduser", "abspath", "realpath"})
"""Methods and `os.path` functions that keep the path they are handed."""

_ROOT_NAME = "PROJECT_ROOT"
"""`test/src/helpers.py`'s repository root, the one root the suite exports."""

AMBIENT_DIR = "test"
"""Where every acceptance module already runs: the runner `cd`s here and
`test/pyproject.toml` puts it on `pythonpath`. A module naming it (a `cwd=`,
a `sys.path` entry) re-states its own working directory, so it is not counted —
which leaves one named residual: a walk *of* `test/` (`rglob` over it) reaches
`test/doc_scripts/` and `test/lint/` unseen."""

HELPER_EXCLUDED_PARTS = frozenset({".venv", "__pycache__", "lint", "results"})
"""Directories under `test/` whose Python no acceptance module imports: the
virtualenv, bytecode, the lint tier (run uncached by its own task) and bench
output."""


@functools.cache
def _parse(source: str) -> ast.Module:
    """One parse per distinct source text for the whole run. Every per-file
    analysis below reads the same tree, and none mutates it. A planted source is
    a different text, so it is parsed afresh; nothing outlives the process."""
    return ast.parse(source)


def _normal(path: str) -> str:
    """Repo-relative POSIX form; the root is `""`, above it starts with `..`."""
    if not path:
        return ""
    normal = posixpath.normpath(path)
    return "" if normal == "." else normal


def _join(base: str, segment: str) -> str:
    return _normal(posixpath.join(base, segment) if base else segment)


def _parent(path: str) -> str:
    return _normal(path + "/..") if path else ".."


def _call_name(func: ast.expr) -> str | None:
    if isinstance(func, ast.Attribute):
        return func.attr
    if isinstance(func, ast.Name):
        return func.id
    return None


class _PathEvaluator:
    """Evaluates one file's path expressions to repo-relative strings.

    A base is the file itself (`__file__`), `PROJECT_ROOT` (imported or reached
    as an attribute), a relative literal handed to `Path`/`open` or `Path.cwd()`
    (both relative to the runner's working directory, `test/`), or a name or function bound to an
    expression that evaluates. Anything else evaluates to `None` and names
    nothing: `tmp_path / "website"` has no base, and a literal that is never
    joined onto one is only a string.
    """

    def __init__(self, tree: ast.Module, path: str) -> None:
        self.file = _normal(path)
        # Names are bound per scope: a function (or lambda) by its node id, the
        # module as 0. A name resolves innermost-out, so `candidate` bound onto
        # the root in one function is not the `candidate` another binds onto
        # `tmp_path`. Comprehensions share their function's scope — a
        # conservative merge, never a leak across functions.
        self.names: dict[tuple[int, str], str] = {(0, _ROOT_NAME): ""}
        self.functions: dict[str, str] = {}
        # One pass over the tree yields everything the fixpoint below and
        # `named_repo_paths` need: each node's parent, each node's enclosing
        # scope chain (innermost first, the module last), and the nodes that
        # can bind. Walking the parent chain per lookup and re-walking the
        # whole tree per fixpoint round made this quadratic in file size.
        self.parent_of: dict[int, ast.AST] = {}
        self.scope_of: dict[int, tuple[int, ...]] = {}
        self.binders: list[ast.AST] = []
        # BFS, so `binders` keeps `ast.walk`'s order: the first binding of a
        # name wins, and which one is first must not change.
        queue: collections.deque[tuple[ast.AST, tuple[int, ...]]] = collections.deque([(tree, (0,))])
        while queue:
            node, chain = queue.popleft()
            self.scope_of[id(node)] = chain
            if isinstance(node, (ast.Assign, ast.AnnAssign, ast.For, ast.comprehension, ast.FunctionDef, ast.AsyncFunctionDef)):
                self.binders.append(node)
            elif isinstance(node, ast.ImportFrom):
                for alias in node.names:
                    if alias.name == _ROOT_NAME:
                        self.names[(chain[0], alias.asname or alias.name)] = ""
            inner = (id(node), *chain) if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)) else chain
            for child in ast.iter_child_nodes(node):
                self.parent_of[id(child)] = node
                queue.append((child, inner))
        self._memo: dict[int, str | None] | None = None
        # Bindings can chain (`R = parents[2]`, then `D = R / "test"`), so the
        # scan repeats until nothing new binds. Bounded by the binding count.
        for _ in range(64):
            if not self._bind():
                break
        # Bindings are final from here on, so an expression's value is too.
        self._memo = {}

    def _scopes(self, node: ast.AST) -> tuple[int, ...]:
        """The enclosing scopes of `node`, innermost first, the module last."""
        return self.scope_of.get(id(node), (0,))

    def _bind(self) -> bool:
        grew = False
        for node in self.binders:
            targets: list[ast.expr] = []
            value: str | None = None
            if isinstance(node, ast.Assign):
                targets, value = node.targets, self.eval(node.value)
            elif isinstance(node, ast.AnnAssign) and node.value is not None:
                targets, value = [node.target], self.eval(node.value)
            elif isinstance(node, (ast.For, ast.comprehension)):
                # `for ancestor in <path>.parents:` binds every ancestor, the
                # repository root among them.
                it = node.iter
                if isinstance(it, ast.Attribute) and it.attr == "parents" and self.eval(it.value) is not None:
                    targets, value = [node.target], ""
            elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name not in self.functions:
                for inner in ast.walk(node):
                    if isinstance(inner, ast.Return) and inner.value is not None:
                        returned = self.eval(inner.value)
                        if returned is not None:
                            self.functions[node.name] = returned
                            grew = True
                            break
            if value is None:
                continue
            for target in targets:
                if isinstance(target, ast.Name):
                    key = (self._scopes(target)[0], target.id)
                    if key not in self.names:
                        self.names[key] = value
                        grew = True
        return grew

    def eval(self, node: ast.expr) -> str | None:
        if self._memo is None:
            return self._eval(node)
        key = id(node)
        if key not in self._memo:
            self._memo[key] = self._eval(node)
        return self._memo[key]

    def _eval(self, node: ast.expr) -> str | None:
        match node:
            case ast.Name(id="__file__"):
                return self.file
            case ast.Name(id=name):
                for scope in self._scopes(node):
                    if (scope, name) in self.names:
                        return self.names[(scope, name)]
                return None
            case ast.Attribute(attr="parent", value=inner):
                base = self.eval(inner)
                return None if base is None else _parent(base)
            case ast.Attribute(attr=attr) if attr == _ROOT_NAME:
                return ""
            case ast.Subscript(value=ast.Attribute(attr="parents", value=inner), slice=ast.Constant(value=int(n))):
                base = self.eval(inner)
                if base is None:
                    return None
                for _ in range(n + 1):
                    base = _parent(base)
                return base
            case ast.BinOp(left=left, op=ast.Div(), right=ast.Constant(value=str(segment))):
                base = self.eval(left)
                return None if base is None else _join(base, segment)
            case ast.BinOp(left=left, op=ast.Add(), right=ast.Constant(value=str(tail))) if tail.startswith("/"):
                # `str(PROJECT_ROOT) + "/website"`
                base = self.eval(left)
                return None if base is None else _join(base, tail[1:])
            case ast.Call(func=func, args=args):
                return self._call(func, args)
        return None

    def _call(self, func: ast.expr, args: list[ast.expr]) -> str | None:
        name = _call_name(func)
        if isinstance(func, ast.Name) and name in self.functions:
            return self.functions[name]
        if name in ("cwd", "getcwd") and not args:
            # `Path.cwd()` / `os.getcwd()`: the runner's working directory.
            return AMBIENT_DIR
        if name in (*_PATH_CONSTRUCTORS, "open") and len(args) >= 1:
            first = args[0]
            if isinstance(first, ast.Constant) and isinstance(first.value, str):
                # Relative to the runner's working directory, `test/`: since
                # `scenarios/`, `specs/` and `taskfile.yml` left
                # `:suite_inputs`, `Path("scenarios/x")` is as much a read as
                # an escape above it. An absolute literal names no repo path.
                return None if first.value.startswith("/") else _join(AMBIENT_DIR, first.value)
            return self.eval(first) if name != "open" else None
        if name == "dirname" and len(args) == 1:
            base = self.eval(args[0])
            return None if base is None else _parent(base)
        if name in _PATH_IDENTITIES:
            # `p.resolve()` or `os.path.realpath(p)`.
            subject = args[0] if args else getattr(func, "value", None)
            return None if subject is None else self.eval(subject)
        if name in ("joinpath", "join") and (args or name == "joinpath"):
            is_method = name == "joinpath"
            if is_method:
                base = self.eval(func.value)  # type: ignore[attr-defined]
                segments = args
            else:
                base, segments = self.eval(args[0]), args[1:]
            if base is None or not all(
                isinstance(s, ast.Constant) and isinstance(s.value, str) for s in segments
            ):
                return None
            for segment in segments:
                base = _join(base, segment.value)  # type: ignore[attr-defined]
            return base
        return None


def named_repo_paths(source: str, path: str) -> list[str]:
    return list(_named_repo_paths(source, path))


@functools.cache
def _named_repo_paths(source: str, path: str) -> tuple[str, ...]:
    """Every repo-relative path one file names through a traceable base (plan
    C-019's predicate, by AST).

    A path is named where it is USED: every expression that evaluates (see
    `_PathEvaluator`) and is not itself part of a larger evaluable one — the
    outermost join of a chain (`/`, `joinpath`, `os.path.join`, `+ "/x"`),
    multi-segment joins and `"a/b"` literals alike, or a bare base handed to a
    call as a `cwd=`, an argument or a receiver (`PROJECT_ROOT.rglob(...)`,
    `PROJECT_ROOT / name`). A binding of the bare root is skipped —
    `R = parents[2]` names nothing until `R` is used. Not named: a literal joined onto `tmp_path` or
    any base it cannot trace, a literal that only appears in a string, and the
    ambient `test/` directory (`AMBIENT_DIR`). `path` is what `__file__` is.

    Whether a named path is *undeclared* is the graph's question, not this
    function's: `uncached_findings` asks the target's input closure.
    """
    tree = _parse(source)
    paths = _PathEvaluator(tree, path)
    parent_of = paths.parent_of
    named: set[str] = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.expr) or getattr(node, "ctx", ast.Load()).__class__ is not ast.Load:
            continue
        value = paths.eval(node)
        if value is None or value == AMBIENT_DIR:
            continue
        parent = parent_of.get(id(node))
        if isinstance(parent, ast.expr) and paths.eval(parent) is not None:
            continue
        if isinstance(parent, ast.Attribute):
            # The receiver of a method whose call evaluates (`p.resolve()`,
            # `p.joinpath("x")`) is part of that call's value, not a use.
            grand = parent_of.get(id(parent))
            if isinstance(grand, ast.Call) and grand.func is parent and paths.eval(grand) is not None:
                continue
        if isinstance(parent, ast.Attribute) and parent.attr == "parents":
            # `x.parents[n]` is judged at the subscript, `for a in x.parents`
            # at the loop variable's uses; `x` alone is read by neither.
            continue
        # A root alias (`ROOT = parents[2]`, `return PROJECT_ROOT`) names
        # nothing until it is used — its uses are where the path is. Any other
        # binding is named where it is bound, so a constant exported to and
        # used by another file (where it cannot be traced) still counts.
        if value == "" and (
            isinstance(parent, ast.Return)
            or (isinstance(parent, (ast.Assign, ast.AnnAssign)) and parent.value is node)
        ):
            continue
        named.add(value)
    return tuple(sorted(named))


def read_helper_sources() -> dict[str, str]:
    return dict(_read_helper_sources())


@functools.cache
def _read_helper_sources() -> tuple[tuple[str, str], ...]:
    """Every non-module Python file under `test/` an acceptance module can
    import, as `{repo-relative path: text}` — `src/**`, `tests/fixtures/**`,
    `bench/**`, the `conftest.py` files. Read so a helper's reads are charged to
    the modules importing it (plan C-019, security review item 1: a cosign
    fixture read the root `ocx.toml` from five unlisted modules)."""
    helpers: dict[str, str] = {}
    root = REPO_ROOT / "test"
    for file in sorted(root.rglob("*.py")):
        relative = file.relative_to(root)
        if HELPER_EXCLUDED_PARTS & set(relative.parts[:-1]):
            continue
        if relative.parts[0] == "tests" and len(relative.parts) == 2 and file.name.startswith("test_"):
            continue
        helpers[f"test/{relative.as_posix()}"] = file.read_text(encoding="utf-8")
    return tuple(helpers.items())


def _dotted_names(path: str) -> set[str]:
    """How `import` can spell one file under `test/`: from `test/` itself and,
    for `src/**`, from `test/src` — `test/pyproject.toml`'s `pythonpath`."""
    parts = path.removeprefix("test/").removesuffix(".py").split("/")
    if parts[-1] == "__init__":
        parts = parts[:-1]
    names = {".".join(parts)} if parts else set()
    if len(parts) > 1 and parts[0] == "src":
        names.add(".".join(parts[1:]))
    if len(parts) == 2 and parts[0] == "tests":
        # pytest's default `prepend` import mode puts a module's own directory
        # on `sys.path`, so `from git_shim import ...` resolves too.
        names.add(parts[1])
    return names


def _imports(tree: ast.Module, path: str) -> set[str]:
    """Every dotted name one file imports, packages on the way included."""
    package = path.removeprefix("test/").removesuffix(".py").split("/")[:-1]
    names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            names.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            base = package[: len(package) - node.level + 1] if node.level else []
            module = ".".join([*base, *(node.module.split(".") if node.module else [])])
            names.add(module)
            names.update(f"{module}.{alias.name}" if module else alias.name for alias in node.names)
    prefixes = {".".join(name.split(".")[:i]) for name in names for i in range(1, name.count(".") + 1)}
    return names | prefixes


@functools.cache
def _imports_of(text: str, path: str) -> frozenset[str]:
    return frozenset(_imports(_parse(text), path))


def named_paths_by_module(
    sources: dict[str, str], helpers: dict[str, str] | None = None
) -> dict[str, list[str]]:
    """`{module basename: every path it names}` for EVERY module read — its own,
    plus those of every helper it reaches by import and of the `conftest.py`
    files pytest loads for it. An empty list is a module that was read and names
    nothing, which is not the same state as a module the reader never saw
    (`uncached_findings` floors on it)."""
    if helpers is None:
        helpers = read_helper_sources()
    # A sibling acceptance module is importable too (`from tests.test_attest
    # import attest`), and it is in neither `//test:suite_inputs` (which
    # excludes `tests/test_*.py`) nor the helper set above — so it joins the
    # graph as one more node, and reaching it charges the importer with the
    # sibling's FILE as well as everything the sibling reads. An importer whose
    # target does not declare that file serves a verdict an edit to the sibling
    # cannot move (cross-model adversary, finding 4).
    siblings = {f"test/tests/{name}": text for name, text in sources.items()}
    graph = {**helpers, **siblings}
    by_name = {name: path for path in graph for name in _dotted_names(path)}
    # The bare root is dropped for helpers, and only for helpers: the live one
    # (`tests/git_shim.py`'s `DENIED_SHIM_ROOTS`) is a containment check, and
    # a helper cannot know which of its importers ever calls the code that
    # uses it. Named residual: a helper starting a subprocess at the root.
    # An imported sibling is a helper to its importer, so the same holds.
    helper_named = {
        path: [read for read in named_repo_paths(text, path) if read]
        for path, text in graph.items()
    }
    helper_edges = {
        path: {by_name[name] for name in _imports_of(graph[path], path) if name in by_name} - {path}
        for path in graph
    }
    conftests = {path for path in helpers if path.endswith("conftest.py")}
    result: dict[str, list[str]] = {}
    for name, text in sorted(sources.items()):
        path = f"test/tests/{name}"
        seen: set[str] = {path}
        queue = [by_name[i] for i in _imports_of(text, path) if i in by_name] + sorted(
            c for c in conftests if path.startswith(posixpath.dirname(c) + "/")
        )
        while queue:
            helper = queue.pop()
            if helper not in seen:
                seen.add(helper)
                queue.extend(helper_edges[helper])
        # Importing a helper reads its source, so the helper file is itself a
        # read: a module importing `bench.scenarios` is stale the moment that
        # file changes, whether or not the helper names a path of its own. An
        # imported sibling module is such a helper, so its FILE is charged too.
        imported = seen - {path}
        reads = set(named_repo_paths(text, path)) | imported
        for helper in imported:
            reads.update(helper_named[helper])
        result[name] = sorted(reads)
    return result


@functools.cache
def _named_paths_of(sources: tuple[tuple[str, str], ...]) -> dict[str, list[str]]:
    """`named_paths_by_module` over the live helpers, memoised on the module
    texts: the self-test judges the same sources dozens of times. A planted
    source is a different key, so it is derived afresh — and then only the
    planted file costs anything, since every per-file analysis is memoised."""
    return named_paths_by_module(dict(sources))


@functools.cache
def tracked_files() -> frozenset[str]:
    """`git ls-files` at the repository root — what a directory named by a
    module actually holds, so a walked directory is judged file by file."""
    listed = subprocess.run(
        ["git", "ls-files", "-z"], cwd=REPO_ROOT, capture_output=True, encoding="utf-8", check=True
    )
    return frozenset(name for name in listed.stdout.split("\0") if name)


def label_path(label: str) -> str | None:
    """`//website:src/a.md` -> `website/src/a.md`; `None` for an external repo."""
    if not label.startswith("//"):
        return None
    package, _, name = label[2:].partition(":")
    return f"{package}/{name}" if package else name


@functools.cache
def _tracked_under(path: str, tracked: frozenset[str]) -> tuple[str, ...]:
    return tuple(file for file in tracked if file == path or file.startswith(path + "/"))


@functools.cache
def _closure_paths(members: frozenset[str]) -> frozenset[str]:
    return frozenset(path for member in members if (path := label_path(member)))


def declared(path: str, members: set[str] | frozenset[str], tracked: frozenset[str]) -> bool:
    """Is `path` — a file, or a directory the module walks — in the closure?

    A file is declared when it is a closure member. A directory is declared only when EVERY tracked file under it is a
    member: one declared file under `test/doc_scripts/` does not declare the
    rest a module walks. An untracked path inside `test/` is generated output
    and counts as declared (see below). The root itself and anything above it
    never are — a subprocess started at the root reads files no closure names.
    """
    if not path or path.startswith(("..", "/")):
        return False
    if path in members:
        return True
    under = _tracked_under(path, tracked)
    if not under and path.startswith(AMBIENT_DIR + "/"):
        # Nothing tracked there: generated output inside the test package
        # (`bench/results/`, `.out/`, a container-only mount point). The
        # suite's glob excludes generated files from every key on purpose, so
        # this is not an input a declaration could cover. `target/` is the
        # same shape OUTSIDE the package and stays undeclared (AM-6).
        return True
    return bool(under) and all(file in members for file in under)


def uncached_findings(
    *,
    acceptance: set[str],
    tags_of: dict[str, frozenset[str]],
    closure: dict[str, frozenset[str]],
    undeclared: dict[str, list[str]],
    uncached: frozenset[str],
) -> list[Finding]:
    """C-019: `external` on an acceptance target exactly where the predicate says.

    `undeclared` is `named_paths_by_module`'s map (every path each module
    names); a path counts as undeclared when the target's own input closure does
    not cover it, so the way off the list is a declaration, not a delete.
    """
    findings: list[Finding] = []
    tracked = tracked_files()
    modules = {label[len(ACCEPTANCE_PACKAGE):] + MODULE_SHAPED_PATTERN_SUFFIX: label for label in acceptance}
    for name in sorted(uncached - set(modules)):
        findings.append(Finding("tag-uncached-unknown", TAG_UNCACHED_UNKNOWN_MSG.format(name=name)))
    for name, label in sorted(modules.items()):
        if name not in undeclared:
            findings.append(Finding("tag-uncached-unread", TAG_UNCACHED_UNREAD_MSG.format(label=label)))
            continue
        members = _closure_paths(closure.get(label, frozenset()))
        reads = [path for path in undeclared[name] if not declared(path, members, tracked)]
        listed = name in uncached
        if reads and not listed:
            findings.append(
                Finding("tag-uncached-unlisted", TAG_UNCACHED_UNLISTED_MSG.format(label=label, reads=reads))
            )
        if listed and not reads:
            findings.append(Finding("tag-uncached-rot", TAG_UNCACHED_ROT_MSG.format(label=label)))
        if listed and CACHE_DEFEATING_TAG not in tags_of[label]:
            findings.append(
                Finding(
                    "tag-uncached-untagged",
                    TAG_UNCACHED_UNTAGGED_MSG.format(label=label, tag=CACHE_DEFEATING_TAG),
                )
            )
    return findings


HAND_DECLARED_READS: dict[str, frozenset[str]] = {
    # `task website:scripts:publish` / `task test:doc-scripts:list`: go-task
    # runs `test/scripts/doc_scripts_list.py`, a read inside a recipe.
    "test_doc_scripts_publish.py": frozenset({"//test:suite_scripts"}),
    # The runner resolves `OCX_SCHEMA_BINARY` from `env`, a value no AST reads
    # (analysis also refuses the `$(rlocationpath)` without it — two reds).
    "test_execution_records.py": frozenset({"//crates/ocx_schema:ocx_schema_bin"}),
    "test_project_env.py": frozenset({"//crates/ocx_schema:ocx_schema_bin"}),
    # Reviewed for `invisible_read_hints`: its `recordings/scripts` literals are
    # needles a sweep searches for, and the `test/recordings/*.py` it reads are
    # `:recording_sources`.
    "test_doc_scripts_one_tree.py": frozenset({"//test:recording_sources"}),
    # Reviewed: `scripts/setup.sh` is a file the test writes under `tmp_path`.
    "test_config_push.py": frozenset(),
}
"""Reads `named_paths_by_module` cannot derive, kept by hand, one module each.

Without it these inputs could leave `module_data` with every gate green — the
derivation only reds what it can see (dropping `:suite_scripts` from
`test_doc_scripts_publish` was measured green before this map). A NEW invisible
read is caught conservatively: a module whose source or imported helpers
launch `task` or name a path under a `test/` directory outside `:suite_inputs`
(`invisible_read_hints`) reds until it has an entry here — an empty one records
a review that found nothing to declare. A read neither spelling reveals (a path
assembled from parts, a subprocess that reads on its own) stays the residual.
An entry for a module that no longer exists reds too."""

IMPLIED_READS: dict[str, frozenset[str]] = {
    # `ocx --project <root>/ocx.toml` reads the lock beside the manifest.
    "//:ocx.toml": frozenset({"//:ocx.lock"}),
}
"""`{input: what reading it also reads}` — for every module, not one: whoever
declares the key must declare the values. The same residual as above."""

SLOTS_ENV = "OCX_ACCEPTANCE_SLOTS"
"""The env name `test/bazel.bzl` `_SLOTS_ENV` sets from `module_slots`."""

KNOWN_GROUP = "patch_global_slot"
"""The reader floor for `slot_findings`: a group `tests/test_patches.py` names
today (`lint/test_patch_global_slot.py` pins its members). A derivation that no
longer finds it has stopped reading, not found a clean tree. It was a
cross-module group until each patch test got a patch registry path of its own;
the floor only needs a group that is certainly there, not a shared one."""


def _stray_group_spellings(tree: ast.Module, calls: set[int]) -> list[str]:
    """Every `xdist_group` a reader of CALLS would miss: the name or attribute
    outside a call's `func` (`g = pytest.mark.xdist_group`), and the string
    `"xdist_group"` (`add_marker("xdist_group")`, `getattr(mark, ...)`). Over-reds
    on purpose — a docstring that merely mentions the mark contains more than
    the bare word and is not matched."""
    stray: list[str] = []
    for node in ast.walk(tree):
        if (isinstance(node, ast.Attribute) and node.attr == "xdist_group") or (
            isinstance(node, ast.Name) and node.id == "xdist_group"
        ):
            if id(node) not in calls:
                stray.append(ast.unparse(node))
        elif isinstance(node, ast.Constant) and node.value == "xdist_group":
            stray.append(ast.unparse(node))
    return stray


def foreign_group_spellings(helpers: dict[str, str]) -> dict[str, list[str]]:
    """`{helper path: every xdist_group spelling}` over the non-module files an
    acceptance module can load (`read_helper_sources`): any spelling there is a
    group no target's slot list can carry."""
    found: dict[str, list[str]] = {}
    for path, text in sorted(helpers.items()):
        spellings = list(_group_spellings(text))
        if spellings:
            found[path] = spellings
    return found


@functools.cache
def _group_spellings(text: str) -> tuple[str, ...]:
    tree = _parse(text)
    return tuple(
        [ast.unparse(node) for node in ast.walk(tree) if isinstance(node, ast.Call) and _call_name(node.func) == "xdist_group"]
        + _stray_group_spellings(tree, set())
    )


def xdist_groups(source: str) -> tuple[set[str], list[str]]:
    groups, unresolved = _xdist_groups(source)
    return set(groups), list(unresolved)


@functools.cache
def _xdist_groups(source: str) -> tuple[frozenset[str], tuple[str, ...]]:
    """`(group names, unresolvable spellings)` one module's source names.

    A name is resolved when the `xdist_group(...)` argument is a string literal,
    or a subscript of a module-level dict whose values are all string literals
    (then every value counts — over-requiring a lock is safe, under-requiring
    is not). Anything else is returned as unresolved and reds, and so is any
    spelling of the mark that is not a call (`_stray_group_spellings`).
    """
    tree = _parse(source)
    dicts: dict[str, set[str]] = {}
    for node in tree.body:
        value = node.value if isinstance(node, (ast.Assign, ast.AnnAssign)) else None
        if not isinstance(value, ast.Dict) or not all(
            isinstance(v, ast.Constant) and isinstance(v.value, str) for v in value.values
        ):
            continue
        targets = node.targets if isinstance(node, ast.Assign) else [node.target]
        for target in targets:
            if isinstance(target, ast.Name):
                dicts[target.id] = {v.value for v in value.values}  # type: ignore[union-attr]
    groups: set[str] = set()
    unresolved: list[str] = []
    calls: set[int] = set()
    for node in ast.walk(tree):
        if not (isinstance(node, ast.Call) and _call_name(node.func) == "xdist_group"):
            continue
        calls.add(id(node.func))
        arg = node.args[0] if node.args else next((k.value for k in node.keywords if k.arg == "name"), None)
        match arg:
            case ast.Constant(value=str(name)):
                groups.add(name)
            case ast.Subscript(value=ast.Name(id=name)) if name in dicts:
                groups |= dicts[name]
            case _:
                unresolved.append(ast.unparse(node))
    return frozenset(groups), tuple(unresolved + _stray_group_spellings(tree, calls))


def slot_findings(
    *,
    acceptance: set[str],
    env_of: dict[str, dict[str, str]],
    sources: dict[str, str],
    helpers: dict[str, str] | None = None,
) -> list[Finding]:
    """Each acceptance target declares exactly the xdist groups its module names
    (`test/bazel.bzl` § Concurrency), and no group is spelled anywhere this
    derivation does not read. `helpers=None` reads the live helper tree."""
    findings: list[Finding] = []
    if helpers is None:
        helpers = read_helper_sources()
    for path, spellings in foreign_group_spellings(helpers).items():
        findings.append(Finding("tag-slot-foreign", TAG_SLOT_FOREIGN_MSG.format(path=path, spelling=spellings)))
    derived = {name: xdist_groups(text) for name, text in sources.items()}
    members: dict[str, set[str]] = {}
    for name, (groups, _) in derived.items():
        for group in groups:
            members.setdefault(group, set()).add(name)
    if KNOWN_GROUP not in members:
        findings.append(Finding("tag-slot-reader", TAG_SLOT_READER_MSG.format(found=sorted(members), known=KNOWN_GROUP)))
    for label in sorted(acceptance):
        name = label[len(ACCEPTANCE_PACKAGE):] + MODULE_SHAPED_PATTERN_SUFFIX
        groups, unresolved = derived.get(name, (set(), []))
        for spelling in unresolved:
            findings.append(
                Finding("tag-slot-unresolved", TAG_SLOT_UNRESOLVED_MSG.format(label=label, spelling=spelling))
            )
        # Every group, not only a shared one: a group one module names is
        # serial inside one pytest process, and two runs of that module from
        # sibling checkouts are two processes.
        required = sorted(groups)
        declared = env_of.get(label, {}).get(SLOTS_ENV, "")
        if sorted(declared.split()) != required:
            findings.append(
                Finding(
                    "tag-slot-undeclared",
                    TAG_SLOT_UNDECLARED_MSG.format(label=label, groups=required, declared=declared),
                )
            )
    return findings


def invisible_read_hints(text: str, moved_out: set[str] | frozenset[str]) -> list[str]:
    return list(_invisible_read_hints(text, frozenset(moved_out)))


@functools.cache
def _invisible_read_hints(text: str, moved_out: frozenset[str]) -> tuple[str, ...]:
    """What in one file's source the path derivation cannot follow: a `task`
    launch (the recipe's reads are go-task's), or a string literal naming a path
    under one of `moved_out`, the `test/` directories `:suite_inputs` does not
    carry. Docstrings are prose, not reads, and are skipped."""
    hints: list[str] = []
    for value in _string_literals(text):
        segments = [part for part in value.split("/") if part not in ("", ".", "..")]
        if segments[:1] == [AMBIENT_DIR]:
            segments = segments[1:]
        if value == "task" or value.startswith("task "):
            hints.append(f"launches `task` ({value!r})")
        elif len(segments) >= 2 and segments[0] in moved_out:
            hints.append(f"names {value!r}")
    return tuple(hints)


@functools.cache
def _string_literals(text: str) -> tuple[str, ...]:
    """Every string literal in one file's source, docstrings skipped, in walk order."""
    tree = _parse(text)
    docs = {id(node.value) for node in ast.walk(tree) if isinstance(node, ast.Expr)}
    return tuple(
        node.value
        for node in ast.walk(tree)
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and id(node) not in docs
    )


def hand_read_findings(
    *,
    acceptance: set[str],
    closure: dict[str, frozenset[str]],
    undeclared: dict[str, list[str]],
    texts: dict[str, str],
) -> list[Finding]:
    """`HAND_DECLARED_READS` and `IMPLIED_READS`, against each target's closure;
    and every module `invisible_read_hints` flags, in itself or in a file its
    `undeclared` reads name (`texts` maps a repo path to its source), must have
    an entry. The directories counted as moved out are derived: those under
    `test/` holding no member of `:suite_inputs`."""
    findings: list[Finding] = []
    modules = {label[len(ACCEPTANCE_PACKAGE):] + MODULE_SHAPED_PATTERN_SUFFIX: label for label in acceptance}
    for name in sorted(set(HAND_DECLARED_READS) - set(modules)):
        findings.append(Finding("tag-hand-read-unknown", TAG_HAND_READ_UNKNOWN_MSG.format(name=name)))
    suite = {path for member in closure.get(SUITE_INPUTS_LABEL, frozenset()) if (path := label_path(member))}
    under_test = {path.split("/")[1] for path in tracked_files() if path.startswith("test/") and path.count("/") >= 2}
    moved_out = {d for d in under_test if not any(member.startswith(f"test/{d}/") for member in suite)}
    for name, label in sorted(modules.items()):
        if name in HAND_DECLARED_READS:
            continue
        reach = [f"test/tests/{name}", *undeclared.get(name, [])]
        hints = [hint for path in reach if path in texts for hint in invisible_read_hints(texts[path], moved_out)]
        if hints:
            findings.append(
                Finding("tag-hand-read-unreviewed", TAG_HAND_READ_UNREVIEWED_MSG.format(label=label, hints=hints))
            )
    for name, label in sorted(modules.items()):
        inputs = closure.get(label, frozenset())
        required = set(HAND_DECLARED_READS.get(name, frozenset()))
        for trigger, implied in IMPLIED_READS.items():
            if trigger in inputs:
                required |= implied
        missing = sorted(required - inputs)
        if missing:
            findings.append(Finding("tag-hand-read-undeclared", TAG_HAND_READ_MSG.format(label=label, missing=missing)))
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

    counted = len(members)
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
    undeclared: dict[str, list[str]] | None = None,
    uncached: frozenset[str] | None = None,
    sources: dict[str, str] | None = None,
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
    # carries no `external` and the blanket rule above would red every one of its
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
        if sources is None:
            sources = read_acceptance_sources()
        if sweeps is None or patterns is None or undeclared is None:
            sweeps = sweep_requirements(sources) if sweeps is None else sweeps
            patterns = sweep_patterns(sources) if patterns is None else patterns
            undeclared = _named_paths_of(tuple(sorted(sources.items()))) if undeclared is None else undeclared
        if uncached is None:
            uncached = frozenset(
                Path(module).name for module in read_uncached_modules()
            )
        findings.extend(
            sweep_findings(acceptance=acceptance, sweeps=sweeps, patterns=patterns)
        )
        findings.extend(suite_inputs_findings(records=records, closure=closure))
        findings.extend(
            slot_findings(
                acceptance=acceptance,
                env_of={record.label: dict(record.env) for record in records},
                sources=sources,
            )
        )
        findings.extend(
            hand_read_findings(
                acceptance=acceptance,
                closure=closure,
                undeclared=undeclared,
                texts={**read_helper_sources(), **{f"test/tests/{n}": t for n, t in sources.items()}},
            )
        )
        findings.extend(
            uncached_findings(
                acceptance=acceptance,
                tags_of=tags_of,
                closure=closure,
                undeclared=undeclared,
                uncached=uncached,
            )
        )
        listed = {ACCEPTANCE_PACKAGE + Path(name).stem for name in uncached}
        for label in sorted(acceptance):
            # C-019: `external` is admitted on a listed module and nowhere else;
            # `local` and `no-cache` stay refused on every target, listed or not.
            forbidden = ACCEPTANCE_SUPPRESSING_TAGS - (
                {CACHE_DEFEATING_TAG} if label in listed else set()
            )
            if SERIALISING_TAG in tags_of[label]:
                findings.append(
                    Finding("tag-acceptance-serialised", TAG_SERIALISED_MSG.format(label=label, tag=SERIALISING_TAG))
                )
            present = sorted(tags_of[label] & forbidden)
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
    exactly like a run where all of them passed."""

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
    undeclared: dict[str, list[str]] | None = None,
    uncached: frozenset[str] | None = None,
    sources: dict[str, str] | None = None,
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
            undeclared=undeclared,
            uncached=uncached,
            sources=sources,
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
    add_inputs: set[str] | None = None,
    env: dict[str, str] | None = None,
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
        if add_inputs is not None:
            record["rule"]["ruleInput"] = sorted(set(record["rule"].get("ruleInput", [])) | add_inputs)
        if env is not None:
            attributes = [a for a in record["rule"].get("attribute", []) if a.get("name") != "env"]
            attributes.append(
                {
                    "name": "env",
                    "type": "STRING_DICT",
                    "stringDictValue": [{"key": k, "value": v} for k, v in sorted(env.items())],
                    "explicitlySpecified": True,
                }
            )
            record["rule"]["attribute"] = attributes
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
    undeclared: dict[str, list[str]] | None = None,
    uncached: frozenset[str] | None = None,
    sources: dict[str, str] | None = None,
) -> list[Finding]:
    """Write a mutated stream, re-read it from disk, and judge it through `run_check`.

    Through the shipped entry point, never through the imported comparator: the
    reader is half of what this file owns, and a proof that skips it proves the
    other WP's function.
    """
    _dump(path, records)
    findings, _, _ = run_check(
        stage=stage,
        bazel="",
        query_json=path,
        sweeps=sweeps,
        patterns=patterns,
        undeclared=undeclared,
        uncached=uncached,
        sources=sources,
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
    # The first no-sandbox genrule, not the first genrule: an ordinary sandboxed
    # genrule (`//crates/ocx_schema:schemas`) sorts ahead of the cast targets.
    subject = next(
        (r for r in genrules if NO_SANDBOX_TAG in _attr(r, "tags").get("stringListValue", [])),
        genrules[0],
    )
    victim = subject["rule"]["name"]
    tags = frozenset(_attr(subject, "tags").get("stringListValue", []))
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

    The live graph's three real `rust_binary` targets — the acceptance suite's
    binary under test and launcher (`//crates/ocx_cli:ocx`,
    `//crates/ocx_shim:ocx_shim`) and `//crates/ocx_schema:ocx_schema_bin`
    (plan_test_speed_tiers.md C-020) — are the subject of the unmutated green
    below: each must carry an explicitly specified `stamp = 0`. Every red state is
    one real `rust_library` record retyped to `rust_binary` with its `stamp` set
    to the default rules_rust measurably gives one (-1), so the reds do not
    depend on how the real binary is written.
    """
    checks = 0
    path = scratch / "mode2.json"
    victim = next(r["rule"]["name"] for r in live if r["rule"]["ruleClass"] == "rust_library")

    binaries = sorted(r["rule"]["name"] for r in live if r["rule"]["ruleClass"] == "rust_binary")
    expect(
        binaries
        == ["//crates/ocx_cli:ocx", "//crates/ocx_schema:ocx_schema_bin", "//crates/ocx_shim:ocx_shim"],
        f"the live graph's rust_binary targets are {binaries}; this proof was re-read against "
        "exactly //crates/ocx_cli:ocx, //crates/ocx_schema:ocx_schema_bin and "
        "//crates/ocx_shim:ocx_shim, and must be re-read against the real ones",
    )
    expect(
        all(record.stamp_zero for record in read_rules("\n".join(json.dumps(r) for r in live))[0]
            if record.label in binaries),
        f"{binaries} must carry an explicitly specified `stamp = 0` — the live green below is "
        "otherwise judging a binary clause 2 would red",
    )
    print(
        f"S-012 mode 2 — the live graph has {len(binaries)} rust_binary target(s) {binaries}, each "
        "with an explicit `stamp = 0`; every red state below is one real rust_library retyped"
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


def _graph_codes(findings: list[Finding]) -> list[str]:
    """`codes()` without C-019's source-side findings. A target that loses its
    input declaration also stops covering the paths its module reads, so the
    uncached clause rightly fires beside the one a case is about; the cases
    that judge the graph clauses compare on these codes, and `prove_uncached`
    owns the C-019 ones."""
    return [code for code in codes(findings) if not code.startswith("tag-uncached")]


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
    # A cached target: C-019's listed ones carry `external`, which answers
    # every case below before the case's own mutation can.
    victim = next(
        r["rule"]["name"]
        for r in acceptance
        if CACHE_DEFEATING_TAG not in set(_attr(r, "tags").get("stringListValue", []))
    )
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
        f"S-012 mode 4 GREEN: {len(acceptance)} no-sandbox acceptance target(s) are silent — each "
        f"declares {sorted(ACCEPTANCE_DECLARED_INPUTS)} through its input closure, and only the "
        f"C-019 `UNCACHED_MODULES` targets carry `{CACHE_DEFEATING_TAG}`"
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
        _graph_codes(findings) == ["tag-acceptance-undeclared", "tag-no-sandbox"],
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
    # one-hop reader cannot see, and it reds every target rather than one: the
    # compose definition left the closure of every acceptance target at once.
    thinned = _mutate(live, group, drop_inputs={"//test:docker-compose.yml"})
    findings = _judge(path, thinned)
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    expect(
        "//test:docker-compose.yml" not in {r.label: r.inputs for r in reread}[group],
        "the group-thinning mutation did not land",
    )
    undeclared = [f for f in findings if f.code == "tag-acceptance-undeclared"]
    # C-019's listed targets carry `external`, which credits them under the
    # blanket rule, so they are the one set this clause does not re-judge.
    uncached = sum(
        CACHE_DEFEATING_TAG in set(_attr(r, "tags").get("stringListValue", [])) for r in acceptance
    )
    expect(
        len(undeclared) == ACCEPTANCE_MODULE_TARGETS - uncached,
        f"{len(undeclared)} acceptance target(s) red, expected all {ACCEPTANCE_MODULE_TARGETS} "
        f"but the {uncached} uncached — the reader stopped at one hop and never expanded the group",
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
    tagged = _mutate(live, victim, tags=[NO_SANDBOX_TAG, CACHE_DEFEATING_TAG])
    tagged = _mutate(tagged, victim, drop_inputs={group})
    findings = _judge(path, tagged)
    landed = _landed(path, victim)
    expect(
        CACHE_DEFEATING_TAG in landed.tags and group not in landed.inputs,
        f"the tag-plus-drop mutation did not land: {sorted(landed.tags)}",
    )
    expect(
        _graph_codes(findings) == ["tag-acceptance-suppressed"],
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
        _graph_codes(findings) == ["tag-cache-insufficient", "tag-no-sandbox"],
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
    # A cached target: C-019's listed ones carry `external`, which answers
    # every case below before the case's own mutation can.
    victim = next(
        r["rule"]["name"]
        for r in acceptance
        if CACHE_DEFEATING_TAG not in set(_attr(r, "tags").get("stringListValue", []))
    )

    green = _judge(path, live)
    expect(
        "tag-acceptance-suppressed" not in codes(green),
        f"the real tag set must be silent, got {codes(green)}",
    )
    victim_record = next(r for r in acceptance if r["rule"]["name"] == victim)
    live_tags = sorted(_attr(victim_record, "tags").get("stringListValue", []))
    print(
        f"S-012 mode 5 GREEN: {victim} carries {live_tags}, and none of the {len(acceptance)} "
        f"acceptance targets carries one of {sorted(ACCEPTANCE_SUPPRESSING_TAGS)} outside C-019's "
        f"listed `{CACHE_DEFEATING_TAG}`"
    )
    checks += 1

    for tag in (CACHE_DEFEATING_TAG, "local", "no-cache"):
        mutated = _mutate(live, victim, tags=[NO_SANDBOX_TAG, tag])
        findings = _judge(path, mutated)
        landed = _landed(path, victim)
        expect(tag in landed.tags, f"the {tag} mutation did not land: {sorted(landed.tags)}")
        suppressed = [f for f in findings if f.code == "tag-acceptance-suppressed"]
        expect(len(suppressed) == 1, f"{len(suppressed)} targets red on {tag}, expected 1")
        expect(victim in suppressed[0].message, "the finding does not name the label")
        expect(f"'{tag}'" in suppressed[0].message, f"the finding does not name {tag}")
        print(f"S-012 mode 5 RED  : {suppressed[0].message}")
        checks += 1

    # AM-9: `exclusive` is refused too — it is no cache tag, but it takes the
    # concurrent suite back to one lane — and so is an rc `--local_test_jobs`.
    mutated = _mutate(live, victim, tags=[NO_SANDBOX_TAG, SERIALISING_TAG])
    serialised = _only(_judge(path, mutated), "tag-acceptance-serialised")
    expect(SERIALISING_TAG in _landed(path, victim).tags, "the exclusive mutation did not land")
    expect(victim in serialised.message, "the serialised finding does not name the label")
    print(f"S-012 mode 5 RED  : {serialised.message}")
    expect(rc_serialisation_findings() == [], "the live rc files must name no --local_test_jobs")
    rc_root = scratch / "rc"
    rc_root.mkdir(exist_ok=True)
    live_rc = (REPO_ROOT / ".bazelrc").read_text(encoding="utf-8")
    (rc_root / ".bazelrc").write_text(live_rc + "test --local_test_jobs=1\n", encoding="utf-8")
    planted = rc_serialisation_findings(rc_root)
    expect(codes(planted) == ["serialise-global"], f"a planted rc line must red, got {codes(planted)}")
    print(f"S-012 mode 5 GREEN/RED: the live .bazelrc is silent; with one line appended: {planted[0].message}")
    checks += 2
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

    # A whole directory glob deleted. `sigstore/**` by name: every module reads
    # it through the fixture `tests/conftest.py` imports, so it is the shared
    # directory the per-module derivation would charge to all 172 at once.
    sigstore = {m for m in members if m.startswith(ACCEPTANCE_PACKAGE + "sigstore/")}
    expect(len(sigstore) > 0, "the live group carries no sigstore/ file, so this case proves nothing")
    findings = _judge(path, _mutate(live, group, drop_inputs=sigstore))
    landed = _landed(path, group)
    expect(not (sigstore & landed.inputs), "the sigstore drop did not land")
    directory = _only(findings, "tag-suite-inputs-directory")
    expect("sigstore/" in directory.message, "the finding does not name the directory")
    print(f"S-012 mode 6 RED  : {directory.message}")
    checks += 1

    # A single named file dropped from `srcs`: `pyproject.toml`, the pytest
    # configuration every module runs under.
    findings = _judge(path, _mutate(live, group, drop_inputs={"//test:pyproject.toml"}))
    landed = _landed(path, group)
    expect("//test:pyproject.toml" not in landed.inputs, "the pyproject.toml drop did not land")
    named = _only(findings, "tag-suite-inputs-file")
    expect("//test:pyproject.toml" in named.message, "the finding does not name the file")
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
    """The modules that read their SIBLINGS' source, derived — and refused.

    Two halves, and they fail differently. The derivation is pure over source
    text and is shown here detecting a sweeper and sparing an arena walk. The
    graph clause is shown green on the live tree, which has no sweeper left
    (they live in `test/lint/`, plan_test_speed_tiers.md C-011), and red on a
    sweeper planted into the live sources — **even with every sibling declared
    among its inputs**, the shape the retired `extra_data` table used to make
    green, because a sweep's verdict is only as fresh as the whole suite and no
    per-target declaration can keep a cached one honest.
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

    # (b) the live tree: every module read, none of them a sweeper, graph green.
    sources = read_acceptance_sources()
    expect(
        len(sources) == ACCEPTANCE_MODULE_TARGETS,
        f"read {len(sources)} acceptance modules, expected {ACCEPTANCE_MODULE_TARGETS} — a "
        "derivation over a short read requires nothing of anybody",
    )
    live_sweeps = sweep_requirements(sources)
    expect(
        not live_sweeps,
        f"the live tree derives sweeping acceptance modules {sorted(live_sweeps)} — a sweep "
        "belongs in test/lint/",
    )
    green = _judge(path, live)
    expect(
        "tag-acceptance-sweeper" not in codes(green),
        f"the live graph must carry no sweeping acceptance target, got {codes(green)}",
    )
    print(
        f"S-012 mode 7 GREEN: {len(sources)} acceptance modules read, none sweeps its siblings, "
        "and the live graph carries no sweeper finding"
    )
    checks += 1

    # (c) a sweeper planted into the live sources, its target handed every
    # sibling as a declared input — the most a BUILD file could ever declare.
    planted = min(sources)
    label = ACCEPTANCE_PACKAGE + planted[: -len(MODULE_SHAPED_PATTERN_SUFFIX)]
    siblings = {module_label(name) for name in sources}
    planted_sources = dict(sources)
    planted_sources[planted] += "\nfor p in TESTS_DIR.glob('test_*.py'): read(p)\n"
    declared = _mutate(live, label, add_inputs=siblings)
    findings = _judge(
        path,
        declared,
        sweeps=sweep_requirements(planted_sources),
        patterns=sweep_patterns(planted_sources),
    )
    landed = _landed(path, label)
    expect(siblings <= landed.inputs, "the sibling declaration did not land")
    sweeper = _only(findings, "tag-acceptance-sweeper")
    expect(label in sweeper.message and "test/lint/" in sweeper.message, "the finding names neither")
    print(f"S-012 mode 7 RED  : {sweeper.message}")
    checks += 1
    return checks


def prove_predicate() -> int:
    """C-019 (f) and the predicate's shapes, on source text this function owns.

    Pure: no graph. Each shape is one the live suite spells today or a spelling
    one line away from it; the controls are the two C-019 names outright.
    """
    module = "test/tests/test_planted.py"
    named = {
        "multi-segment join": ('X = PROJECT_ROOT / "target" / "release" / "ocx_schema"\n', "target/release/ocx_schema"),
        "single 'a/b' literal": ('X = PROJECT_ROOT / "website/src/index.md"\n', "website/src/index.md"),
        "parents[2] base": ('X = Path(__file__).resolve().parents[2] / "crates" / "x"\n', "crates/x"),
        "parent chain base": ('X = Path(__file__).parent.parent.parent / ".github" / "w.yml"\n', ".github/w.yml"),
        "parents[1] onto the subpackage": ('X = Path(__file__).parents[1] / "doc_scripts"\n', "test/doc_scripts"),
        "aliased root": ('R = Path(__file__).resolve().parents[2]\nX = R / "packaging"\n', "packaging"),
        "aliased prefix": ('D = PROJECT_ROOT / "test"\nX = D / "doc_scripts" / "a.sh"\n', "test/doc_scripts/a.sh"),
        "function returning the root": (
            'def _root():\n    return Path(__file__).resolve().parent.parent.parent\nX = _root() / "website"\n',
            "website",
        ),
        "loop over the ancestors": (
            'for a in Path(__file__).resolve().parents:\n    c = a / "website" / "x.md"\n',
            "website/x.md",
        ),
        "joinpath": ('X = PROJECT_ROOT.joinpath("target", "debug")\n', "target/debug"),
        "os.path.join": ('X = os.path.join(PROJECT_ROOT, "crates")\n', "crates"),
        "cwd= the root": ('subprocess.run(["task"], cwd=str(PROJECT_ROOT))\n', ""),
        "escape above the root": ('X = Path(__file__).parents[3] / "elsewhere"\n', "../elsewhere"),
        # Security review item 3: a traced base used any way but a constant
        # join names itself, so the root it reaches is the finding.
        "root under an f-string join": ('X = PROJECT_ROOT / f"website/{name}"\n', ""),
        "root walked": ('for p in PROJECT_ROOT.rglob("*.md"):\n    pass\n', ""),
        "root in argv": ('subprocess.run(["git", "-C", str(PROJECT_ROOT), "ls-files"])\n', ""),
        "string concatenation": ('X = str(PROJECT_ROOT) + "/website/a.md"\n', "website/a.md"),
        # Item 4: the runner's cwd is `test/`, so `../` leaves the package.
        "relative escape": ('X = Path("../website/a.md")\n', "website/a.md"),
        "relative escape opened": ('open("../crates/x.rs").read()\n', "crates/x.rs"),
        # ...and a relative literal that stays inside it is `test/<literal>`:
        # `scenarios/` and `specs/` left `:suite_inputs` (review 3, finding 1).
        "relative literal": ('X = Path("scenarios") / "exec"\n', "test/scenarios/exec"),
        "relative literal opened": ('open("taskfile.yml").read()\n', "test/taskfile.yml"),
        "cwd joined": ('X = Path.cwd() / "specs"\n', "test/specs"),
    }
    checks = 0
    for shape, (source, expected) in named.items():
        got = named_repo_paths(source, module)
        expect(expected in got, f"predicate shape {shape!r}: expected {expected!r} in {got}")
        checks += 1
    print(f"C-019 predicate GREEN: {len(named)} undeclared-read shapes each named, e.g. {sorted(named)[:3]}")

    # (f) the two controls C-019 names: a literal joined onto `tmp_path`, and a
    # root-shaped literal that only ever appears in a message string.
    controls = {
        "tmp_path join": 'def test_x(tmp_path):\n    d = tmp_path / "website" / "src"\n',
        "message string": 'def test_x():\n    assert ok, "website/src/docs is stale; see target/release"\n',
        # An absolute literal is no repository path, wherever the runner is.
        "Path of an absolute literal": 'X = Path("/etc") / "hosts"\n',
    }
    # Bindings are scoped per function: `candidate` bound onto the root in one
    # function must not leak into another that binds it onto `tmp_path` (the
    # live `test_shell_reconcile.py` shape, which read as `website/...`).
    controls["same name, another function"] = (
        "def locate():\n"
        "    for a in Path(__file__).resolve().parents:\n"
        '        candidate = a / "website" / "x.md"\n'
        "        return candidate\n"
        "def arena(tmp_path):\n"
        '    candidate = tmp_path / "bin" / "ocx"\n'
        "    candidate.parent.mkdir()\n"
    )
    for shape, source in controls.items():
        got = named_repo_paths(source, module)
        if shape == "same name, another function":
            expect(got == ["website/x.md"], f"control {shape!r} must name only its own read, got {got}")
        else:
            expect(got == [], f"control {shape!r} must name nothing, got {got}")
        checks += 1
    print(f"C-019 (f) CONTROL: {sorted(controls)} name nothing")

    # A helper's reads are charged to the modules that import it, and every
    # `conftest.py` above a module to that module. The first case is the live
    # shape the security review found: a cosign fixture reading the root
    # `ocx.toml` on behalf of five modules that name nothing themselves.
    helpers = {
        "test/tests/fixtures/planted.py": 'P = PROJECT_ROOT / "ocx.toml"\n',
        "test/tests/planted_shim.py": 'ROOTS = (Path(__file__).resolve().parents[2],)\n',
        "test/src/inert.py": "Y = 1\n",
    }
    modules = {
        "test_fixture_user.py": "from tests.fixtures import planted\n",
        "test_bare_import.py": "from planted_shim import ROOTS\n",
        "test_clean.py": "from src import inert\n",
    }
    # An imported helper's SOURCE is a read as well, whether or not it names a
    # path: `test_state_providers.py` reaches `recordings/setups.py` only by
    # import, and the group that file lives in is declared per module.
    charged = named_paths_by_module(modules, helpers)
    expect(
        charged["test_fixture_user.py"] == ["ocx.toml", "test/tests/fixtures/planted.py"],
        f"helper read not charged: {charged}",
    )
    expect(
        charged["test_bare_import.py"] == ["test/tests/planted_shim.py"],
        f"a helper's bare root must not be charged, its source must: {charged}",
    )
    expect(charged["test_clean.py"] == ["test/src/inert.py"], f"an inert helper's source not charged: {charged}")
    conftest = named_paths_by_module(
        {"test_any.py": "x = 1\n"}, {"test/conftest.py": 'C = PROJECT_ROOT / "website"\n'}
    )
    expect(conftest["test_any.py"] == ["test/conftest.py", "website"], f"conftest read not charged: {conftest}")
    transitive = named_paths_by_module(
        {"test_deep.py": "from src import front\n"},
        {"test/src/front.py": "from recordings.setups import S\n", "test/recordings/setups.py": "S = 1\n"},
    )
    expect(
        transitive["test_deep.py"] == ["test/recordings/setups.py", "test/src/front.py"],
        f"a helper imported through another helper not charged: {transitive}",
    )
    print(
        "C-019 predicate GREEN: a fixture's `ocx.toml` read is charged to its importer, a "
        "conftest's to every module under it, and every imported helper's own source — "
        "transitively — to the module importing it"
    )
    checks += 3

    # An imported SIBLING module is charged as a file, with its own reads and
    # the helpers it imports, in both spellings pytest resolves; the sibling
    # itself is charged nothing for being imported.
    siblings = named_paths_by_module(
        {
            "test_importer.py": "from tests.test_lib import f\n",
            "test_bare_importer.py": "import test_lib\n",
            "test_lib.py": "from tests.fixtures import planted\ndef f():\n    return 1\n",
        },
        helpers,
    )
    for importer in ("test_importer.py", "test_bare_importer.py"):
        expect(
            siblings[importer] == ["ocx.toml", "test/tests/fixtures/planted.py", "test/tests/test_lib.py"],
            f"sibling import not charged to {importer}: {siblings}",
        )
    expect(
        siblings["test_lib.py"] == ["ocx.toml", "test/tests/fixtures/planted.py"],
        f"the imported sibling charged itself: {siblings}",
    )
    print("C-019 predicate GREEN: an imported sibling module is charged to its importer with its reads")
    checks += 1

    # `declared`: a directory is declared file by file, never by one member.
    tracked = frozenset({"test/doc_scripts/a.sh", "test/doc_scripts/b.sh", "test/src/x.py"})
    members = {"test/doc_scripts/a.sh", "test/src/x.py"}
    verdicts = {
        "test/doc_scripts": False,  # b.sh is tracked there and undeclared
        "test/doc_scripts/a.sh": True,
        "test/src": True,
        "test/bench/results": True,  # untracked inside the test package: generated
        "target/release/ocx_schema": False,  # untracked OUTSIDE it: AM-6's case
        "test/bin/ocx": True,
        "": False,
    }
    for path, want in verdicts.items():
        expect(declared(path, members, tracked) is want, f"declared({path!r}) should be {want}")
    print(f"C-019 declared GREEN/RED: {verdicts}")
    checks += 1
    return checks


def prove_uncached(scratch: Path, live: list[dict]) -> int:
    """C-019 (a)-(e) on the live capture and the live sources, one mutation each."""
    checks = 0
    path = scratch / "mode8.json"
    sources = read_acceptance_sources()
    expect(len(sources) == ACCEPTANCE_MODULE_TARGETS, f"read {len(sources)} acceptance modules")
    live_named = named_paths_by_module(sources)
    live_list = frozenset(Path(m).name for m in read_uncached_modules())

    def uncached_codes(findings: list[Finding]) -> list[str]:
        return [c for c in codes(findings) if c.startswith("tag-uncached") or c == "tag-acceptance-suppressed"]

    green = _judge(path, live, undeclared=live_named, uncached=live_list)
    expect(uncached_codes(green) == [], f"the live tree must be silent, got {uncached_codes(green)}")
    print(f"C-019 GREEN: the live list {sorted(live_list)} is exactly what the predicate yields")
    checks += 1

    # The victim: a module the predicate leaves alone today, so each case below
    # is decided by the one thing it plants.
    victim_module = "test_install.py"
    victim = ACCEPTANCE_PACKAGE + "test_install"
    expect(victim_module not in live_list, f"{victim_module} is listed, pick another victim")
    planted_sources = dict(sources)
    planted_sources[victim_module] += '\nPLANTED = PROJECT_ROOT / "website" / "src" / "index.md"\n'
    planted = named_paths_by_module(planted_sources)
    expect("website/src/index.md" in planted[victim_module], "the planted read did not land")

    # (a) an undeclared-root module, unlisted.
    findings = _judge(path, live, undeclared=planted, uncached=live_list)
    unlisted = _only(findings, "tag-uncached-unlisted")
    expect(victim in unlisted.message and "website/src/index.md" in unlisted.message, "(a) names neither")
    print(f"C-019 (a) RED  : {unlisted.message}")
    checks += 1

    # The same, spelled relative to the runner's working directory (`test/`):
    # the directories that left `:suite_inputs` are reachable without a root.
    for plant, read in (
        ('\nPLANTED = Path("scenarios/exec/diamond-dep-env.sh")\n', "test/scenarios/exec/diamond-dep-env.sh"),
        ('\nPLANTED = Path.cwd() / "specs"\n', "test/specs"),
        ('\nPLANTED = open("taskfile.yml")\n', "test/taskfile.yml"),
    ):
        relative = dict(sources)
        relative[victim_module] += plant
        named = named_paths_by_module(relative)
        expect(read in named[victim_module], f"the planted {plant.strip()!r} did not land")
        unlisted = _only(_judge(path, live, undeclared=named, uncached=live_list), "tag-uncached-unlisted")
        expect(victim in unlisted.message and read in unlisted.message, f"{plant.strip()!r} names neither")
        print(f"C-019 cwd RED  : {plant.strip()}: {unlisted.message}")
        checks += 1

    # (b) the same module listed, its target carrying `external`: green.
    tagged = _mutate(live, victim, tags=[NO_SANDBOX_TAG, CACHE_DEFEATING_TAG])
    findings = _judge(path, tagged, undeclared=planted, uncached=live_list | {victim_module})
    expect(CACHE_DEFEATING_TAG in _landed(path, victim).tags, "the external mutation did not land")
    expect(uncached_codes(findings) == [], f"(b) listed + external must be green, got {uncached_codes(findings)}")
    print(f"C-019 (b) GREEN: {victim} listed and tagged `{CACHE_DEFEATING_TAG}` is silent")
    checks += 1

    # (c) listed, tagged, but the module no longer names an undeclared root: rot.
    findings = _judge(path, tagged, undeclared=live_named, uncached=live_list | {victim_module})
    rot = _only(findings, "tag-uncached-rot")
    expect(victim in rot.message, "(c) does not name the label")
    print(f"C-019 (c) RED  : {rot.message}")
    checks += 1

    # (d) listed, but tagged `no-cache` instead of `external`.
    nocache = _mutate(live, victim, tags=[NO_SANDBOX_TAG, "no-cache"])
    findings = _judge(path, nocache, undeclared=planted, uncached=live_list | {victim_module})
    expect("no-cache" in _landed(path, victim).tags, "the no-cache mutation did not land")
    suppressed = _only(findings, "tag-acceptance-suppressed")
    untagged = _only(findings, "tag-uncached-untagged")
    expect("'no-cache'" in suppressed.message and victim in untagged.message, "(d) names neither")
    print(f"C-019 (d) RED  : {suppressed.message}")
    print(f"C-019 (d) RED  : {untagged.message}")
    checks += 1

    # (e) `external` on a module that is not listed.
    findings = _judge(path, tagged, undeclared=live_named, uncached=live_list)
    suppressed = _only(findings, "tag-acceptance-suppressed")
    expect(f"'{CACHE_DEFEATING_TAG}'" in suppressed.message and victim in suppressed.message, "(e)")
    print(f"C-019 (e) RED  : {suppressed.message}")
    checks += 1

    # The way off the list (WP-08b): the read, declared as an input, is covered.
    declared = _mutate(live, victim, add_inputs={"//website:src/index.md"})
    findings = _judge(path, declared, undeclared=planted, uncached=live_list)
    expect("//website:src/index.md" in _landed(path, victim).inputs, "the declaration did not land")
    expect(uncached_codes(findings) == [], f"a declared read must be silent, got {uncached_codes(findings)}")
    print("C-019 GREEN: the same read, declared as an input of the target, needs no listing")
    checks += 1

    # A sibling-module import (adversary finding 4): the sibling is excluded
    # from `//test:suite_inputs`, so an undeclared import is an undeclared read.
    sibling = "test_env.py"
    imported = dict(sources)
    imported[victim_module] += f"\nfrom tests.{sibling[:-3]} import *  # noqa: F403\n"
    importing = named_paths_by_module(imported)
    expect(f"test/tests/{sibling}" in importing[victim_module], "the planted import did not land")
    findings = _judge(path, live, undeclared=importing, uncached=live_list)
    unlisted = _only(findings, "tag-uncached-unlisted")
    expect(victim in unlisted.message and f"test/tests/{sibling}" in unlisted.message, "import names neither")
    print(f"C-019 import RED  : {unlisted.message}")
    declared = _mutate(live, victim, add_inputs={f"{ACCEPTANCE_PACKAGE[:-1]}:tests/{sibling}"})
    findings = _judge(path, declared, undeclared=importing, uncached=live_list)
    expect(uncached_codes(findings) == [], f"a declared sibling must be silent, got {uncached_codes(findings)}")
    print(f"C-019 import GREEN: the imported sibling, declared on {victim}, needs no listing")
    checks += 2

    # An import-only reader loses its per-module group. `test_state_providers.py`
    # names no `recordings/` path; it imports `recordings.setups`, which is
    # declared on its target through `:recording_sources` and on no shared
    # group. Before helper sources counted as reads this mutation stayed green.
    reader, group_label = ACCEPTANCE_PACKAGE + "test_state_providers", "//test:recording_sources"
    expect(group_label in _landed(path, reader).inputs, f"{reader} no longer declares {group_label}")
    findings = _judge(path, _mutate(live, reader, drop_inputs={group_label}), undeclared=live_named, uncached=live_list)
    expect(group_label not in _landed(path, reader).inputs, "the recording_sources drop did not land")
    stale = _only(findings, "tag-uncached-unlisted")
    expect(reader in stale.message and "test/recordings/setups.py" in stale.message, "the import read is not named")
    print(f"C-019 import RED: {stale.message}")
    checks += 1

    # Reader floor: a module the source reader never saw is not a clean module.
    short = {name: paths for name, paths in live_named.items() if name != victim_module}
    findings = _judge(path, live, undeclared=short, uncached=live_list)
    unread = _only(findings, "tag-uncached-unread")
    expect(victim in unread.message, "the unread finding does not name the label")
    print(f"C-019 floor RED: {unread.message}")
    checks += 1

    # A listed name no acceptance target carries.
    findings = _judge(path, live, undeclared=live_named, uncached=live_list | {"test_absent.py"})
    unknown = _only(findings, "tag-uncached-unknown")
    expect("test_absent.py" in unknown.message, "the unknown finding does not name the entry")
    print(f"C-019 list RED : {unknown.message}")
    checks += 1
    return checks


def prove_slots(scratch: Path, live: list[dict]) -> int:
    """The xdist-group slot locks, derived from source and read off `env`."""
    checks = 0
    path = scratch / "mode9.json"
    sources = read_acceptance_sources()

    def slot_codes(findings: list[Finding]) -> list[str]:
        return [c for c in codes(findings) if c.startswith("tag-slot")]

    green = _judge(path, live, sources=sources)
    expect(slot_codes(green) == [], f"the live tree must be silent, got {slot_codes(green)}")
    print(f"slots GREEN: every target declares exactly the xdist groups its module names ({KNOWN_GROUP} among them)")
    checks += 1

    # A member loses its lock: the case the whole clause exists for.
    member = ACCEPTANCE_PACKAGE + "test_patches"
    expect(KNOWN_GROUP in xdist_groups(sources["test_patches.py"])[0], "test_patches is no member")
    findings = _judge(path, _mutate(live, member, env={}), sources=sources)
    expect(SLOTS_ENV not in dict(_landed(path, member).env), "the env drop did not land")
    dropped = _only(findings, "tag-slot-undeclared")
    expect(member in dropped.message and KNOWN_GROUP in dropped.message, "names neither")
    print(f"slots RED  : {dropped.message}")
    checks += 1

    # A lock nobody needs: it would serialise an unrelated module behind the group.
    bystander = ACCEPTANCE_PACKAGE + "test_install"
    findings = _judge(path, _mutate(live, bystander, env={SLOTS_ENV: KNOWN_GROUP}), sources=sources)
    expect(dict(_landed(path, bystander).env).get(SLOTS_ENV) == KNOWN_GROUP, "the add did not land")
    extra = _only(findings, "tag-slot-undeclared")
    expect(bystander in extra.message, "the finding does not name the bystander")
    print(f"slots RED  : {extra.message}")
    checks += 1

    # A group spelled through a name the derivation cannot resolve.
    planted = dict(sources)
    planted["test_install.py"] += '\nGROUP = "patch_global_slot"\npytestmark = pytest.mark.xdist_group(GROUP)\n'
    findings = _judge(path, live, sources=planted)
    unresolved = _only(findings, "tag-slot-unresolved")
    expect("xdist_group(GROUP)" in unresolved.message, "the unresolved spelling is not named")
    print(f"slots RED  : {unresolved.message}")
    checks += 1

    # A group only ONE module names still needs its lock: two runs of that
    # module from sibling checkouts are two processes on one stack.
    solo = dict(sources)
    solo["test_install.py"] += '\npytestmark = pytest.mark.xdist_group("solo_slot")\n'
    findings = _judge(path, live, sources=solo)
    undeclared = _only(findings, "tag-slot-undeclared")
    expect(bystander in undeclared.message and "solo_slot" in undeclared.message, "the solo group is not named")
    print(f"slots RED  : {undeclared.message}")
    findings = _judge(path, _mutate(live, bystander, env={SLOTS_ENV: "solo_slot"}), sources=solo)
    expect(dict(_landed(path, bystander).env).get(SLOTS_ENV) == "solo_slot", "the solo env did not land")
    expect(slot_codes(findings) == [], f"a declared solo group must green, got {slot_codes(findings)}")
    print("slots GREEN: the same one-module group, declared, is silent")
    checks += 2

    # A group spelled where the derivation does not read: helpers and
    # conftests (`read_helper_sources`), and a non-call spelling in a module.
    _dump(path, live)
    records, _ = read_rules(path.read_text(encoding="utf-8"))
    acceptance = {r.label for r in records if r.label.startswith(ACCEPTANCE_PACKAGE) and is_test_rule(r.rule_class)}
    env_of = {r.label: dict(r.env) for r in records}
    helpers = read_helper_sources()
    expect(len(helpers) > 10 and "test/tests/conftest.py" in helpers, f"the helper reader read {len(helpers)} file(s)")
    live_slots = slot_findings(acceptance=acceptance, env_of=env_of, sources=sources, helpers=helpers)
    expect(live_slots == [], f"the live helpers must be silent, got {codes(live_slots)}")
    print(f"slots GREEN: {len(helpers)} helper/conftest file(s) read, none spells xdist_group")
    checks += 1
    for case, planted_text in {
        "a conftest hook": 'def pytest_collection_modifyitems(items):\n    for i in items:\n        i.add_marker(pytest.mark.xdist_group("patch_global_slot"))\n',
        "an alias": "SLOT = pytest.mark.xdist_group\n",
        "a string mark name": 'def hook(item):\n    item.add_marker("xdist_group")\n',
    }.items():
        planted = dict(helpers)
        planted["test/tests/conftest.py"] += "\n" + planted_text
        foreign = _only(
            slot_findings(acceptance=acceptance, env_of=env_of, sources=sources, helpers=planted), "tag-slot-foreign"
        )
        expect("test/tests/conftest.py" in foreign.message, f"{case}: the helper is not named")
        print(f"slots RED  : {case}: {foreign.message}")
        checks += 1
    alias = dict(sources)
    alias["test_install.py"] += "\nG = pytest.mark.xdist_group\npytestmark = G(\"patch_global_slot\")\n"
    stray = _only(_judge(path, live, sources=alias), "tag-slot-unresolved")
    expect("pytest.mark.xdist_group" in stray.message, "the module alias is not named")
    print(f"slots RED  : {stray.message}")
    checks += 1

    # Pure: dict values resolve; both members of a shared group red; the reader floor.
    expect(xdist_groups('D = {"a": "g1", "b": "g2"}\nm = pytest.mark.xdist_group(D[k])\n') == ({"g1", "g2"}, []), "dict values")
    alone = slot_findings(acceptance={"//test:test_a"}, env_of={}, sources={"test_a.py": 'pytest.mark.xdist_group("solo")\n'}, helpers={})
    expect(codes(alone) == ["tag-slot-reader", "tag-slot-undeclared"], f"got {codes(alone)}")
    pair_sources = {name: f'pytest.mark.xdist_group("{KNOWN_GROUP}")\n' for name in ("test_a.py", "test_b.py")}
    pair = slot_findings(acceptance={"//test:test_a", "//test:test_b"}, env_of={}, sources=pair_sources, helpers={})
    expect(
        [f.code for f in pair] == ["tag-slot-undeclared"] * 2 and all("patch_global_slot" in f.message for f in pair),
        f"both members of a shared group must red, got {[f.code for f in pair]}",
    )
    print("slots GREEN/RED: a one-module group needs its lock too, a shared one needs it on both members, and the reader floor reds a derivation that lost the known group")
    checks += 3
    return checks


def prove_hand_reads(scratch: Path, live: list[dict]) -> int:
    """`HAND_DECLARED_READS` / `IMPLIED_READS`: the reads no AST derives."""
    checks = 0
    path = scratch / "mode10.json"
    green = [c for c in codes(_judge(path, live)) if c.startswith("tag-hand-read")]
    expect(green == [], f"the live tree must be silent, got {green}")
    print(f"hand reads GREEN: {len(HAND_DECLARED_READS)} module entr(ies) and {len(IMPLIED_READS)} implied read(s) all declared")
    checks += 1
    for label, dropped in (
        (ACCEPTANCE_PACKAGE + "test_doc_scripts_publish", "//test:suite_scripts"),
        (ACCEPTANCE_PACKAGE + "test_cosign_interop", "//:ocx.lock"),
    ):
        findings = _judge(path, _mutate(live, label, drop_inputs={dropped}))
        expect(dropped not in _landed(path, label).inputs, f"dropping {dropped} did not land")
        red = _only(findings, "tag-hand-read-undeclared")
        expect(label in red.message and dropped in red.message, "the finding names neither")
        print(f"hand reads RED  : {red.message}")
        checks += 1
    unknown = hand_read_findings(acceptance={ACCEPTANCE_PACKAGE + "test_install"}, closure={}, undeclared={}, texts={})
    expect(
        [f.code for f in unknown] == ["tag-hand-read-unknown"] * len(HAND_DECLARED_READS),
        f"entries for absent modules must red, got {[f.code for f in unknown]}",
    )
    print(f"hand reads RED  : {unknown[0].message}")
    checks += 1

    # A NEW invisible read, which no entry names yet: a `task` launch planted in
    # a module, then the same module once reviewed into the map.
    victim_module, victim = "test_install.py", ACCEPTANCE_PACKAGE + "test_install"
    expect(victim_module not in HAND_DECLARED_READS, f"{victim_module} already has an entry, pick another")
    planted = read_acceptance_sources()
    planted[victim_module] += '\nsubprocess.run(["task", "website:build"], check=True)\n'
    red = _only(_judge(path, live, sources=planted), "tag-hand-read-unreviewed")
    expect(victim in red.message and "task" in red.message, "the unreviewed finding names neither")
    print(f"hand reads RED  : {red.message}")
    HAND_DECLARED_READS[victim_module] = frozenset()
    try:
        reviewed = [c for c in codes(_judge(path, live, sources=planted)) if c.startswith("tag-hand-read")]
    finally:
        del HAND_DECLARED_READS[victim_module]
    expect(reviewed == [], f"the reviewed module must be silent, got {reviewed}")
    print(f"hand reads GREEN: the same launch, with a `HAND_DECLARED_READS` entry for {victim_module}")
    checks += 2

    # Through a helper the module imports, and a moved-out directory by string;
    # a docstring naming the same path is prose and stays silent.
    closure = input_closure(read_rules(path.read_text(encoding="utf-8"))[0])
    helper = "test/src/planted.py"
    for text, expected in (
        ('X = "scenarios/exec/diamond-dep-env.sh"\n', ["tag-hand-read-unreviewed"]),
        ('"""Reads scenarios/exec/diamond-dep-env.sh."""\n', []),
    ):
        found = hand_read_findings(
            acceptance={victim}, closure=closure, undeclared={victim_module: [helper]}, texts={helper: text}
        )
        unreviewed = [c for c in codes(found) if c == "tag-hand-read-unreviewed"]
        expect(unreviewed == expected, f"{text.strip()!r}: expected {expected}, got {codes(found)}")
        print(f"hand reads {'RED  ' if expected else 'GREEN'}: helper {text.strip()!r} -> {unreviewed}")
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
    # Dropped from the `rust_test` records, which no target takes as an input:
    # truncating the tail dropped whatever sorts last, and once that was a
    # filegroup an acceptance target declares (C-020's `//website:taskfiles`)
    # the case also redded `tag-uncached-unlisted` — a second finding for a
    # reason this case is not about.
    dropped = [r for r in live if r["rule"]["ruleClass"] == "rust_test"][-drop:]
    short = [r for r in live if r not in dropped]
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
        f"stage-4's floor must exceed stage-3's by more than the {ACCEPTANCE_MODULE_TARGETS} modules, or a run that read "
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


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
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

    findings, queried, census = run_check(
        stage=args.stage, bazel=args.bazel, universe=args.universe, query_json=args.query_json
    )
    # The runner's host locks (`test/bazel.bzl` § Concurrency) are text the
    # graph does not carry, so they are judged here, by running the generated
    # script against logging fakes; `scripts/tests/test_bazel_accept_proofs.py` shows
    # each lock's removal red.
    findings.extend(runner_lock_findings())
    # AM-9's other refusal: `--local_test_jobs` in an rc file would serialise
    # every test target, the Rust ones included. Text the graph does not carry.
    findings.extend(rc_serialisation_findings())
    if queried is not None:
        # Said out loud on every run: a green is only as wide as what ran, and an
        # unstated scope is read as the name's. The two clause subject counts are
        # on the line too, because those are the numbers that can be zero while
        # the reader count looks healthy.
        print(f"bazel tag guard: stage {args.stage} over {queried} — {census.line()}")
    return report(findings)


if __name__ == "__main__":
    raise SystemExit(main())
