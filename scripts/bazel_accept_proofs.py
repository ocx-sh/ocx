#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Stage-4 Bazel edge cases — S-015 and S-004, the red states WP-36 and WP-39 owe.

    scripts/bazel_accept_proofs.py --self-test
    scripts/bazel_accept_proofs.py --prove-s015 [--probe-root DIR]
    scripts/bazel_accept_proofs.py --check-s015 --warm BEP --tags TAGS.json \
        [--built-binary FILE --binary-under-test FILE]
    scripts/bazel_accept_proofs.py --check-s004 --docs-bep BEP --crate-bep BEP \
        [--entry-taskfile taskfiles/bazel.taskfile.yml]

This file lands **before** WP-36, so `test/BUILD.bazel` and its 172 `sh_test`
targets do not exist and `bazel test //test:all` is not a command anybody can
run. A harness that could only red after WP-36 landed would be a harness nobody
can red today, so — as WP-13 did for stage 1 — everything here is split:

* **Comparators** — pure functions over already-read inputs. These are the
  gates' actual logic and the thing red-proven in `--self-test`. WP-36 owns the
  `bazel query` that produces the tag table and the target sets, WP-39 owns the
  two CI entry points; both call in here for the verdict, so the check their
  implementation is written against is one already watched go red.
* **`--prove-s015`** — a live Bazel run with a subject of its own, built here
  out of four `sh_test` targets in a throwaway workspace. It is the only thing
  in this plan that *measures* rather than assumes the mechanism C-024 rests
  on, and it needs neither WP-36 nor the acceptance suite nor a registry.

**S-015, and why the untagged sibling is not optional.** Run `bazel test`
twice with no source change and read run 2: "no acceptance target reported
cached" is equally consistent with *caching is off for these targets* and with
*the second run never happened* — and with *caching is off everywhere*, which
would be a different (and much worse) finding. One untagged sibling in the same
invocation separates all three: it must report cached. Only then does the
acceptance targets' silence mean anything.

**The mechanism is measured, and one intuitive spelling of it is wrong.**
`--prove-s015` on the pinned binary (bazel 9.2.0, `rules_shell` 0.8.0, four
`sh_test` targets over one script, `--disk_cache` only):

    target            tags                             cold   warm    warm
                                                              (same   (fresh
                                                              server) server)
    sibling           -                                 ran   CACHED  CACHED
    acc_local_only    local, exclusive                  ran   CACHED  ran
    acc_nocache       local, exclusive, no-cache        ran   CACHED  ran
    acc_external      local, external, exclusive        ran   ran     ran

So **only `external` actually stops a test result being reused.** `no-cache`
— the spelling a reader reaches for first — leaves the local action cache hit
in place: on a developer running `bazel test //test:all` twice in a row, a
`no-cache`-tagged acceptance target reports `(cached)`. `local` is not it
either: it suppresses the *disk*-cache hit (Bazel treats `--disk_cache` as a
remote cache, and `local` implies `no-remote`) while leaving the action-cache
hit, which is the developer-machine case and the one that matters. The
comparator therefore credits `external` and refuses to credit the other three
(`accept-tag-insufficient`), and `_no_prefix_reader` is kept as a named control
showing the natural "any `no-*` tag will do" reader calling `acc_nocache` safe
on the same bytes.

**Serialisation is the tag, and that is measured too** (C-024). Same probe,
three 0.7 s tests tagged `exclusive` and three not, one invocation, no
`--local_test_jobs`: the `exclusive` trio's windows are disjoint (0 overlaps,
run after everything else) and the untagged trio's three windows overlap
pairwise. A global `--local_test_jobs=1` would buy the same serialisation and
also serialise the 34 Rust test targets, which is stage 2's whole win — so an
rc file naming that flag is its own finding.

**S-015's second half — the binary under test.** A green suite against last
week's `ocx` is silent, and this repository has shipped one. The proof is a
content digest on both sides: what the build produced, and what the suite
executed, resolved through symlinks. Path and mtime both answer "fine" on a
freshly-*copied* stale binary — `test/bin/ocx` is a `cp` of
`target/release/ocx` (`test/taskfile.yml:125`), so the stale file's mtime is
the copy's, not the build's — and `_mtime_reader` and `_existence_reader` are
kept as named controls answering exactly that on the same fixture the digest
comparator reds.

**S-004 — selection, through the entry point.** Two independent readings, and
they answer different questions. The *selection* reading is the discriminator:
a docs-only change must select zero acceptance targets while a crate-touching
change selects some, or selection is unwired. The *BEP* reading is what makes
it observable rather than asserted — and it only works because of S-015: since
acceptance targets never cache, an acceptance target appearing in the BEP of a
docs-only build **ran**, so "ran" and "was selected" are the same event. The
Rust half is the opposite shape and must not be read the same way: Rust tests
*do* cache, so a docs-only build re-running zero of them is true whether or not
selection works, and it is floored, never used as the discriminator.

`SCOPED_ROWS` (`test/taskfile.yml`, 20 rows, two of them `escalate`) is read
unchanged as the crate→target selection query; `escalate` maps to `//test:all`.
It is a YAML mapping and this file is stdlib-only (plan DEC-8), so it is read
the way `scoped_gate.py` reads the same table — a regex over the block — and
the two readers' `escalate` sets are cross-checked against each other.

**C-029.** No red or green here reaches the owner-gated remote realm. Asserted
twice rather than promised: every Bazel argv this file builds is passed through
`realm_findings` before it is spawned, the probe workspace ships its own
`.bazelrc` with no remote flag and is launched with `--nosystem_rc --nohome_rc`
so an ambient one cannot be inherited, and `env_reads` AST-checks that this
file reads no environment variable at all. The single exception is `HOME`, via
`Path.expanduser`, to place the probe workspace outside the repository — never
a credential, and `BAZEL_CACHE_READ_AUTH`/`BAZEL_CACHE_WRITE_AUTH` are
unreachable from here by construction.

Every mutation in `--self-test` is proven to have landed before its result is
trusted; until then a surviving green is *unexplained*, not excused. Fixtures
are synthetic, live under this repository's own `.tmp/` (never `/tmp`, which is
a reaped tmpfs on the development host), and nothing is ever restored with
`git checkout --`, which restores from the index and would make the run vacuous.

Not wired into `taskfiles/scripts.taskfile.yml` — WP-17 owns that file. The one
line it owes the `self-test:` list is named in `--self-test`'s closing output.
"""

from __future__ import annotations

import argparse
import ast
import dataclasses
import fnmatch
import hashlib
import json
import re
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

from bazel_floor_proofs import task_cmds
from bazel_gate_proofs import (
    REPO_ROOT,
    Finding,
    bep_line,
    codes,
    read_test_results,
    report,
    rerun_set,
)

# ---------------------------------------------------------------------------
# Counts. Sums and live cross-checks, never bare literals, for WP-13's reason:
# a later correction to one summand must not leave a total at its old value.
# `prove_counts` reads the live tree for both of these, so a constant that
# drifts from `test/tests/` or from `test/taskfile.yml` reds here rather than
# quietly widening or narrowing every floor below.
# ---------------------------------------------------------------------------

ACCEPTANCE_MODULES = 181
"""`test/tests/test_*.py` — C-024's target count, one `sh_test` per module."""

SCOPED_ROWS = 20
"""Rows of `test/taskfile.yml`'s `SCOPED_ROWS` map — one per workspace member."""

SCOPED_ESCALATE_ROWS = 2
"""`ocx_test_support` and `ocx`: rows whose subset is the whole suite."""

ACCEPTANCE_PACKAGE = "//test"
ALL_TARGET = f"{ACCEPTANCE_PACKAGE}:all"

# ---------------------------------------------------------------------------
# Tag vocabulary. Measured on the pin by `--prove-s015`, not read off the docs
# — the docs describe `no-cache` in terms that would have been credited here.
# ---------------------------------------------------------------------------

CACHE_DEFEATING_TAG = "external"
"""The only tag measured to stop a test result being reused (both caches)."""

SERIALISING_TAG = "exclusive"
"""C-024's serialisation, measured disjoint without `--local_test_jobs`."""

INSUFFICIENT_TAGS = frozenset({"no-cache", "no-remote-cache", "no-remote", "local"})
"""Tags that read as "this will not be cached" and are not enough.

Measured: `no-cache` and `local` both reported `(cached)` on a second run in
the same output base. `local` additionally suppresses the disk-cache hit, which
makes it look sufficient on a fresh server and hides the developer-machine case.
"""

GLOBAL_SERIALISATION = re.compile(r"--?local_test_jobs\b")
"""C-024's forbidden mechanism. An rc line is per-command, never per-target
pattern, so this would also serialise the 34 Rust test targets."""

# ---------------------------------------------------------------------------
# C-029 — the realm this corpus must not reach.
# ---------------------------------------------------------------------------

REALM_FLAGS = re.compile(
    r"--(remote_cache|remote_header|remote_executor|remote_instance_name"
    r"|bes_backend|bes_results_url|credential_helper|google_credentials)\b"
)
REALM_ENV = re.compile(r"BAZEL_CACHE_[A-Z_]+")

# ---------------------------------------------------------------------------
# Messages. `code` is what the self-test asserts on; the message is stderr.
# Asserting a substring of an English sentence is one of the cheapest ways to
# write an assertion that also matches the opposite outcome (WP-13).
# ---------------------------------------------------------------------------

ACCEPT_CACHED_MSG = (
    "S-015 BLOCK: {count} acceptance target(s) reported cached on a re-run, first {sample} — "
    "nothing in the action graph tracks the compose stack the suite talks to, so a cached "
    "pass is a pass against a world that may have changed"
)
ACCEPT_CONTROL_ABSENT_MSG = (
    "S-015: no untagged control target was named. Without one, 'no acceptance target "
    "reported cached' is equally consistent with caching being off and with the second run "
    "never having happened — the control is what separates them, so a run without it is refused"
)
ACCEPT_CONTROL_COLD_MSG = (
    "S-015: the untagged control {sample} did not report cached either. Caching is off for "
    "the whole invocation, or run 2 re-ran everything — either way the acceptance targets' "
    "silence is not evidence about their tags"
)
ACCEPT_READER_MSG = (
    "S-015: the BEP carries no result for {count} of the target(s) being judged, first "
    "{sample} — the reader stopped early, and a target it never saw cannot report cached"
)
ACCEPT_UNIVERSE_MSG = (
    "S-015: {read} acceptance target(s) named, expected >= {minimum} — the query read a "
    "subset of test/tests/, so every verdict below is about that subset"
)
ACCEPT_TAG_MSG = (
    "S-015: {label} carries no `{tag}` tag — measured on bazel 9.2.0, it is the only tag "
    "that stops a test result being reused from either cache"
)
ACCEPT_TAG_WEAK_MSG = (
    "S-015: {label} carries {present} but not `{tag}`. Measured on the pin: a target tagged "
    "no-cache/local reports (cached) on a second run in the same output base — `local` only "
    "suppresses the disk-cache hit, which hides exactly the developer-machine case"
)
ACCEPT_SERIAL_MSG = (
    "S-015/C-024: {label} carries no `{tag}` tag — one compose stack, so two acceptance "
    "targets running at once share it"
)
SERIAL_GLOBAL_MSG = (
    "C-024: {source} names {flag!r} — serialisation is the `{tag}` tag on the targets. An rc "
    "line is per-command and never per-target-pattern, so this also serialises the Rust test "
    "targets and deletes stage 2's entire win"
)

