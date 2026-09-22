#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`bazel:tag:guard` — C-011, the compensating control for the plan's one BZL-CORE-01 deviation.

    scripts/bazel_tag_guard.py                       # the gate: stage-3, //crates/... + //test/doc_scripts/...
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

**The floor is stage-scoped and advances — and this is the default stage.**
`STAGE_FLOORS` (WP-13) declares stage 1 (`//crates/...` >= 56) and stage 3
(`//crates/... + //test/doc_scripts/...` >= 143, WP-33's 45 and WP-33b's 42
added to WP-13's 56 — the 42 GIF renders took that package from 45 targets to
87, and until the floor moved with them it cleared by 42 and discriminated
nothing). `--stage` defaults to stage 3, which is the single place the adoption's
stage advances: `bazel:tag:guard` passes no `--stage` on purpose, so a second
spelling cannot go stale. Stage 4 (`//...` >= 272, wave 11) stays **undeclared**
— WP-36 has not published the acceptance count and a guessed floor is worse than
its absence — and an undeclared stage is a **hard red** (`tag-stage-unknown`)
refused before any query runs, so a floorless run is not reachable.

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
import dataclasses
import json
import subprocess
import tempfile
from pathlib import Path

from bazel_accept_proofs import CACHE_DEFEATING_TAG, INSUFFICIENT_TAGS
from bazel_gate_proofs import (
    CAST_GENRULE_TARGETS,
    CAST_RULE_TARGETS,
    CRATES_RULE_TARGETS,
    DRIFT_TARGET_FLOOR,
    GIF_RULE_TARGETS,
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
                stamp_zero=stamp_zero,
            )
        )
    return records, findings


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


def check_tags(
    *, stage: str, records: list[RuleRecord], queried_universe: str | None
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

    def line(self) -> str:
        return (
            f"{self.rule_targets} rule targets read ({self.rust_targets} Rust) — "
            f"{self.no_sandbox} no-sandbox, {self.rust_binaries} rust_binary, "
            f"so that is what the two clauses judged"
        )


def census_of(records: list[RuleRecord]) -> Census:
    return Census(
        rule_targets=len(records),
        rust_targets=len([r for r in records if r.rule_class.startswith(RUST_RULE_PREFIX)]),
        no_sandbox=len([r for r in records if NO_SANDBOX_TAG in r.tags]),
        rust_binaries=len([r for r in records if r.rule_class == "rust_binary"]),
    )


def run_check(
    *,
    stage: str,
    bazel: str,
    universe: str | None = None,
    query_json: Path | None = None,
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
    findings.extend(check_tags(stage=stage, records=records, queried_universe=queried))
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
    expect(hits == 1, f"{label} appears {hits} times in the capture, expected exactly 1")
    return records


def _judge(path: Path, records: list[dict], *, stage: str = "stage-3") -> list[Finding]:
    """Write a mutated stream, re-read it from disk, and judge it through `run_check`.

    Through the shipped entry point, never through the imported comparator: the
    reader is half of what this file owns, and a proof that skips it proves the
    other WP's function.
    """
    _dump(path, records)
    findings, _, _ = run_check(stage=stage, bazel="", query_json=path)
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
    **stage-3** universe, on this tree, right now.

    Stage 3 and not stage 1, because `//crates/...` contains no `no-sandbox`
    target and no `rust_binary` — both clauses were vacuous on it, and the 40
    targets that do carry `no-sandbox` all live in `//test/doc_scripts/...`. A
    self-test whose capture cannot contain its subject can only ever judge
    synthetic bytes."""
    universe = STAGE_FLOORS["stage-3"].universe
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
    print(
        f"S-012 mode 1 — the live graph carries {len(tagged)} tagged target(s) of {len(live)}, "
        f"{len(no_sandbox)} of them no-sandbox, and every one of those is a BUILD action. So "
        "this proof mutates a real `rust_test` to reach the TEST half of clause 1; "
        "`prove_build_action` below judges the live build actions themselves"
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


def prove_floor(scratch: Path, live: list[dict], bazel: str) -> int:
    """Mode 3 — the stage-scoped reader floor, and the undeclared stage it refuses."""
    checks = 0
    path = scratch / "mode3.json"
    floor = STAGE_FLOORS["stage-3"]
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

    # The floor this fix replaced. Reading only the Rust subset is what a
    # stage-1-shaped floor greened over, and it is also every state where the 40
    # no-sandbox targets went unread — so it has to be the loud one.
    crates_only = [r for r in live if not r["rule"]["name"].startswith("//test/doc_scripts")]
    findings = _judge(path, crates_only)
    reread, _ = read_rules(path.read_text(encoding="utf-8"))
    expect(
        len(reread) == len(crates_only) and not any(
            r.label.startswith("//test/doc_scripts") for r in reread
        ),
        "the doc_scripts truncation did not land",
    )
    expect(
        codes(findings) == ["tag-reader-floor"],
        f"a stage-3 run that read no //test/doc_scripts target must red, got {codes(findings)}",
    )
    print(f"S-012 mode 3 RED  : {_only(findings, 'tag-reader-floor').message}")
    checks += 1

    # The strongest red available: the real tool, a real narrower universe, no
    # byte mutation at all. A zero-match query exits 0, so the count is what
    # gates — and the override finding is what keeps a narrowed run from ever
    # being reported under the stage's name.
    narrow = "//crates/ocx_exit/..."
    findings, queried, census = run_check(stage="stage-3", bazel=bazel, universe=narrow)
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

    findings, queried, census = run_check(stage="stage-4", bazel=bazel)
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
    expect(
        set(STAGE_FLOORS) == {"stage-1", "stage-3"},
        f"STAGE_FLOORS declares {sorted(STAGE_FLOORS)} — C-011 names stage-4 (`//...` >= 272) "
        "at wave 11 and nobody has published WP-36's count, so it stays undeclared, which is a "
        "hard red rather than a guessed number",
    )
    print(
        f"S-012 counts: stage-1 floor {stage1.minimum} over {stage1.universe}, stage-3 floor "
        f"{stage3.minimum} over {stage3.universe}, both imported from WP-13; stage-4 stays "
        "undeclared and an undeclared stage is refused"
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
        checks += prove_floor(work, live, bazel)
    print(
        f"bazel tag guard self-test: {checks} checks passed — C-011's three clauses each shown "
        "red and green on the live graph's own query output: clause 1 in BOTH kinds (a test "
        "action without `external`, a build action without `no-remote-cache`, and each kind "
        "refusing the other's tag), clause 2 (a rust_binary with neither per-target escape), "
        "and the stage-3 reader floor, including the Rust-subset reading it replaced"
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
        default="stage-3",
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