BINARY_UNREADABLE_MSG = "S-015: no digest could be read for the {side} binary ({source}): {reason}"
BINARY_EMPTY_MSG = (
    "S-015: the {side} binary ({source}) is zero bytes — two empty files agree, so that "
    "agreement is not evidence that the suite ran the binary this build produced"
)
BINARY_MALFORMED_MSG = "S-015: the {side} digest is not a sha256 hex string: {digest!r} ({source})"
BINARY_STALE_MSG = (
    "S-015 BLOCK: the binary under test is not the one this build produced — built "
    "{built_digest} ({built_source}, {built_len} bytes), under test {test_digest} "
    "({test_source}, {test_len} bytes). A green suite against a stale binary is silent"
)
BINARY_UNDECLARED_MSG = (
    "S-015: {label} is not a declared input of the acceptance targets — the digests agree "
    "today and nothing re-runs the suite when they stop agreeing"
)

SELECT_ROW_MSG = (
    "S-004: changed crate {crate!r} has no SCOPED_ROWS row — the selection query cannot "
    "answer for it, which is not the same as it selecting nothing"
)
SELECT_GLOB_MSG = (
    "S-004: SCOPED_ROWS row {crate!r} glob {glob!r} matches no test/tests/ module — a stale "
    "glob narrows every selection taken from this table and nothing else reds"
)
SELECT_ROWS_FLOOR_MSG = (
    "S-004: read {read} SCOPED_ROWS row(s) / {escalate} escalate row(s), expected {rows} / "
    "{expected_escalate} — the table reader stopped early, and a short table selects less"
)
SELECT_MODULES_FLOOR_MSG = (
    "S-004: read {read} acceptance module(s), expected >= {minimum} — the module reader "
    "stopped early, so every glob below was matched against a subset of the suite"
)
SELECT_ESCALATE_PARITY_MSG = (
    "S-004: this reader sees escalate rows {mine}, scoped_gate.TABLE_ESCALATES has {theirs} "
    "— two readers of one table disagree, so at least one of them is wrong"
)
SELECT_DOCS_MSG = (
    "S-004: a docs-only change selected {count} target(s), first {sample}"
    "{same} — selection is unwired"
)
SELECT_SAME_CLAUSE = " — the same selection a crate-touching change produced"
SELECT_CONTROL_MSG = (
    "S-004: the crate-touching control selected nothing. A selector that selects nothing "
    "ever also selects nothing for a docs-only change, so the green above is vacuous"
)
SELECT_DOCS_RERAN_MSG = (
    "S-004: {count} acceptance target(s) ran on the docs-only build, first {sample}. "
    "Acceptance targets never cache (S-015), so an acceptance target in this BEP ran, and "
    "a target that ran was selected"
)
SELECT_RUST_RERAN_MSG = (
    "S-004: {count} Rust test target(s) re-ran on the docs-only build, first {sample}"
)
SELECT_BEP_FLOOR_MSG = (
    "S-004: the crate-touching control BEP carries {accept} acceptance and {rust} Rust test "
    "result(s), expected >= 1 of each — the reader never saw either universe, so the "
    "docs-only zeroes above are indistinguishable from a reader that read nothing"
)

ENTRY_READER_MSG = (
    "S-004/C-025: read no command item for the {role} entry point {name!r} — an entry point "
    "that could not be read cannot be judged, and an absent one is not a compliant one"
)
ENTRY_FULL_MSG = (
    "C-025: the full-suite entry point {name!r} runs nothing over the whole scope "
    "unconditionally — the release gate would skip targets the PR lane also skipped"
)
ENTRY_UNCONDITIONAL_MSG = (
    "S-004/C-025: the change-selected entry point {name!r} runs the whole scope "
    "unconditionally ({item!r}) — an unconditional `bazel test //...` defeats selection, "
    "which is why C-025 asks for two entry points rather than one"
)
ENTRY_SELECTION_MSG = (
    "S-004/C-025: the change-selected entry point {name!r} names no selection source "
    "(none of {tokens}) — an entry point that computes no target list is the full-suite "
    "entry under a second name"
)

REALM_FLAG_MSG = (
    "C-029: {source} carries {hit!r} — every red and green in this corpus runs on "
    "--disk_cache alone, because the remote realm is owner-gated and is not deployed"
)
REALM_ENV_MSG = "C-029: {source} reads {hit!r} — this corpus must not consult a cache credential"


@dataclasses.dataclass(frozen=True)
class BinaryReading:
    """One side of the binary-digest proof. `digest` is `None` when unreadable."""

    side: str
    source: str
    digest: str | None
    length: int | None
    reason: str = ""


@dataclasses.dataclass(frozen=True)
class EntryPoint:
    """One CI entry point's recipe, sliced into command items.

    `role` is `"full"` or `"selected"`; the two roles are judged by opposite
    legs of one predicate, which is what makes a recipe satisfying both
    impossible rather than merely discouraged.
    """

    name: str
    role: str
    items: list[str]


SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")

#: A go-task command item that `task_cmds` folded: a conditional or a loop
#: opens with its own key, so "does this item always run" is decidable from the
#: item's text. Measured against the live `taskfile.yml` pair — `verify:scoped`
#: has 17 of its 19 items guarded this way and `verify` has none.
GUARDED_ITEM = re.compile(r"^(if|for):")

#: What "the whole scope" spells as, per entry-point family. The Bazel set is
#: the one WP-39's pair is judged by; the task set exists so the predicate's
#: reader can be shown working on a file that exists today.
BAZEL_SCOPE_TOKENS = ("//...", "//test:all", "//test/...", "//crates/...")
SELECTION_TOKENS = ("scoped_gate", ".DECISION", "CRATES", "SCOPED_ROWS", "--targets-from")


# ---------------------------------------------------------------------------
# S-015, first half — caching is off for acceptance, and the control says so.
# ---------------------------------------------------------------------------


def caching_off_findings(
    *,
    warm: dict[str, bool],
    acceptance: set[str],
    control: set[str],
    tags: dict[str, set[str]],
    minimum: int = ACCEPTANCE_MODULES,
) -> list[Finding]:
    """S-015: run 2 of an unchanged tree. Empty list == the property holds.

    `warm` is `read_test_results`' `{label: cached}` over run 2 — the BEP, not
    the terminal summary line, and `cachedLocally` read for truth rather than
    presence, because proto3 omits a false boolean and the natural spelling
    (`payload.get("cachedLocally") is not False`) reports every target that ran
    as cached.

    Ordered so the control and the reader floor are settled before the verdict:
    a run where the reader stopped early would otherwise report "no acceptance
    target cached" and read as the property holding.
    """
    findings: list[Finding] = []

    if not control:
        findings.append(Finding("accept-control-absent", ACCEPT_CONTROL_ABSENT_MSG))

    if len(acceptance) < minimum:
        findings.append(
            Finding(
                "accept-universe-floor",
                ACCEPT_UNIVERSE_MSG.format(read=len(acceptance), minimum=minimum),
            )
        )

    unseen = sorted((acceptance | control) - set(warm))
    if unseen:
        findings.append(
            Finding(
                "accept-reader-floor",
                ACCEPT_READER_MSG.format(count=len(unseen), sample=unseen[0]),
            )
        )

    cached_control = sorted(label for label in control if warm.get(label))
    if control and not cached_control:
        findings.append(
            Finding(
                "accept-control-cold",
                ACCEPT_CONTROL_COLD_MSG.format(sample=min(control)),
            )
        )

    cached = sorted(label for label in acceptance if warm.get(label))
    if cached:
        findings.append(
            Finding(
                "accept-cached",
                ACCEPT_CACHED_MSG.format(count=len(cached), sample=cached[0]),
            )
        )

    for label in sorted(acceptance):
        carried = tags.get(label, set())
        if CACHE_DEFEATING_TAG not in carried:
            weak = sorted(carried & INSUFFICIENT_TAGS)
            if weak:
                findings.append(
                    Finding(
                        "accept-tag-insufficient",
                        ACCEPT_TAG_WEAK_MSG.format(
                            label=label, present=weak, tag=CACHE_DEFEATING_TAG
                        ),
                    )
                )
            else:
                findings.append(
                    Finding(
                        "accept-tag-missing",
                        ACCEPT_TAG_MSG.format(label=label, tag=CACHE_DEFEATING_TAG),
                    )
                )
        if SERIALISING_TAG not in carried:
            findings.append(
                Finding(
                    "accept-serial-tag-missing",
                    ACCEPT_SERIAL_MSG.format(label=label, tag=SERIALISING_TAG),
                )
            )
    return findings


def global_serialisation_findings(sources: dict[str, str]) -> list[Finding]:
    """C-024: no rc file may reach for `--local_test_jobs`.

    `sources` maps a name a human can act on (a path) to that file's text. A
    comment mentioning the flag counts: the check is over the file, and a
    commented-out flag one uncomment away from serialising 34 Rust targets is
    worth a line of stderr.
    """
    findings: list[Finding] = []
    for source, text in sorted(sources.items()):
        hit = GLOBAL_SERIALISATION.search(text)
        if hit is not None:
            findings.append(
                Finding(
                    "serialise-global",
                    SERIAL_GLOBAL_MSG.format(
                        source=source, flag=hit.group(0), tag=SERIALISING_TAG
                    ),
                )
            )
    return findings


# ---------------------------------------------------------------------------
# S-015, second half — the binary under test is the one this build produced.
# ---------------------------------------------------------------------------


def read_binary_file(path: Path, side: str) -> BinaryReading:
    """sha256 of `path`, symlinks resolved, streamed rather than slurped.

    `ocx` is ~59 MB and this runs beside a test suite; `hashlib.file_digest`
    reads it in blocks. An unreadable path yields `digest=None` and its reason,
    never a digest of nothing — two unreadable sides must produce two loud
    findings, not a silent agreement between two `None`s.
    """
    try:
        resolved = path.resolve(strict=True)
        with resolved.open("rb") as handle:
            digest = hashlib.file_digest(handle, "sha256").hexdigest()
        return BinaryReading(
            side=side, source=str(resolved), digest=digest, length=resolved.stat().st_size
        )
    except OSError as error:
        return BinaryReading(side=side, source=str(path), digest=None, length=None, reason=str(error))


def read_bep_output_digest(bep: Path, basename: str, side: str) -> BinaryReading:
    """The digest Bazel published for an output file named `basename`.

    Measured on bazel 9.2.0: `namedSetOfFiles.files[]` carries `name`, `uri`,
    `digest` (sha256 hex, unprefixed) and `length` (a JSON string, proto3 int64).
    Taken from the BEP rather than from `bazel-bin/`, because `bazel-bin` is a
    convenience symlink into an output base that a second invocation may have
    moved — the digest in the build's own event stream is what *this* build
    produced, which is the thing the proof is about.
    """
    try:
        text = bep.read_text(encoding="utf-8")
    except OSError as error:
        return BinaryReading(side=side, source=str(bep), digest=None, length=None, reason=str(error))
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        payload = event.get("namedSetOfFiles")
        if not isinstance(payload, dict):
            continue
        for entry in payload.get("files", []):
            if not isinstance(entry, dict) or entry.get("name") != basename:
                continue
            digest = entry.get("digest")
            raw_length = entry.get("length")
            try:
                length = int(raw_length)
            except (TypeError, ValueError):
                length = None
            return BinaryReading(
                side=side,
                source=f"{bep}#namedSetOfFiles/{basename}",
                digest=digest if isinstance(digest, str) else None,
                length=length,
                reason="the entry carries no digest" if not isinstance(digest, str) else "",
            )
    return BinaryReading(
        side=side,
        source=str(bep),
        digest=None,
        length=None,
        reason=f"no namedSetOfFiles entry named {basename!r}",
    )


def binary_digest_findings(
    built: BinaryReading,
    under_test: BinaryReading,
    *,
    declared_inputs: set[str] | None = None,
    binary_label: str = "",
) -> list[Finding]:
    """S-015's second half. Empty list == the suite ran this build's binary.

    Two independent reader floors, one per side, in `pin_drift`'s shape: absent,
    unreadable or zero-length each produce their own finding rather than letting
    two failures agree with each other.

    `declared_inputs` is optional and is the durability half: equal digests
    today say nothing about tomorrow unless the binary is a declared input of
    the test action, because nothing would re-run the suite when it changes.
    WP-36 owns the `cquery` that produces the set.
    """
    findings: list[Finding] = []

    for reading in (built, under_test):
        if reading.digest is None:
            findings.append(
                Finding(
                    f"binary-{reading.side}-unreadable",
                    BINARY_UNREADABLE_MSG.format(
                        side=reading.side,
                        source=reading.source,
                        reason=reading.reason or "no reason recorded",
                    ),
                )
            )
        elif not SHA256_HEX.match(reading.digest):
            findings.append(
                Finding(
                    "binary-digest-malformed",
                    BINARY_MALFORMED_MSG.format(
                        side=reading.side, digest=reading.digest, source=reading.source
                    ),
                )
            )
        elif not reading.length:
            findings.append(
                Finding(
                    "binary-empty",
                    BINARY_EMPTY_MSG.format(side=reading.side, source=reading.source),
                )
            )

    if not findings and built.digest != under_test.digest:
        findings.append(
            Finding(
                "binary-stale",
                BINARY_STALE_MSG.format(
                    built_digest=built.digest,
                    built_source=built.source,
                    built_len=built.length,
                    test_digest=under_test.digest,
                    test_source=under_test.source,
                    test_len=under_test.length,
                ),
            )
        )

    if declared_inputs is not None and binary_label and binary_label not in declared_inputs:
        findings.append(
            Finding("binary-undeclared", BINARY_UNDECLARED_MSG.format(label=binary_label))
        )
    return findings


# ---------------------------------------------------------------------------
# S-004 — selection. The SCOPED_ROWS reader, then the two verdicts.
# ---------------------------------------------------------------------------


def read_scoped_rows(taskfile_text: str) -> dict[str, str]:
    """`test/taskfile.yml`'s `SCOPED_ROWS` map, as `{crate: glob-string}`.

    Stdlib only (plan DEC-8), so the block is sliced by indentation and its rows
    matched the way `scoped_gate.py` matches the same table's `escalate` rows —
    one grammar, two readers, and `selection_table_findings` makes them agree.
    """
    rows: dict[str, str] = {}
    lines = taskfile_text.splitlines()
    inside = False
    header_indent = 0
    for line in lines:
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())
        if not inside:
            if stripped == "SCOPED_ROWS:":
                inside = True
                header_indent = indent
            continue
        if stripped and indent <= header_indent:
            break
        match = re.match(r"^([A-Za-z_][A-Za-z0-9_]*):\s+(\S.*?)\s*$", stripped)
        if match and stripped != "map:":
            rows[match.group(1)] = match.group(2)
    return rows


def acceptance_modules(test_dir: Path) -> set[str]:
    """`tests/test_*.py` relative to `test/` — the unit C-024 makes a target of."""
    return {f"tests/{path.name}" for path in sorted((test_dir / "tests").glob("test_*.py"))}


def module_target(module: str) -> str:
    """`tests/test_install.py` -> `//test:test_install`."""
    return f"{ACCEPTANCE_PACKAGE}:{Path(module).stem}"


def selection_table_findings(
    rows: dict[str, str],
    modules: set[str],
    *,
    peer_escalates: frozenset[str] | None = None,
) -> list[Finding]:
    """Floors on both readers, before any selection is taken from either.

    A short table and a short module list are each indistinguishable from a
    narrow selection in the answer alone, which is the whole shape S-004 is
    trying to catch, so both are floored here rather than inside the selector.
    """
    findings: list[Finding] = []
    escalates = {crate for crate, row in rows.items() if row == "escalate"}

    if len(rows) != SCOPED_ROWS or len(escalates) != SCOPED_ESCALATE_ROWS:
        findings.append(
            Finding(
                "select-rows-floor",
                SELECT_ROWS_FLOOR_MSG.format(
                    read=len(rows),
                    escalate=len(escalates),
                    rows=SCOPED_ROWS,
                    expected_escalate=SCOPED_ESCALATE_ROWS,
                ),
            )
        )
    if len(modules) < ACCEPTANCE_MODULES:
        findings.append(
            Finding(
                "select-modules-floor",
                SELECT_MODULES_FLOOR_MSG.format(read=len(modules), minimum=ACCEPTANCE_MODULES),
            )
        )
    if peer_escalates is not None and escalates != set(peer_escalates):
        findings.append(
            Finding(
                "select-escalate-parity",
                SELECT_ESCALATE_PARITY_MSG.format(
                    mine=sorted(escalates), theirs=sorted(peer_escalates)
                ),
            )
        )

    for crate, row in sorted(rows.items()):
        if row == "escalate":
            continue
        for glob in row.split():
            if not fnmatch.filter(sorted(modules), glob):
                findings.append(
                    Finding("select-glob-dead", SELECT_GLOB_MSG.format(crate=crate, glob=glob))
                )
    return findings


def select_targets(
    changed: list[str],
    *,
    rows: dict[str, str],
    modules: set[str],
    crate_of_dir: dict[str, str],
) -> tuple[set[str], list[Finding]]:
    """The crate→target selection query, unchanged from SCOPED_ROWS.

    A path that routes to no crate and to no acceptance module selects nothing:
    that is S-004's whole subject, and it is why a docs-only change has an empty
    answer rather than a small one. A changed acceptance module selects itself;
    one that no longer exists on disk selects `//test:all`, because a deletion
    can move coverage anywhere and `scoped_gate.py` escalates on the same input.

    **Both halves of a crate path's answer are here**: its Rust package
    (`//crates/<dir>:all`) and the acceptance targets its `SCOPED_ROWS` row
    names. `bazel:test:scoped` spelled the Rust half in its own heredoc until
    R-2 of the end-of-run review, which made that heredoc a third reader of the
    same mapping and — because it skipped the `crate_of_dir` membership check
    below — emitted a label for any `crates/<dir>/<file>` whether or not that
    directory was a crate. Both answers come off one guard now.
    """
    selected: set[str] = set()
    findings: list[Finding] = []

    for path in changed:
        parts = path.split("/")
        if parts[0] == "crates" and len(parts) > 2:
            crate = crate_of_dir.get(f"crates/{parts[1]}")
            if crate is None:
                continue
            # The Rust half, before the row lookup: a crate that changed has
            # Bazel targets to build whatever its acceptance row says, and an
            # `escalate` row is about acceptance coverage, not about this.
            selected.add(f"//crates/{parts[1]}:all")
            row = rows.get(crate)
            if row is None:
                findings.append(Finding("select-row-missing", SELECT_ROW_MSG.format(crate=crate)))
                selected.add(ALL_TARGET)
                continue
            if row == "escalate":
                selected.add(ALL_TARGET)
                continue
            for glob in row.split():
                selected.update(module_target(m) for m in fnmatch.filter(sorted(modules), glob))
        elif parts[:2] == ["test", "tests"] and len(parts) == 3:
            module = f"tests/{parts[2]}"
            selected.add(module_target(module) if module in modules else ALL_TARGET)
    return selected, findings


def selection_findings(
    *,
    docs_selected: set[str],
    crate_selected: set[str],
    docs_bep: dict[str, bool],
    crate_bep: dict[str, bool],
    acceptance_prefix: str = f"{ACCEPTANCE_PACKAGE}:",
    rust_prefix: str = "//crates/",
) -> list[Finding]:
    """S-004: the selection verdict and the BEP verdict, in that order.

    The floors come first and they sit on the **control** run, not on the
    docs-only one: a docs-only build legitimately carries zero test results, so
    flooring that BEP would red on the green case. What has to be floored is
    that the reader can see both universes at all, which the crate-touching run
    is what establishes.
    """
    findings: list[Finding] = []

    accept_control = [label for label in crate_bep if label.startswith(acceptance_prefix)]
    rust_control = [label for label in crate_bep if label.startswith(rust_prefix)]
    if not accept_control or not rust_control:
        findings.append(
            Finding(
                "select-bep-floor",
                SELECT_BEP_FLOOR_MSG.format(accept=len(accept_control), rust=len(rust_control)),
            )
        )

    if not crate_selected:
        findings.append(Finding("select-control-empty", SELECT_CONTROL_MSG))

    if docs_selected:
        same = SELECT_SAME_CLAUSE if docs_selected == crate_selected else ""
        findings.append(
            Finding(
                "select-docs-nonempty",
                SELECT_DOCS_MSG.format(
                    count=len(docs_selected), sample=min(docs_selected), same=same
                ),
            )
        )

    # Acceptance targets never cache (S-015), so presence in this BEP is
    # execution and execution is selection. Rust targets do cache, which is why
    # the reading below is a *re-run* set and is never used as the discriminator.
    docs_accept = sorted(label for label in docs_bep if label.startswith(acceptance_prefix))
    if docs_accept:
        findings.append(
            Finding(
                "select-docs-reran",
                SELECT_DOCS_RERAN_MSG.format(count=len(docs_accept), sample=docs_accept[0]),
            )
        )

    docs_rust = sorted(
        label for label in rerun_set(docs_bep) if label.startswith(rust_prefix)
    )
    if docs_rust:
        findings.append(
            Finding(
                "select-rust-reran",
                SELECT_RUST_RERAN_MSG.format(count=len(docs_rust), sample=docs_rust[0]),
            )
        )
    return findings


# ---------------------------------------------------------------------------
# S-004, through the entry point. C-025's two of them, one predicate.
# ---------------------------------------------------------------------------


def entry_findings(
    entry: EntryPoint,
    *,
    scope_tokens: tuple[str, ...] = BAZEL_SCOPE_TOKENS,
    selection_tokens: tuple[str, ...] = SELECTION_TOKENS,
) -> list[Finding]:
    """C-025: a full-suite entry and a change-selected one, judged oppositely.

    The two roles are the same predicate read from either end, so a single
    recipe cannot satisfy both — which is the point of C-025 asking for two
    entry points. A change-selected entry that runs the whole scope with no
    guard is the failure S-004 names by name, and one that mentions no
    selection source at all is the same failure wearing a second task name.
    """
    findings: list[Finding] = []
    if not entry.items:
        findings.append(
            Finding(
                "entry-reader-floor",
                ENTRY_READER_MSG.format(role=entry.role, name=entry.name),
            )
        )
        return findings

    unguarded_scope = [
        item
        for item in entry.items
        if not GUARDED_ITEM.match(item) and any(token in item for token in scope_tokens)
    ]

    if entry.role == "full":
        if not unguarded_scope:
            findings.append(Finding("entry-full-partial", ENTRY_FULL_MSG.format(name=entry.name)))
        return findings

    if unguarded_scope:
        findings.append(
            Finding(
                "entry-selection-unconditional",
                ENTRY_UNCONDITIONAL_MSG.format(name=entry.name, item=unguarded_scope[0][:90]),
            )
        )
    if not any(token in item for item in entry.items for token in selection_tokens):
        findings.append(
            Finding(
                "entry-selection-absent",
                ENTRY_SELECTION_MSG.format(name=entry.name, tokens=list(selection_tokens)),
            )
        )
    return findings


# ---------------------------------------------------------------------------
# C-029 — this corpus reaches no realm. Asserted twice, promised nowhere.
# ---------------------------------------------------------------------------


def realm_findings(sources: dict[str, str]) -> list[Finding]:
    """Any remote-cache flag or cache credential name in `sources`.

    `sources` maps a name to text a human can act on: a joined argv, an rc
    file's contents. Never this file — the banned spellings appear here as data
    and in the docstring as prose, so a text scan of this source would match
    itself in every state. The self-reach property has its own reader below.
    """
    findings: list[Finding] = []
    for source, text in sorted(sources.items()):
        flag = REALM_FLAGS.search(text)
        if flag is not None:
            findings.append(
                Finding("realm-flag", REALM_FLAG_MSG.format(source=source, hit=flag.group(0)))
            )
        env = REALM_ENV.search(text)
        if env is not None:
            findings.append(
                Finding("realm-env", REALM_ENV_MSG.format(source=source, hit=env.group(0)))
            )
    return findings


def env_reads(source: str) -> list[str]:
    """Every environment read in `source`, by AST rather than by grep.

    This file spawns Bazel, so WP-13's `ambient_reads` (which bans `subprocess`
    outright) is the wrong predicate here; the property that survives is the
    narrower one — no environment is consulted at all, so no cache credential
    can be picked up even by accident. `Path.expanduser` resolves `HOME` inside
    the standard library and is the one deliberate exception, named in the
    module docstring.
    """
    hits: list[str] = []
    for node in ast.walk(ast.parse(source)):
        if isinstance(node, ast.Attribute):
            value = node.value
            if isinstance(value, ast.Name) and value.id == "os" and node.attr in {"environ", "getenv"}:
                hits.append(f"os.{node.attr}")
        elif isinstance(node, ast.Import):
            hits.extend(
                f"import {alias.name}" for alias in node.names if alias.name.split(".")[0] == "os"
            )
    return hits


# ---------------------------------------------------------------------------
# The live probe. Four `sh_test` targets, three runs, one measured table.
# ---------------------------------------------------------------------------

PROBE_DEFAULT_ROOT = Path("~/.cache/ocx/wp35-s015-probe")
BAZEL_BIN = Path("~/.ocx/symlinks/ocx.sh/bazelbuild/bazel/candidates/9.2.0/content/bazel")

PROBE_TARGETS: dict[str, tuple[str, ...]] = {
    "sibling": (),
    "acc_local_only": ("local", "exclusive"),
    "acc_nocache": ("local", "exclusive", "no-cache"),
    "acc_external": ("local", "external", "exclusive"),
}

#: The measurement. `True` means "reported cached", per run. Shipped as an
#: expectation rather than a note so that a Bazel release changing any cell
#: reds here instead of silently invalidating C-024.
PROBE_EXPECTED: dict[str, tuple[bool, bool, bool]] = {
    #                cold   warm/same-server  warm/fresh-server
    "sibling": (False, True, True),
    "acc_local_only": (False, True, False),
    "acc_nocache": (False, True, False),
    "acc_external": (False, False, False),
}

SERIAL_TRIO = ("excl1", "excl2", "excl3")
PARALLEL_TRIO = ("para1", "para2", "para3")
SERIAL_SLEEP = "0.7"


def probe_build_file() -> str:
    lines = ['load("@rules_shell//shell:sh_test.bzl", "sh_test")', ""]
    for name, tags in PROBE_TARGETS.items():
        attr = f", tags = {list(tags)}" if tags else ""
        lines.append(f'sh_test(name = "{name}", srcs = ["quiet.sh"]{attr})')
    for name in SERIAL_TRIO:
        lines.append(
            f'sh_test(name = "{name}", srcs = ["timed.sh"], args = ["{name}"], '
            f'tags = ["local", "external", "exclusive"])'
        )
    for name in PARALLEL_TRIO:
        lines.append(
            f'sh_test(name = "{name}", srcs = ["timed.sh"], args = ["{name}"], '
            f'tags = ["local", "external"])'
        )
    return "\n".join(lines) + "\n"


def probe_rc(root: Path, disk_cache: Path, output_root: Path, repo_cache: Path) -> str:
    """The probe's whole rc. No remote flag of any kind reaches it (C-029).

    `--output_user_root` and `--repo_contents_cache` are placed outside the
    probe workspace because bazel 9 refuses a repo contents cache inside the
    main repository (DX-16), and off `/tmp` because that is a reaped tmpfs on
    the development host.
    """
    return (
        f"# Generated by scripts/bazel_accept_proofs.py --prove-s015 for {root.name}.\n"
        "startup --host_jvm_args=-Xmx2g\n"
        f"startup --output_user_root={output_root}\n"
        f"common --repo_contents_cache={repo_cache}\n"
        "build --jobs=12\n"
        f"build --disk_cache={disk_cache}\n"
    )


def materialise_probe(root: Path) -> tuple[Path, Path, Path]:
    """Write the throwaway workspace. Returns disk cache, output root, timeline."""
    if root.exists():
        shutil.rmtree(root)
    (root / "timeline").mkdir(parents=True)
    (root / "MODULE.bazel").write_text(
        'module(name = "wp35_s015_probe", version = "0.0.0")\n'
        'bazel_dep(name = "rules_shell", version = "0.8.0")\n',
        encoding="utf-8",
    )
    (root / "BUILD.bazel").write_text(probe_build_file(), encoding="utf-8")
    quiet = root / "quiet.sh"
    quiet.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    quiet.chmod(0o755)
    timed = root / "timed.sh"
    timed.write_text(
        "#!/bin/sh\n"
        f'log="{root / "timeline" / "log"}"\n'
        'printf "%s start %s\\n" "$1" "$(date +%s.%N)" >> "$log"\n'
        f"sleep {SERIAL_SLEEP}\n"
        'printf "%s end %s\\n" "$1" "$(date +%s.%N)" >> "$log"\n',
        encoding="utf-8",
    )
    timed.chmod(0o755)

    cache_home = root.parent
    disk_cache = cache_home / f"{root.name}-disk"
    output_root = cache_home / f"{root.name}-out"
    # Shared across probe roots on purpose, and it is the only one that is: the
    # disk cache and the output base are the subject, so they are wiped per
    # probe, while the repo contents cache only holds `rules_shell` and its
    # transitive BCR fetch. Keying it on the probe name too made a second
    # probe root re-download rules_license from GitHub, which is a network
    # flake standing between a run and its answer.
    repo_cache = cache_home / "wp35-probe-repo"
    (root / ".bazelrc").write_text(
        probe_rc(root, disk_cache, output_root, repo_cache), encoding="utf-8"
    )
    return disk_cache, output_root, root / "timeline"


def probe_argv(bazel: Path, command: str, *rest: str) -> list[str]:
    """`--nosystem_rc --nohome_rc`: only the probe's own rc may reach this run.

    Without them an ambient `~/.bazelrc` carrying a `--remote_cache` line would
    make the probe's cache claims about a realm rather than about `--disk_cache`,
    which is the exact coupling C-029 forbids.
    """
    return [str(bazel), "--nosystem_rc", "--nohome_rc", command, *rest]


def run_probe_command(root: Path, argv: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv, cwd=root, capture_output=True, text=True, encoding="utf-8", check=False, timeout=900
    )


def timeline_overlaps(
    log_text: str, names: tuple[str, ...]
) -> tuple[list[tuple[float, float, str]], int]:
    """Per-name (start, end, name) windows in start order, and overlapping pairs.

    The tuple leads with `start` because the sort key is the whole point: sorted
    by *name* — which is what a `(name, start, end)` tuple sorts by, and what the
    first draft of this function did — three disjoint windows run out of
    alphabetical order report a phantom overlap, and three overlapping ones
    can report none. Measured on the probe's own log: the name-ordered reader
    called the serialised trio 1-overlapping and the real reader calls it 0.
    """
    windows: dict[str, dict[str, float]] = {}
    for line in log_text.splitlines():
        parts = line.split()
        if len(parts) != 3 or parts[0] not in names or parts[1] not in {"start", "end"}:
            continue
        windows.setdefault(parts[0], {})[parts[1]] = float(parts[2])
    rows = sorted(
        (marks["start"], marks["end"], name)
        for name, marks in windows.items()
        if "start" in marks and "end" in marks
    )
    overlaps = sum(1 for i in range(len(rows) - 1) if rows[i][1] > rows[i + 1][0])
    return rows, overlaps


def run_prove_s015(root: Path) -> int:
    """Three live runs, the measured table, and the serialisation measurement."""
    bazel = BAZEL_BIN.expanduser()
    if not bazel.exists():
        print(
            f"--prove-s015 needs the pinned binary at {bazel} — it resolves from ocx.lock "
            "(`ocx pull`). Refusing to run against whatever `bazel` is on PATH.",
        )
        return 1

    root = root.expanduser()
    disk_cache, output_root, timeline = materialise_probe(root)
    rc_text = (root / ".bazelrc").read_text(encoding="utf-8")

    runs = ("cold", "warm-same-server", "warm-fresh-server")
    bep = {name: root / f"{name}.json" for name in runs}
    argvs = {
        name: probe_argv(bazel, "test", "//:all", f"--build_event_json_file={bep[name]}")
        for name in runs
    }

    realm = realm_findings({**{f"argv[{k}]": " ".join(v) for k, v in argvs.items()}, ".bazelrc": rc_text})
    if realm:
        sys_findings = report(realm)
        print("C-029: refusing to spawn a run that could reach the remote realm")
        return sys_findings

    print(f"--prove-s015: probe workspace {root}, disk cache {disk_cache}, no remote flag")

    # The cold run has to be genuinely cold, and wiping the *workspace* is not
    # enough: the action cache lives in the output base, which survives a
    # rebuilt workspace and made the first draft of this probe report every
    # target CACHED on run 1. Shut the server down before the directory it owns
    # is removed, then remove both caches — `bazel clean` would leave the disk
    # cache, which is the one thing run 3 is about.
    run_probe_command(root, probe_argv(bazel, "shutdown"))
    for stale in (disk_cache, output_root):
        if stale.exists():
            shutil.rmtree(stale)

    outcome = run_probe_command(root, argvs["cold"])
    if outcome.returncode != 0:
        print(outcome.stdout[-3000:])
        print(outcome.stderr[-3000:])
        return report([Finding("probe-cold-failed", "S-015 probe: the cold run did not pass")])
    serial_log = timeline.joinpath("log").read_text(encoding="utf-8")

    if run_probe_command(root, argvs["warm-same-server"]).returncode != 0:
        return report([Finding("probe-warm-failed", "S-015 probe: the warm run did not pass")])

    run_probe_command(root, probe_argv(bazel, "clean"))
    run_probe_command(root, probe_argv(bazel, "shutdown"))
    if run_probe_command(root, argvs["warm-fresh-server"]).returncode != 0:
        return report([Finding("probe-fresh-failed", "S-015 probe: the fresh-server run failed")])

    findings: list[Finding] = []
    observed: dict[str, list[bool]] = {name: [] for name in PROBE_TARGETS}
    for run in runs:
        results, parse = read_test_results(bep[run])
        findings.extend(parse)
        for name in PROBE_TARGETS:
            label = f"//:{name}"
            if label not in results:
                findings.append(
                    Finding(
                        "probe-reader-floor",
                        f"S-015 probe: the {run} BEP carries no result for {label}",
                    )
                )
                observed[name].append(False)
            else:
                observed[name].append(results[label])

    print(f"{'target':<16}{'tags':<34}{'cold':>7}{'warm/same':>11}{'warm/fresh':>12}")
    for name, tags in PROBE_TARGETS.items():
        cells = observed[name]
        print(
            f"{name:<16}{', '.join(tags) or '-':<34}"
            + "".join(
                f"{'CACHED' if cell else 'ran':>{width}}"
                for cell, width in zip(cells, (7, 11, 12), strict=True)
            )
        )
        expected = list(PROBE_EXPECTED[name])
        if cells != expected:
            findings.append(
                Finding(
                    "probe-table-drift",
                    f"S-015 probe: {name} answered {cells}, the measurement on bazel 9.2.0 is "
                    f"{expected} — the tag semantics C-024 rests on have moved",
                )
            )

    serial_rows, serial_overlaps = timeline_overlaps(serial_log, SERIAL_TRIO)
    parallel_rows, parallel_overlaps = timeline_overlaps(serial_log, PARALLEL_TRIO)
    if len(serial_rows) != len(SERIAL_TRIO) or len(parallel_rows) != len(PARALLEL_TRIO):
        findings.append(
            Finding(
                "probe-timeline-floor",
                f"S-015 probe: read {len(serial_rows)}/{len(parallel_rows)} timed windows, "
                f"expected {len(SERIAL_TRIO)}/{len(PARALLEL_TRIO)} — the timeline reader stopped early",
            )
        )
    if serial_overlaps:
        findings.append(
            Finding(
                "probe-not-serialised",
                f"C-024: {serial_overlaps} overlapping pair(s) among the `{SERIALISING_TAG}` "
                "trio — the tag did not serialise, so WP-36's 172 targets would share one "
                "compose stack",
            )
        )
    if not parallel_overlaps:
        findings.append(
            Finding(
                "probe-control-serial",
                "C-024: the untagged trio did not overlap either, so this run cannot tell "
                f"`{SERIALISING_TAG}` from a machine that never runs two tests at once",
            )
        )
    print(
        f"serialisation : `{SERIALISING_TAG}` trio {serial_overlaps} overlapping pair(s), "
        f"untagged control {parallel_overlaps} — no --local_test_jobs anywhere in the rc"
    )

    if findings:
        return report(findings)
    print(
        "S-015 MEASURED: only `external` stops a test result being reused (both caches); "
        "`no-cache` and `local` do not, and the untagged sibling reports cached in both warm "
        "runs, which is what makes the acceptance targets' silence evidence"
    )
    return 0


# ---------------------------------------------------------------------------
# Fixtures. Synthetic by construction — WP-36 has not run.
# ---------------------------------------------------------------------------


def fixture_modules(count: int = ACCEPTANCE_MODULES) -> set[str]:
    """`count` acceptance modules, named so no real test file is implied."""
    return {f"tests/test_mod{index:03d}.py" for index in range(count)}


def fixture_rows(modules: set[str]) -> dict[str, str]:
    """A `SCOPED_ROWS`-shaped table: 20 rows, two of them `escalate`.

    Globs are two-digit prefixes over the fixture's own three-digit module
    names, so each of the 18 non-`escalate` rows owns a live decade and every
    module is reachable from some row — a dead glob is then a mutation rather
    than this table's normal state, which is the only way `select-glob-dead`
    can be shown red on purpose.
    """
    rows: dict[str, str] = {"ocx_test_support": "escalate", "ocx": "escalate"}
    glob_rows = SCOPED_ROWS - SCOPED_ESCALATE_ROWS
    # Decades are derived from the modules present, not from the row count: the
    # fixture's size follows the live acceptance suite, so hard-coding one decade
    # per row leaves the tail uncovered the moment the suite outgrows
    # `glob_rows * 10` — and an uncovered module makes `select-glob-dead` this
    # table's resting state instead of a mutation. Surplus decades fold into the
    # last row, which keeps every module reachable at any count.
    decades = sorted({name[len("tests/test_mod") : -len("0.py")] for name in modules})
    for index in range(glob_rows):
        owned = (
            decades[index : index + 1] if index < glob_rows - 1 else decades[index:]
        )
        rows[f"crate{index:02d}"] = " ".join(f"tests/test_mod{d}*.py" for d in owned) or (
            f"tests/test_mod{index:02d}*.py"
        )
    covered = {
        module
        for row in rows.values()
        if row != "escalate"
        for glob in row.split()
        for module in fnmatch.filter(sorted(modules), glob)
    }
    if covered != modules:
        raise SystemExit(
            f"fixture_rows leaves {len(modules - covered)} module(s) unreachable — "
            "the dead-glob red below would then be the fixture's resting state"
        )
    return rows


def fixture_crate_of_dir() -> dict[str, str]:
    """Directory → package, including the one pair where they differ.

    `crates/ocx_cli` holds the package named `ocx`; every other member's
    directory is its package name. A selector keyed on the directory would
    miss exactly that row, which is the `escalate` one.
    """
    mapping = {f"crates/crate{index:02d}": f"crate{index:02d}" for index in range(18)}
    mapping["crates/ocx_cli"] = "ocx"
    mapping["crates/ocx_test_support"] = "ocx_test_support"
    return mapping


def write_accept_bep(path: Path, outcomes: dict[str, bool]) -> None:
    """A BEP over `outcomes`; a re-run target carries no `cachedLocally` key.

    That absence is what Bazel actually emits (proto3 omits a false boolean),
    measured on the pin in `--prove-s015`, and it is why `read_test_results`
    reads for truth rather than for presence.
    """
    lines = [
        json.dumps({"id": {"progress": {"opaqueCount": 1}}, "progress": {"stderr": "INFO: ..."}})
    ]
    lines += [bep_line(label, cached=cached) for label, cached in sorted(outcomes.items())]
    lines.append(json.dumps({"id": {"buildFinished": {}}, "finished": {"exitCode": {"code": 0}}}))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def fixture_acceptance_labels(count: int = ACCEPTANCE_MODULES) -> list[str]:
    return [module_target(module) for module in sorted(fixture_modules(count))]


CONTROL_LABEL = "//crates/ocx_exit:ocx_exit_test"


def fixture_tags(labels: list[str]) -> dict[str, set[str]]:
    return {label: {"local", CACHE_DEFEATING_TAG, SERIALISING_TAG} for label in labels}


def fixture_named_set(basename: str, digest: str, length: int) -> str:
    """One `namedSetOfFiles` event in the shape measured on bazel 9.2.0."""
    return json.dumps(
        {
            "id": {"namedSet": {"id": "0"}},
            "namedSetOfFiles": {
                "files": [
                    {
                        "name": basename,
                        "uri": f"file:///bazel-out/k8-fastbuild/bin/{basename}",
                        "digest": digest,
                        "length": str(length),
                    }
                ]
            },
        }
    )


# ---------------------------------------------------------------------------
# Self-test.
# ---------------------------------------------------------------------------


def expect(condition: bool, problem: str) -> None:
    """A loud exit — a bare `assert` vanishes under `python3 -O`."""
    if not condition:
        raise SystemExit(f"bazel accept proofs self-test: {problem}")


def _no_prefix_reader(tags: set[str]) -> bool:
    """The wrong tag reader, kept as a named control, called from nowhere else.

    "Any `no-*` tag means it will not be cached" is the natural rule and it is
    refuted by measurement: `--prove-s015` shows a target tagged
    `local, exclusive, no-cache` reporting `(cached)` on the second run in the
    same output base. This reader calls that target safe.
    """
    return any(tag.startswith("no-") for tag in tags) or "local" in tags


def _mtime_reader(built: Path, under_test: Path) -> bool:
    """The wrong freshness reader: "the binary under test is not older".

    `test/bin/ocx` is a `cp` of `target/release/ocx` (`test/taskfile.yml:125`),
    so a stale copy carries the *copy's* mtime, not the build's — this reader
    calls it fresh on exactly the bytes the digest comparator reds.
    """
    return under_test.stat().st_mtime >= built.stat().st_mtime


def _existence_reader(under_test: Path) -> bool:
    """The other wrong reader: the file is there and it is executable."""
    return under_test.is_file()


def prove_caching_off(scratch: Path) -> int:
    """S-015, first half — red and green on a 172-target fixture plus a control."""
    checks = 0
    labels = fixture_acceptance_labels()
    acceptance = set(labels)
    control = {CONTROL_LABEL}
    tags = fixture_tags(labels)

    warm_path = scratch / "warm.json"
    green_outcomes = {label: False for label in labels} | {CONTROL_LABEL: True}
    write_accept_bep(warm_path, green_outcomes)
    warm, parse = read_test_results(warm_path)
    expect(parse == [], f"the green BEP must parse clean, got {codes(parse)}")
    expect(len(warm) == ACCEPTANCE_MODULES + 1, f"the fixture BEP read {len(warm)} labels")

    green = caching_off_findings(warm=warm, acceptance=acceptance, control=control, tags=tags)
    expect(green == [], f"the compliant run must be silent, got {[f.message for f in green]}")
    print(
        f"S-015 GREEN: {ACCEPTANCE_MODULES} `external`-tagged acceptance targets re-ran on run 2 "
        f"while the untagged control {CONTROL_LABEL} reported cached"
    )
    checks += 1

    # --- one acceptance target reports cached. The Block-tier case.
    mutated = dict(green_outcomes)
    mutated[labels[7]] = True
    write_accept_bep(warm_path, mutated)
    reread, _ = read_test_results(warm_path)
    expect(reread[labels[7]] is True, "the cached-acceptance mutation did not land")
    expect(reread[labels[8]] is False, "the mutation must touch exactly one label")
    cached = caching_off_findings(warm=reread, acceptance=acceptance, control=control, tags=tags)
    expect(codes(cached) == ["accept-cached"], f"got {codes(cached)}")
    expect(labels[7] in cached[0].message, "the finding does not name the offending target")
    print(f"S-015 RED  : {cached[0].message}")
    checks += 1

    # --- the control does not cache either. "No acceptance target cached" is
    #     then consistent with caching being off for everything, or with run 2
    #     never happening; the control is the only thing that separates them.
    cold = dict(green_outcomes)
    cold[CONTROL_LABEL] = False
    write_accept_bep(warm_path, cold)
    reread, _ = read_test_results(warm_path)
    expect(reread[CONTROL_LABEL] is False, "the cold-control mutation did not land")
    control_cold = caching_off_findings(
        warm=reread, acceptance=acceptance, control=control, tags=tags
    )
    expect(codes(control_cold) == ["accept-control-cold"], f"got {codes(control_cold)}")
    print(f"S-015 RED  : {control_cold[0].message}")
    checks += 1

    # --- no control at all. Refused rather than reported as the property
    #     holding, which is what a control-free run would otherwise look like.
    write_accept_bep(warm_path, green_outcomes)
    warm, _ = read_test_results(warm_path)
    absent = caching_off_findings(warm=warm, acceptance=acceptance, control=set(), tags=tags)
    expect(codes(absent) == ["accept-control-absent"], f"got {codes(absent)}")
    print(f"S-015 RED  : {absent[0].message}")
    checks += 1

    # --- the tag that reads as sufficient and is not. Measured, not assumed.
    weak_tags = dict(tags)
    weak_tags[labels[3]] = {"local", "exclusive", "no-cache"}
    expect(
        CACHE_DEFEATING_TAG not in weak_tags[labels[3]] and "no-cache" in weak_tags[labels[3]],
        "the weak-tag mutation did not land",
    )
    weak = caching_off_findings(warm=warm, acceptance=acceptance, control=control, tags=weak_tags)
    expect(codes(weak) == ["accept-tag-insufficient"], f"got {codes(weak)}")
    print(f"S-015 RED  : {weak[0].message}")
    checks += 1

    # The control: the natural "any no-* tag will do" reader calls it safe.
    expect(
        _no_prefix_reader(weak_tags[labels[3]]) is True,
        "the control reader was expected to credit the no-cache tag",
    )
    expect(
        _no_prefix_reader(fixture_tags(labels)[labels[3]]) is True,
        "the control reader credits the compliant tag set too — it cannot discriminate",
    )
    print(
        "S-015 RED  : on that same tag set the no-prefix reader control answers 'safe' — and it "
        "answers 'safe' for the compliant set too, so it separates nothing. `--prove-s015` "
        "measured `no-cache` reporting (cached) on the pin"
    )
    checks += 1

    # --- no serialising tag: 172 targets, one compose stack.
    unserialised = dict(tags)
    unserialised[labels[9]] = {"local", CACHE_DEFEATING_TAG}
    expect(SERIALISING_TAG not in unserialised[labels[9]], "the exclusive-drop did not land")
    serial = caching_off_findings(
        warm=warm, acceptance=acceptance, control=control, tags=unserialised
    )
    expect(codes(serial) == ["accept-serial-tag-missing"], f"got {codes(serial)}")
    print(f"S-015 RED  : {serial[0].message}")
    checks += 1

    # --- reader floors. A query that read half the suite, and a BEP that did.
    half = set(labels[:80])
    narrow = caching_off_findings(
        warm=warm, acceptance=half, control=control, tags=tags, minimum=ACCEPTANCE_MODULES
    )
    expect(codes(narrow) == ["accept-universe-floor"], f"got {codes(narrow)}")
    print(f"S-015 RED  : {narrow[0].message}")
    checks += 1

    short_path = scratch / "short.json"
    write_accept_bep(short_path, {label: False for label in labels[:100]} | {CONTROL_LABEL: True})
    short, _ = read_test_results(short_path)
    expect(len(short) == 101, f"the short BEP read {len(short)} labels")
    floored = caching_off_findings(warm=short, acceptance=acceptance, control=control, tags=tags)
    expect(codes(floored) == ["accept-reader-floor"], f"got {codes(floored)}")
    print(f"S-015 RED  : {floored[0].message}")
    checks += 1

    # --- C-024's forbidden mechanism, in an rc file.
    clean = global_serialisation_findings(
        {".bazelrc": "build --jobs=12\nbuild --disk_cache=~/.cache/ocx/bazel-disk\n"}
    )
    expect(clean == [], f"a realm-free, flag-free rc must be silent, got {clean}")
    global_flag = global_serialisation_findings(
        {".bazelrc": "build --jobs=12\ntest --local_test_jobs=1\n"}
    )
    expect(codes(global_flag) == ["serialise-global"], f"got {codes(global_flag)}")
    print(f"S-015 RED  : {global_flag[0].message}")
    checks += 1

    # --- the serialisation reader, on the probe's own log grammar. Kept here
    #     rather than only inside `--prove-s015`: its first draft sorted the
    #     windows by NAME and reported a phantom overlap on a genuinely
    #     serialised trio, which is a live-mode red nobody could have attributed.
    serialised = (
        "excl1 start 100.0\nexcl1 end 100.7\n"
        "excl3 start 100.8\nexcl3 end 101.5\n"
        "excl2 start 101.6\nexcl2 end 102.3\n"
    )
    rows, overlaps = timeline_overlaps(serialised, SERIAL_TRIO)
    expect(len(rows) == 3, f"the timeline reader read {len(rows)} windows, expected 3")
    expect([row[2] for row in rows] == ["excl1", "excl3", "excl2"], "windows must be start-ordered")
    expect(overlaps == 0, f"three disjoint windows out of name order must not overlap, got {overlaps}")
    print("S-015 GREEN: three disjoint windows in non-alphabetical order report 0 overlaps")
    checks += 1

    concurrent = (
        "para1 start 200.0\npara1 end 200.7\n"
        "para2 start 200.1\npara2 end 200.8\n"
        "para3 start 200.2\npara3 end 200.9\n"
    )
    rows, overlaps = timeline_overlaps(concurrent, PARALLEL_TRIO)
    expect(len(rows) == 3, f"the timeline reader read {len(rows)} windows, expected 3")
    expect(overlaps == 2, f"three overlapping windows must report 2 pairs, got {overlaps}")
    print("S-015 RED  : three overlapping windows report 2 pairs — the trio was not serialised")
    checks += 1

    half, floored_overlaps = timeline_overlaps("excl1 start 100.0\n", SERIAL_TRIO)
    expect(
        half == [] and floored_overlaps == 0,
        f"a window with no end must not be read as one, got {half}",
    )
    print(
        "S-015 RED  : a start with no matching end reads as 0 windows, so `--prove-s015`'s "
        "timeline floor fires rather than a truncated log reporting perfect serialisation"
    )
    checks += 1
    return checks


def prove_binary_digest(scratch: Path) -> int:
    """S-015, second half — content, never path and never mtime."""
    checks = 0
    built_path = scratch / "build" / "ocx"
    test_path = scratch / "bin" / "ocx"
    built_path.parent.mkdir(parents=True, exist_ok=True)
    test_path.parent.mkdir(parents=True, exist_ok=True)

    fresh = b"ocx-build-B" + b"\x00" * 64
    stale = b"ocx-build-A" + b"\x00" * 64
    expect(len(fresh) == len(stale), "the two builds must be the same size, or size would tell")

    built_path.write_bytes(fresh)
    test_path.write_bytes(fresh)
    built = read_binary_file(built_path, "built")
    under_test = read_binary_file(test_path, "under-test")
    expect(built.digest == under_test.digest, "the green fixture's two copies must agree")
    green = binary_digest_findings(built, under_test)
    expect(green == [], f"matching digests must be silent, got {[f.message for f in green]}")
    print(f"S-015 GREEN: the binary under test hashes to {built.digest[:16]}… — this build's own")
    checks += 1

    # --- the stale copy. Same size, same path, *newer* mtime: the shape
    #     `cp target/release/ocx test/bin/ocx` leaves when the build it copied
    #     from was not the build that just ran.
    test_path.write_bytes(stale)
    time.sleep(0.01)
    test_path.touch()
    expect(test_path.read_bytes() == stale, "the stale-binary mutation did not land")
    expect(
        test_path.stat().st_size == built_path.stat().st_size,
        "the stale fixture must match on size, or the digest is not what discriminated",
    )
    stale_reading = read_binary_file(test_path, "under-test")
    expect(stale_reading.digest != built.digest, "the two fixtures hash the same — pick other bytes")
    red = binary_digest_findings(built, stale_reading)
    expect(codes(red) == ["binary-stale"], f"got {codes(red)}")
    expect(built.digest in red[0].message and stale_reading.digest in red[0].message, "both digests")
    print(f"S-015 RED  : {red[0].message}")
    checks += 1

    # The controls: both natural readers call that same file fresh.
    expect(_existence_reader(test_path) is True, "the existence control should answer 'fine'")
    expect(_mtime_reader(built_path, test_path) is True, "the mtime control should answer 'fresh'")
    print(
        "S-015 RED  : on those same bytes the existence control answers 'fine' and the mtime "
        "control answers 'fresh' — a stale copy carries the copy's mtime, not the build's"
    )
    checks += 1

    # --- reader floors, one per side, never a silent agreement between Nones.
    missing = read_binary_file(scratch / "build" / "absent", "built")
    expect(missing.digest is None, "the absent-file reading must carry no digest")
    one_side = binary_digest_findings(missing, under_test)
    expect(codes(one_side) == ["binary-built-unreadable"], f"got {codes(one_side)}")
    print(f"S-015 RED  : {one_side[0].message}")
    checks += 1

    both = binary_digest_findings(
        missing, read_binary_file(scratch / "bin" / "absent", "under-test")
    )
    expect(
        codes(both) == ["binary-built-unreadable", "binary-under-test-unreadable"],
        f"a run that read neither side must red on both, got {codes(both)}",
    )
    print("S-015 RED  : zero binaries read — two distinct findings, never a silent agreement")
    checks += 1

    empty_built = scratch / "build" / "empty"
    empty_test = scratch / "bin" / "empty"
    empty_built.write_bytes(b"")
    empty_test.write_bytes(b"")
    expect(empty_built.stat().st_size == 0, "the zero-byte mutation did not land")
    empties = binary_digest_findings(
        read_binary_file(empty_built, "built"), read_binary_file(empty_test, "under-test")
    )
    expect(codes(empties) == ["binary-empty"], f"two empty files must red, got {codes(empties)}")
    print(f"S-015 RED  : {empties[0].message}")
    checks += 1

    # --- the BEP side of the build reading, in the shape measured on the pin.
    bep = scratch / "digest_bep.json"
    bep.write_text(
        "\n".join(
            [
                json.dumps({"id": {"progress": {}}, "progress": {}}),
                fixture_named_set("ocx", built.digest, len(fresh)),
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    from_bep = read_bep_output_digest(bep, "ocx", "built")
    expect(from_bep.digest == built.digest, "the BEP reader must recover the published digest")
    expect(from_bep.length == len(fresh), f"the BEP reader read length {from_bep.length}")
    expect(
        binary_digest_findings(from_bep, under_test) == [],
        "the BEP-sourced digest must green against the same bytes",
    )
    print(
        "S-015 GREEN: the same verdict taken from the build's own event stream — "
        "namedSetOfFiles digest, not a bazel-bin path a later invocation may have moved"
    )
    checks += 1

    absent_entry = read_bep_output_digest(bep, "ocx-shim", "built")
    expect(absent_entry.digest is None, "a missing output must not yield a digest")
    named = binary_digest_findings(absent_entry, under_test)
    expect(codes(named) == ["binary-built-unreadable"], f"got {codes(named)}")
    print(f"S-015 RED  : {named[0].message}")
    checks += 1

    malformed = BinaryReading(side="built", source="<fixture>", digest="sha256:deadbeef", length=9)
    bad = binary_digest_findings(malformed, under_test)
    expect(codes(bad) == ["binary-digest-malformed"], f"got {codes(bad)}")
    print(f"S-015 RED  : {bad[0].message}")
    checks += 1

    # --- the durability half: equal today, and nothing re-runs when it changes.
    label = "//crates/ocx_cli:ocx"
    declared = binary_digest_findings(
        built, under_test, declared_inputs={"//test:conftest"}, binary_label=label
    )
    expect(codes(declared) == ["binary-undeclared"], f"got {codes(declared)}")
    print(f"S-015 RED  : {declared[0].message}")
    expect(
        binary_digest_findings(built, under_test, declared_inputs={label}, binary_label=label) == [],
        "a declared binary must green",
    )
    checks += 1
    return checks


def prove_selection(scratch: Path) -> int:
    """S-004 — the selection discriminator and the BEP reading beside it."""
    checks = 0
    modules = fixture_modules()
    rows = fixture_rows(modules)
    crate_of_dir = fixture_crate_of_dir()

    table = selection_table_findings(rows, modules)
    expect(table == [], f"the fixture table must be silent, got {[f.message for f in table]}")
    print(
        f"S-004 GREEN: {len(rows)} SCOPED_ROWS rows ({SCOPED_ESCALATE_ROWS} escalate) over "
        f"{len(modules)} acceptance modules, every glob live"
    )
    checks += 1

    docs = ["website/src/docs/reference/environment.md", "README.md", "CLAUDE.md"]
    crate_change = ["crates/crate03/src/lib.rs"]
    docs_selected, docs_notes = select_targets(
        docs, rows=rows, modules=modules, crate_of_dir=crate_of_dir
    )
    crate_selected, crate_notes = select_targets(
        crate_change, rows=rows, modules=modules, crate_of_dir=crate_of_dir
    )
    expect(docs_notes == [] and crate_notes == [], "neither selection should raise a note")
    expect(docs_selected == set(), f"a docs-only change selected {sorted(docs_selected)}")
    expect(crate_selected, "the crate-touching control selected nothing")
    expect(
        crate_selected != docs_selected and ALL_TARGET not in crate_selected,
        f"the control must select a proper subset, got {sorted(crate_selected)[:3]}",
    )
    print(
        f"S-004 GREEN: docs-only selects 0 acceptance targets; the crate-touching control "
        f"selects {len(crate_selected)} of {len(modules)} — the two answers differ"
    )
    checks += 1

    # --- the escalate row still maps to the whole acceptance package, beside
    #     the crate's own Rust label.
    escalated, _ = select_targets(
        ["crates/ocx_cli/src/command/install.rs"],
        rows=rows,
        modules=modules,
        crate_of_dir=crate_of_dir,
    )
    expect(
        escalated == {ALL_TARGET, "//crates/ocx_cli:all"},
        f"the escalate row must map to {ALL_TARGET} plus the crate label, got {escalated}",
    )
    print(
        f"S-004 GREEN: the `ocx` escalate row (package of crates/ocx_cli) maps to {ALL_TARGET}, "
        f"and the path maps to //crates/ocx_cli:all beside it"
    )
    checks += 1

    # --- the membership guard the heredoc skipped: a `crates/<dir>` that is no
    #     crate yields no label at all, where a path-shaped mapping yields one.
    orphan, _ = select_targets(
        ["crates/not_a_crate/src/lib.rs"],
        rows=rows,
        modules=modules,
        crate_of_dir=crate_of_dir,
    )
    expect(
        "crates/not_a_crate" not in crate_of_dir,
        "the orphan control must name a directory the workspace does not have",
    )
    expect(orphan == set(), f"a non-crate directory must select nothing, got {sorted(orphan)}")
    print(
        "S-004 GREEN: `crates/not_a_crate/src/lib.rs` selects nothing — the Rust label is "
        "behind the same `crate_of_dir` membership check the acceptance half uses"
    )
    checks += 1

    docs_bep = scratch / "docs_bep.json"
    crate_bep = scratch / "crate_bep.json"
    rust_labels = [f"//crates/crate{index:02d}:crate{index:02d}_test" for index in range(18)]
    write_accept_bep(docs_bep, {label: True for label in rust_labels})
    write_accept_bep(
        crate_bep,
        {label: True for label in rust_labels}
        | {label: False for label in sorted(crate_selected)}
        | {rust_labels[3]: False},
    )
    docs_read, _ = read_test_results(docs_bep)
    crate_read, _ = read_test_results(crate_bep)

    green = selection_findings(
        docs_selected=docs_selected,
        crate_selected=crate_selected,
        docs_bep=docs_read,
        crate_bep=crate_read,
    )
    expect(green == [], f"the wired selection must be silent, got {[f.message for f in green]}")
    print(
        "S-004 GREEN: the docs-only build's BEP carries 0 acceptance results and 0 Rust re-runs, "
        "and the control BEP carries both universes"
    )
    checks += 1

    # --- selection unwired: the docs-only change selects what the crate one did.
    unwired = selection_findings(
        docs_selected=crate_selected,
        crate_selected=crate_selected,
        docs_bep=docs_read,
        crate_bep=crate_read,
    )
    expect(codes(unwired) == ["select-docs-nonempty"], f"got {codes(unwired)}")
    expect(SELECT_SAME_CLAUSE.strip(" —") in unwired[0].message, "the finding must name the shape")
    print(f"S-004 RED  : {unwired[0].message}")
    checks += 1

    # --- the control selects nothing, so the green above was vacuous.
    vacuous = selection_findings(
        docs_selected=set(), crate_selected=set(), docs_bep=docs_read, crate_bep=crate_read
    )
    expect(codes(vacuous) == ["select-control-empty"], f"got {codes(vacuous)}")
    print(f"S-004 RED  : {vacuous[0].message}")
    checks += 1

    # --- the entry ran //test:all anyway. Observable only because acceptance
    #     targets never cache: every one of them appears in the BEP having run.
    ran_anyway = scratch / "docs_ran.json"
    write_accept_bep(
        ran_anyway,
        {label: True for label in rust_labels}
        | {label: False for label in fixture_acceptance_labels()},
    )
    ran_read, _ = read_test_results(ran_anyway)
    expect(
        sum(1 for label in ran_read if label.startswith("//test:")) == ACCEPTANCE_MODULES,
        "the ran-anyway mutation did not land",
    )
    ran = selection_findings(
        docs_selected=set(),
        crate_selected=crate_selected,
        docs_bep=ran_read,
        crate_bep=crate_read,
    )
    expect(codes(ran) == ["select-docs-reran"], f"got {codes(ran)}")
    print(f"S-004 RED  : {ran[0].message}")
    checks += 1

    # --- a Rust test re-ran on a docs-only change.
    rust_ran = scratch / "docs_rust.json"
    write_accept_bep(rust_ran, {label: True for label in rust_labels} | {rust_labels[5]: False})
    rust_read, _ = read_test_results(rust_ran)
    expect(rust_read[rust_labels[5]] is False, "the rust re-run mutation did not land")
    rust = selection_findings(
        docs_selected=set(),
        crate_selected=crate_selected,
        docs_bep=rust_read,
        crate_bep=crate_read,
    )
    expect(codes(rust) == ["select-rust-reran"], f"got {codes(rust)}")
    print(f"S-004 RED  : {rust[0].message}")
    checks += 1

    # --- the floor sits on the control run, never on the docs-only one: a
    #     docs-only build legitimately carries zero test results.
    empty_control = selection_findings(
        docs_selected=set(),
        crate_selected=crate_selected,
        docs_bep={},
        crate_bep={},
    )
    expect(codes(empty_control) == ["select-bep-floor"], f"got {codes(empty_control)}")
    print(f"S-004 RED  : {empty_control[0].message}")
    checks += 1

    # --- table floors: a short table, a short module list, a dead glob.
    short_rows = {crate: row for index, (crate, row) in enumerate(sorted(rows.items())) if index < 9}
    expect(len(short_rows) == 9, "the short-table mutation did not land")
    short = selection_table_findings(short_rows, modules)
    expect("select-rows-floor" in codes(short), f"got {codes(short)}")
    print(f"S-004 RED  : {next(f for f in short if f.code == 'select-rows-floor').message}")
    checks += 1

    thin = selection_table_findings(rows, set(sorted(modules)[:40]))
    expect("select-modules-floor" in codes(thin), f"got {codes(thin)}")
    print(f"S-004 RED  : {next(f for f in thin if f.code == 'select-modules-floor').message}")
    checks += 1

    stale_rows = dict(rows)
    stale_rows["crate04"] = "tests/test_departed_*.py"
    expect(
        not fnmatch.filter(sorted(modules), stale_rows["crate04"]),
        "the dead-glob mutation did not land — the glob still matches",
    )
    dead = selection_table_findings(stale_rows, modules)
    expect(codes(dead) == ["select-glob-dead"], f"got {codes(dead)}")
    print(f"S-004 RED  : {dead[0].message}")
    checks += 1

    parity = selection_table_findings(
        rows, modules, peer_escalates=frozenset({"ocx_test_support"})
    )
    expect(codes(parity) == ["select-escalate-parity"], f"got {codes(parity)}")
    print(f"S-004 RED  : {parity[0].message}")
    checks += 1
    return checks


def prove_live_table() -> int:
    """The two readers, pointed at the files that exist today.

    Everything above runs on fixtures this file builds, which proves the
    comparators and says nothing about the tree. These four lines are the other
    half: the `SCOPED_ROWS` reader and the module reader are run against the
    live `test/taskfile.yml` and `test/tests/`, and their answers are floored
    against this file's constants and cross-checked against `scoped_gate.py`'s
    independent reading of the same table.
    """
    # The peer reader, imported here rather than at module scope: it is the
    # only use, and `scoped_gate` is a sibling gate rather than a dependency.
    from scoped_gate import TABLE_ESCALATES

    rows = read_scoped_rows((REPO_ROOT / "test" / "taskfile.yml").read_text(encoding="utf-8"))
    modules = acceptance_modules(REPO_ROOT / "test")
    findings = selection_table_findings(rows, modules, peer_escalates=TABLE_ESCALATES)
    expect(
        findings == [],
        f"the live table and module list must satisfy their own floors: "
        f"{[f.message for f in findings]}",
    )
    print(
        f"S-004 GREEN: live read — {len(rows)} SCOPED_ROWS rows in test/taskfile.yml, "
        f"{len(modules)} modules in test/tests/, escalate rows agree with "
        f"scoped_gate.TABLE_ESCALATES ({sorted(TABLE_ESCALATES)})"
    )
    return 1


def prove_entry_points() -> int:
    """C-025's two entry points. The predicate on fixtures, the reader on live bytes."""
    checks = 0

    full = EntryPoint(
        name="bazel:test",
        role="full",
        items=["bazel test //... --keep_going", "task: bazel:lint"],
    )
    selected = EntryPoint(
        name="bazel:test:scoped",
        role="selected",
        items=[
            "python3 scripts/scoped_gate.py --json > {{.PLAN}}",
            "if: '[ -n \"{{.CRATES}}\" ]' cmd: bazel test $(cat {{.TARGETS}})",
        ],
    )
    expect(entry_findings(full) == [], "the full-suite entry must be silent")
    expect(entry_findings(selected) == [], "the change-selected entry must be silent")
    print(
        "S-004 GREEN: `bazel:test` runs //... unconditionally; `bazel:test:scoped` names a "
        "selection source and guards its only whole-scope item"
    )
    checks += 1

    defeated = dataclasses.replace(
        selected, items=[*selected.items, "bazel test //... --build_tests_only"]
    )
    expect(
        len(defeated.items) == len(selected.items) + 1
        and "//..." in defeated.items[-1]
        and not GUARDED_ITEM.match(defeated.items[-1]),
        "the unconditional-//... mutation did not land",
    )
    red = entry_findings(defeated)
    expect(codes(red) == ["entry-selection-unconditional"], f"got {codes(red)}")
    print(f"S-004 RED  : {red[0].message}")
    checks += 1

    nameless = dataclasses.replace(
        selected, items=["if: '[ -n \"{{.X}}\" ]' cmd: bazel test //test:test_install"]
    )
    expect(
        not any(token in nameless.items[0] for token in SELECTION_TOKENS),
        "the selection-source mutation did not land",
    )
    blind = entry_findings(nameless)
    expect(codes(blind) == ["entry-selection-absent"], f"got {codes(blind)}")
    print(f"S-004 RED  : {blind[0].message}")
    checks += 1

    # The opposite leg, on the same two recipes: neither can satisfy both roles.
    swapped = entry_findings(dataclasses.replace(selected, role="full"))
    expect(codes(swapped) == ["entry-full-partial"], f"got {codes(swapped)}")
    print(f"S-004 RED  : {swapped[0].message}")
    checks += 1

    crossed = entry_findings(dataclasses.replace(full, role="selected"))
    expect(
        codes(crossed) == ["entry-selection-absent", "entry-selection-unconditional"],
        f"the full recipe must fail both selected legs, got {codes(crossed)}",
    )
    print(
        "S-004 RED  : the full-suite recipe judged as change-selected reds on both legs — "
        "one recipe cannot be both entry points, which is why C-025 asks for two"
    )
    checks += 1

    absent = entry_findings(EntryPoint(name="bazel:test:scoped", role="selected", items=[]))
    expect(codes(absent) == ["entry-reader-floor"], f"got {codes(absent)}")
    print(f"S-004 RED  : {absent[0].message}")
    checks += 1

    # The reader, on the bytes C-025 is about. This leg read `taskfile.yml`'s
    # `verify` / `verify:scoped` pair while WP-39's taskfile did not exist; it
    # does now, so the live subject is the two entry points themselves — the
    # substitute proved the predicate could separate *a* pair and said nothing
    # about the tasks the contract names.
    entry_taskfile = (REPO_ROOT / "taskfiles" / "bazel.taskfile.yml").read_text(encoding="utf-8")
    live_full = EntryPoint(name="bazel:test", role="full", items=task_cmds(entry_taskfile, "test"))
    live_selected = EntryPoint(
        name="bazel:test:scoped",
        role="selected",
        items=task_cmds(entry_taskfile, "test:scoped"),
    )
    expect(len(live_full.items) >= 4, f"the reader read {len(live_full.items)} items from `test`")
    expect(
        len(live_selected.items) >= 3,
        f"the reader read {len(live_selected.items)} items from `test:scoped`",
    )
    expect(
        entry_findings(live_full) == [],
        f"live `bazel:test` must green as full: {[f.message for f in entry_findings(live_full)]}",
    )
    expect(
        entry_findings(live_selected) == [],
        f"live `bazel:test:scoped` must green: "
        f"{[f.message for f in entry_findings(live_selected)]}",
    )
    print(
        f"S-004 GREEN (live): taskfiles/bazel.taskfile.yml — `bazel:test` "
        f"({len(live_full.items)} items) runs the whole scope unconditionally; "
        f"`bazel:test:scoped` ({len(live_selected.items)} items) names a selection source and "
        f"guards every whole-scope item"
    )
    checks += 1

    # The same live bytes, one item planted ahead of the guarded one: the red
    # half of the leg above, so a green there cannot be a reader that stopped.
    planted = dataclasses.replace(
        live_selected,
        items=["ocx exec bazel -- bazel test //...", *live_selected.items],
    )
    expect(
        codes(entry_findings(planted)) == ["entry-selection-unconditional"],
        f"the planted whole-scope item must red: {codes(entry_findings(planted))}",
    )
    print(f"S-004 RED  : {entry_findings(planted)[0].message}")
    checks += 1
    return checks


def prove_c029(scratch: Path) -> int:
    """C-029 — nothing here can reach the owner-gated realm."""
    checks = 0
    disk_only = probe_rc(
        Path("/probe"), Path("/cache/disk"), Path("/cache/out"), Path("/cache/repo")
    )
    argv = probe_argv(Path("/bin/bazel"), "test", "//:all", "--build_event_json_file=/probe/a.json")
    green = realm_findings({".bazelrc": disk_only, "argv": " ".join(argv)})
    expect(green == [], f"the disk-cache-only probe must be silent, got {green}")
    expect("--nosystem_rc" in argv and "--nohome_rc" in argv, "the probe must refuse ambient rcs")
    print(
        "C-029 GREEN: the probe's whole rc is --disk_cache, its argv names no remote flag, and "
        "--nosystem_rc --nohome_rc keep an ambient ~/.bazelrc out of the measurement"
    )
    checks += 1

    leaky = disk_only + "build --remote_cache=https://bazel-cache.ocx.sh/v1\n"
    expect("remote_cache" in leaky, "the realm-flag mutation did not land")
    red = realm_findings({".bazelrc": leaky})
    expect(codes(red) == ["realm-flag"], f"got {codes(red)}")
    print(f"C-029 RED  : {red[0].message}")
    checks += 1

    credential = "build --remote_header=Authorization=$BAZEL_CACHE_READ_AUTH\n"
    creds = realm_findings({"$RUNNER_TEMP/bazelrc": credential})
    expect(codes(creds) == ["realm-env", "realm-flag"], f"got {codes(creds)}")
    print(f"C-029 RED  : {next(f for f in creds if f.code == 'realm-env').message}")
    checks += 1

    source = Path(__file__).read_text(encoding="utf-8")
    hits = env_reads(source)
    expect(hits == [], f"this file reads the environment: {hits}")
    print(
        "C-029 GREEN: no `import os`, no os.environ and no os.getenv in this file — "
        "BAZEL_CACHE_READ_AUTH and BAZEL_CACHE_WRITE_AUTH are unreachable from here"
    )
    checks += 1

    mutated_path = scratch / "mutated_accept_proofs.py"
    mutated = source + "\nimport os\n\n\ndef _leak() -> object:\n    return os.getenv('BAZEL_CACHE_READ_AUTH')\n"
    mutated_path.write_text(mutated, encoding="utf-8")
    landed = mutated_path.read_text(encoding="utf-8")
    expect(landed != source and landed.endswith("')\n"), "the env-read mutation did not land")
    found = env_reads(landed)
    expect(found == ["import os", "os.getenv"], f"the mutated copy must red on both, got {found}")
    print(f"C-029 RED  : {found} — an added environment read is visible to the AST check")
    checks += 1
    return checks


def prove_counts() -> int:
    """The constants, against the live tree rather than against themselves."""
    modules = acceptance_modules(REPO_ROOT / "test")
    rows = read_scoped_rows((REPO_ROOT / "test" / "taskfile.yml").read_text(encoding="utf-8"))
    escalates = {crate for crate, row in rows.items() if row == "escalate"}
    expect(
        len(modules) == ACCEPTANCE_MODULES,
        f"test/tests/ holds {len(modules)} modules, this file's floor says {ACCEPTANCE_MODULES}",
    )
    expect(len(rows) == SCOPED_ROWS, f"SCOPED_ROWS has {len(rows)} rows, floor says {SCOPED_ROWS}")
    expect(
        len(escalates) == SCOPED_ESCALATE_ROWS,
        f"{len(escalates)} escalate rows, floor says {SCOPED_ESCALATE_ROWS}",
    )
    expect(
        CACHE_DEFEATING_TAG not in INSUFFICIENT_TAGS,
        "the tag this file credits must not also be in the set it refuses to credit",
    )
    print(
        f"counts  OK : {ACCEPTANCE_MODULES} acceptance modules and {SCOPED_ROWS} SCOPED_ROWS rows "
        f"({SCOPED_ESCALATE_ROWS} escalate) read off the live tree, not asserted against themselves"
    )
    return 1


def self_test() -> int:
    """All three proofs, both halves, on inputs this file builds."""
    scratch = REPO_ROOT / ".tmp"
    scratch.mkdir(exist_ok=True)
    checks = 0
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        work = Path(directory)
        checks += prove_caching_off(work)
        checks += prove_binary_digest(work)
        checks += prove_selection(work)
        checks += prove_live_table()
        checks += prove_entry_points()
        checks += prove_c029(work)
        checks += prove_counts()
    print(
        f"bazel accept proofs self-test: {checks} checks passed — S-015 (caching off, and the "
        "binary under test) and S-004 (selection, and the CI entry point) each shown red and "
        "green, plus C-029 and the live table floors"
    )
    print(
        "  The tag semantics all of S-015 rests on are measured, not assumed: run "
        "`python3 scripts/bazel_accept_proofs.py --prove-s015` (~40 s, --disk_cache only, "
        "no acceptance suite and no registry)."
    )
    print(
        "  wired into taskfiles/scripts.taskfile.yml `self-test:` (WP-17): "
        "`- python3 scripts/bazel_accept_proofs.py --self-test`"
    )
    return 0


# ---------------------------------------------------------------------------
# Live check modes. The I/O WP-36 and WP-39 own, wired to the comparators above.
# ---------------------------------------------------------------------------


def read_tag_table(path: Path) -> tuple[dict[str, set[str]], list[Finding]]:
    """`{label: [tag, ...]}` JSON — WP-36's `bazel query --output=...` reduced."""
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return {}, [Finding("tags-unreadable", f"S-015: cannot read the tag table {path}: {error}")]
    if not isinstance(raw, dict) or not raw:
        return {}, [
            Finding("tags-empty", f"S-015: the tag table {path} carries no label — a query that "
                    "matched nothing exits 0, so the count is the floor, never the exit code")
        ]
    return {label: set(tags) for label, tags in raw.items()}, []


def run_check_s015(
    *,
    warm: Path,
    tags_path: Path,
    built: Path | None,
    under_test: Path | None,
    control_prefix: str,
) -> int:
    findings: list[Finding] = []
    outcomes, parse = read_test_results(warm)
    findings.extend(parse)
    tags, tag_findings = read_tag_table(tags_path)
    findings.extend(tag_findings)

    acceptance = {label for label in tags if label.startswith(f"{ACCEPTANCE_PACKAGE}:")}
    control = {label for label in outcomes if label.startswith(control_prefix)}
    findings.extend(
        caching_off_findings(warm=outcomes, acceptance=acceptance, control=control, tags=tags)
    )
    rc_files = {
        str(path): path.read_text(encoding="utf-8")
        for path in (REPO_ROOT / ".bazelrc", REPO_ROOT / ".bazelrc.user")
        if path.is_file()
    }
    findings.extend(global_serialisation_findings(rc_files))

    if built is not None and under_test is not None:
        findings.extend(
            binary_digest_findings(
                read_binary_file(built, "built"), read_binary_file(under_test, "under-test")
            )
        )
    if findings:
        sys_out = report(findings)
        print("S-015: NOT MET", flush=True)
        return sys_out
    print(
        f"S-015: MET — {len(acceptance)} acceptance target(s) re-ran on an unchanged tree while "
        f"{len(control)} untagged control target(s) reported cached"
    )
    return 0


def run_check_s004(*, docs_bep: Path, crate_bep: Path, entry_taskfile: Path | None) -> int:
    from scoped_gate import TABLE_ESCALATES, cargo_metadata, parse_workspace

    findings: list[Finding] = []
    rows = read_scoped_rows((REPO_ROOT / "test" / "taskfile.yml").read_text(encoding="utf-8"))
    modules = acceptance_modules(REPO_ROOT / "test")
    findings.extend(selection_table_findings(rows, modules, peer_escalates=TABLE_ESCALATES))

    workspace = parse_workspace(cargo_metadata(REPO_ROOT))
    crate_of_dir = workspace.crate_of_dir
    docs_change = ["website/src/docs/reference/environment.md"]
    # The control is the alphabetically first member directory, chosen so the
    # probe is deterministic across checkouts rather than pinned to a crate
    # name that a later split could retire. Any member works: what the control
    # has to establish is that the selector selects *something*.
    crate_change = [f"{min(crate_of_dir)}/src/lib.rs"]
    docs_selected, notes = select_targets(
        docs_change, rows=rows, modules=modules, crate_of_dir=crate_of_dir
    )
    findings.extend(notes)
    crate_selected, notes = select_targets(
        crate_change, rows=rows, modules=modules, crate_of_dir=crate_of_dir
    )
    findings.extend(notes)

    docs_read, parse = read_test_results(docs_bep)
    findings.extend(parse)
    crate_read, parse = read_test_results(crate_bep)
    findings.extend(parse)
    findings.extend(
        selection_findings(
            docs_selected=docs_selected,
            crate_selected=crate_selected,
            docs_bep=docs_read,
            crate_bep=crate_read,
        )
    )

    if entry_taskfile is not None:
        text = entry_taskfile.read_text(encoding="utf-8") if entry_taskfile.is_file() else ""
        for name, role in (("test", "full"), ("test:scoped", "selected")):
            findings.extend(
                entry_findings(EntryPoint(name=name, role=role, items=task_cmds(text, name)))
            )

    if findings:
        sys_out = report(findings)
        print("S-004: NOT MET", flush=True)
        return sys_out
    print(
        f"S-004: MET — a docs-only change selects 0 acceptance targets and re-runs 0 Rust tests; "
        f"the crate-touching control selects {len(crate_selected)}"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--self-test", action="store_true", help="prove all three pairs red and green")
    mode.add_argument("--prove-s015", action="store_true", help="measure the tag semantics live")
    mode.add_argument("--check-s015", action="store_true", help="judge a warm acceptance run")
    mode.add_argument("--check-s004", action="store_true", help="judge a change-selected run")
    parser.add_argument("--probe-root", type=Path, default=PROBE_DEFAULT_ROOT)
    parser.add_argument("--warm", type=Path, help="--check-s015: run 2's BEP")
    parser.add_argument("--tags", type=Path, help="--check-s015: {label: [tag, ...]} JSON")
    parser.add_argument("--built-binary", type=Path, help="--check-s015: what the build produced")
    parser.add_argument("--binary-under-test", type=Path, help="--check-s015: what the suite runs")
    parser.add_argument("--control-prefix", default="//crates/", help="--check-s015: control labels")
    parser.add_argument("--docs-bep", type=Path, help="--check-s004: the docs-only build's BEP")
    parser.add_argument("--crate-bep", type=Path, help="--check-s004: the control build's BEP")
    parser.add_argument("--entry-taskfile", type=Path, help="--check-s004: WP-39's taskfile")
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if args.prove_s015:
        return run_prove_s015(args.probe_root)
    if args.check_s015:
        if args.warm is None or args.tags is None:
            parser.error("--check-s015 needs --warm and --tags")
        return run_check_s015(
            warm=args.warm,
            tags_path=args.tags,
            built=args.built_binary,
            under_test=args.binary_under_test,
            control_prefix=args.control_prefix,
        )
    if args.docs_bep is None or args.crate_bep is None:
        parser.error("--check-s004 needs --docs-bep and --crate-bep")
    return run_check_s004(
        docs_bep=args.docs_bep, crate_bep=args.crate_bep, entry_taskfile=args.entry_taskfile
    )


if __name__ == "__main__":
    raise SystemExit(main())
