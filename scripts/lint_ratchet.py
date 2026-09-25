#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Per-lint-code ratchet over compiler diagnostics (rust-cargo.md LINT-16,
plan_bazel_cargo_port.md C-012/C-013/C-017/C-018).

Input is `--bep PATH`, a Bazel build event stream. The `*.clippy.diagnostics`
files (`task rust:clippy:check`) or `*.rustdoc.diagnostics` files
(`task rust:doc:ratchet`, `--suffix .rustdoc.diagnostics`) its output-group
file sets name are read as rustc-native JSON lines and attributed to the member
whose `//crates/<member>` package produced them. One compare is both the
`-D warnings` gate and the backlog ratchet — any key above its entry, an absent
key counting as 0, fails. The baseline may hold only `--allow-codes` codes
(C-018), and a Cargo `[lints]` entry Bazel does not read is refused (C-017).

Its proofs run as pytest, from `scripts/tests/test_lint_ratchet.py`.

Groups every compiler diagnostic of a workspace member by `.message.code.code`,
deduplicated by primary span (the lib and the lib-test targets report the
same span twice), and compares the counts to the baseline file:

  - a code above its entry fails (a regression);
  - a code absent from the baseline has an implicit entry of 0;
  - a baseline entry of 0 fails — that lint belongs in `[workspace.lints]`
    now, deleted from the baseline in the same commit;
  - a code in both the baseline and `[workspace.lints]` fails (LINT-16);
  - a decrease passes with a notice: run `--update` and commit the new file.

`--update` writes a **downward** move freely — locking in an improvement is the
point of the ratchet, and a decrease is what `--check` already asked for. An
upward move is refused: `--update` alone exits 1 and names every key that rose,
so the one command a red prompts cannot also be the command that clears it.
Raising a baseline needs `--update --allow-regression`, which prints the raised
keys on the way through. There is no per-key escape — a file rename under
`--by-file` moves counts between keys and therefore needs the flag too, which
is the intended prompt to look at what moved rather than a reason to loosen the
rule. A baseline file that does not exist yet reads as `{}`, so creating one is
itself an upward move and takes the flag once.

Diagnostics files produced outside `//crates` (the vendored forks under
`external/`) are ignored — they are not under this repository's lint policy.

A stream that does not describe a **whole-workspace** run fails instead of
reading as "every count dropped to 0". Two independent conditions, both
fail-closed: every member `[workspace] members` selects owns at least one
diagnostics file, and the stream carries a successful `buildFinished` and its
`lastMessage`. Counting diagnostics alone is not enough — a run truncated after one crate's
first warning carries a diagnostic, so every *other* baseline key reads as a
decrease, `--check` exits 0, and `--update` writes the partial payload with no
rises and therefore without `--allow-regression`. Erasure by truncation arrives
as decreases, which the upward-move gate cannot see. The expected member set is
derived from the manifest and the filesystem, never from the stream: a set read
out of the stream is satisfied by whatever the stream happened to carry.

Stdlib only; `task rust:clippy:check` and `task rust:doc:ratchet` are the
callers.

`--by-file` keys each entry `<file>::<code>` instead of `<code>` alone. A bare
per-code count lets one broken link be fixed while another appears elsewhere
in the same pass — across a nine-batch crate split that is the expected case,
not a hypothesis. The cost is that moving a file reds the ratchet until
`--update` is run, which is the intended prompt to look. The clippy baseline
is per-file too (`task rust:clippy:check` passes `--by-file`): crate-boundary
`pub` promotion is exactly what `unreachable_pub` fires on, so per-code counts
would hide one crate's rise behind another's fix.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
import urllib.parse
import urllib.request
from collections import Counter
from collections.abc import Iterable
from pathlib import Path, PurePosixPath
from typing import NamedTuple

REPO_ROOT = Path(__file__).resolve().parents[1]
BASELINE = REPO_ROOT / "clippy-warn-baseline.json"
WORKSPACE_MANIFEST = REPO_ROOT / "Cargo.toml"
DEFAULT_SUFFIX = ".clippy.diagnostics"
#: C-018's clippy allow-list; `task rust:doc:ratchet` passes its own.
DEFAULT_ALLOW_CODES = ("unreachable_pub",)
#: The code a spanned diagnostic with no lint code is keyed under (C-013).
UNCODED = "<uncoded>"
#: rustc's per-crate summary — span-less, code-less, and the ONLY such record
#: the gate may drop. Measured over the 58 diagnostics files of this tree: every
#: one reads `N warnings emitted` / `1 warning emitted`. (cargo's "generated N
#: warnings" line never reaches a rustc-native file.)
RUSTC_SUMMARY = re.compile(r"^\d+ warnings? emitted$")
#: `rust_clippy_aspect`'s opt-out tags (rules_rust 0.74 `clippy.bzl`
#: `get_clippy_ready_crate_info`), compared after `-` → `_` and lower-casing.
CLIPPY_OPT_OUT = frozenset({"no_clippy", "no_lint", "nolint", "noclippy"})
#: The rule kinds that provide `CrateInfo` / `TestCrateInfo`, which is exactly
#: what the clippy aspect visits — each must leave a diagnostics file.
ASPECT_KINDS = frozenset(
    {"rust_library", "rust_binary", "rust_test", "rust_proc_macro", "rust_shared_library", "rust_static_library"}
)


class Aspect(NamedTuple):
    """What the aspect writing one diagnostics suffix visits: the rule kinds
    that must each leave a file, and the tags that make it skip a target."""

    name: str
    kinds: frozenset[str]
    opt_out: frozenset[str]


#: Per suffix, because the two aspects cover different targets:
#: `rustdoc_diagnostics_aspect` (`rustdoc_diagnostics.bzl`) documents non-test
#: crates only — `cargo doc` has no test target to document — and honours no
#: tag, so a clippy opt-out tag is not its concern.
ASPECTS = {
    DEFAULT_SUFFIX: Aspect("rust_clippy_aspect", ASPECT_KINDS, CLIPPY_OPT_OUT),
    ".rustdoc.diagnostics": Aspect("rustdoc_diagnostics_aspect", ASPECT_KINDS - {"rust_test"}, frozenset()),
}


def workspace_members(manifest: Path = WORKSPACE_MANIFEST) -> set[str]:
    """The `crates/` directory name of every member `[workspace] members`
    selects.

    Read from the manifest and the filesystem, never from the stream — the
    coverage gate exists to catch a stream that is short, so a set derived from
    that same stream would be satisfied by construction.

    Fails closed rather than quietly selecting less: an entry that is neither a
    literal directory nor a `<dir>/*` glob, one that matches no crate, and one
    resolving outside `crates/` are each a refusal. `run` builds its member
    prefix from `crates/` alone, so a member elsewhere would never be seen in a
    stream and the gate could not go green."""
    root = manifest.parent
    crates = root / "crates"
    members: set[str] = set()
    for entry in tomllib.loads(manifest.read_text(encoding="utf-8"))["workspace"]["members"]:
        if entry.endswith("/*"):
            parent = root / entry[: -len("/*")]
            found = (
                sorted(path for path in parent.iterdir() if (path / "Cargo.toml").is_file())
                if parent.is_dir()
                else []
            )
        elif "*" in entry:
            raise SystemExit(
                f"lint ratchet: [workspace] members entry {entry!r} uses a glob this reader does not "
                "expand — every crate it selects would drop out of the coverage census, which only "
                "ever makes the gate weaker"
            )
        else:
            candidate = root / entry
            found = [candidate] if (candidate / "Cargo.toml").is_file() else []
        if not found:
            raise SystemExit(
                f"lint ratchet: [workspace] members entry {entry!r} names no crate under {root}"
            )
        for path in found:
            if path.parent != crates:
                raise SystemExit(
                    f"lint ratchet: [workspace] members selects {path}, which is not a direct child of "
                    f"{crates} — the member prefix this script builds could never match it"
                )
            members.add(path.name)
    if not members:
        raise SystemExit(f"lint ratchet: [workspace] members selects no crate in {manifest}")
    return members


class Census(NamedTuple):
    """What one stream says about the workspace.

    `member_records` and `diagnosing` are the same evidence at two
    granularities — how many diagnostics arrived, and which members they
    arrived for. `reported` is the coverage claim; a stream that did not
    finish never becomes a census (`bep_census` refuses it)."""

    counts: Counter[str]
    member_records: int
    reported: set[str]
    diagnosing: set[str]


def tally(messages: Iterable[tuple[str, dict]], by_file: bool) -> tuple[Counter[str], int, set[str]]:
    """Unique diagnostics per key over `(member, rustc message)` pairs, the
    number of counted records, and the members a counted record arrived for.

    A diagnostic is deduplicated by `(code, file, line, column)` of its
    primary span (the lib and its `crate =` test target report the same span
    twice).

    Every code-less warning is keyed as `UNCODED` (C-013) — spanned as
    `<file>::<uncoded>`, span-less as `<no span>::<uncoded>` — so it fails like
    any other new key; only rustc's own `RUSTC_SUMMARY` line is dropped,
    matched by its exact text rather than by its shape."""
    seen: set[tuple] = set()
    counts: Counter[str] = Counter()
    records = 0
    diagnosing: set[str] = set()
    for member, message in messages:
        if message.get("level") not in ("warning", "error"):
            continue
        primary = next((s for s in message.get("spans", []) if s.get("is_primary")), None)
        code = (message.get("code") or {}).get("code")
        if code is None:
            if not primary and RUSTC_SUMMARY.match(str(message.get("message", ""))):
                continue
            code = UNCODED
        records += 1
        diagnosing.add(member)
        key = (
            (code, primary["file_name"], primary["line_start"], primary["column_start"])
            if primary
            else (code, message.get("rendered"))
        )
        if key in seen:
            continue
        seen.add(key)
        if by_file:
            counts[f"{primary['file_name'] if primary else '<no span>'}::{code}"] += 1
        else:
            counts[code] += 1
    return counts, records, diagnosing


def workspace_lints(manifest: Path = WORKSPACE_MANIFEST) -> set[str]:
    """Lint codes already enforced through `[workspace.lints]`, clippy-prefixed."""
    table = (
        tomllib.loads(manifest.read_text(encoding="utf-8"))
        .get("workspace", {})
        .get("lints", {})
    )
    names = set(table.get("rust", {}))
    names |= {f"clippy::{name}" for name in table.get("clippy", {})}
    return names


def code_of(key: str, by_file: bool) -> str:
    """The lint code inside a baseline key. A `<file>::<code>` key splits on
    its FIRST `::`: a path holds none, and `clippy::foo` holds one of its own.

    A `by_file` key with no `::` is malformed rather than code-less. Returning
    `""` for one made the LINT-16 double-listing check skip it in silence —
    `"" in enforced` is always false — so the key is named instead."""
    if not by_file:
        return key
    head, separator, code = key.partition("::")
    if not separator:
        raise SystemExit(
            f"lint ratchet: baseline key {key!r} carries no `::`, so it names no lint code "
            "— a --by-file baseline was read as a per-code one, or the file is corrupt"
        )
    del head
    return code


def member_of(key: str) -> str | None:
    """The workspace member a `<file>::<code>` key attributes its diagnostic
    to, or `None` for a per-code key, which attributes it to nobody.

    Read off the committed baseline rather than maintained anywhere: the
    baseline is the only record of *which crates had a backlog*, and that is
    the evidence a stream claiming they are all clean has to survive."""
    parts = PurePosixPath(key.split("::", 1)[0]).parts
    return parts[1] if len(parts) > 1 and parts[0] == "crates" else None


def compare(
    live: Counter[str],
    baseline: dict[str, int],
    enforced: set[str],
    by_file: bool = False,
) -> tuple[list[str], list[str]]:
    """(failures, notices) of `live` against `baseline`."""
    failures: list[str] = []
    notices: list[str] = []
    for key, entry in sorted(baseline.items()):
        if entry == 0:
            failures.append(
                f"{key}: baseline entry is 0 — move it into [workspace.lints] and delete the entry"
            )
        if code_of(key, by_file) in enforced:
            failures.append(
                f"{key}: listed in both the baseline and [workspace.lints]"
            )
    for key in sorted(set(live) | set(baseline)):
        count = live.get(key, 0)
        entry = baseline.get(key, 0)
        if count > entry:
            failures.append(
                f"{key}: {count} live, baseline {entry} (+{count - entry})"
            )
        elif count < entry:
            notices.append(
                f"{key}: dropped to {count} (baseline {entry}) — run `--update` and commit"
            )
    return failures, notices


def raised(live: Counter[str], baseline: dict[str, int]) -> list[str]:
    """Keys `live` puts above `baseline`, rendered — an absent entry is 0.

    The same rule `compare` applies on `--check`, kept as its own function so
    the write path cannot drift from the read path and so it is testable
    without touching a file."""
    return [
        f"{key}: {live[key]} live, baseline {baseline.get(key, 0)} (+{live[key] - baseline.get(key, 0)})"
        for key in sorted(live)
        if live[key] > baseline.get(key, 0)
    ]


def read_baseline(baseline_path: Path) -> dict[str, int]:
    """The committed entries, or `{}` when the file does not exist yet."""
    try:
        return json.loads(baseline_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}


def judge(
    mode: str,
    census: Census,
    baseline_path: Path,
    by_file: bool,
    allow_regression: bool,
    allow_codes: list[str],
) -> int:
    """The verdict over one census: coverage, the zero-diagnostic and
    dark-member refusals, then compare or update."""
    live, member_records = census.counts, census.member_records
    expected = workspace_members()
    missing = sorted(expected - census.reported)
    if missing:
        print(
            f"lint ratchet: the stream does not describe a whole-workspace run — {len(missing)} of "
            f"{len(expected)} member(s) own no diagnostics file in the build event stream: "
            f"{', '.join(missing)}. "
            "A run truncated after one crate leaves every other key reading as a decrease, so --check "
            "would exit 0 and --update would write the partial payload with no rises and therefore "
            "without --allow-regression, erasing the backlog. Re-run the whole gate "
            "(`task rust:clippy:check` / `task rust:doc:ratchet`), which covers every member",
            file=sys.stderr,
        )
        return 1
    if member_records == 0:
        print(
            "lint ratchet: the input carries no diagnostic for a workspace member — "
            "empty input, a truncated run, not the diagnostics format this mode reads, or the "
            "lint flags were not passed. If the backlog is genuinely clear, delete "
            f"{baseline_path.name} and move the lint into [workspace.lints] instead",
            file=sys.stderr,
        )
        return 1
    # Which crates had a backlog is recorded in exactly one place — the
    # baseline — and a stream claiming they are now all clean has to survive
    # it. A member the baseline attributes keys to that produced no coded
    # diagnostic here is either genuinely fixed or was never linted, and the
    # two are byte-identical in the stream; the committed file is what breaks
    # the tie, so the drop is recorded deliberately rather than taken. Without
    # this, 21 artifacts plus one member's diagnostics rewrote a 198-key
    # baseline to 36 keys under a plain `--update`, exit 0.
    dark = sorted({member for key in read_baseline(baseline_path) if (member := member_of(key))} - census.diagnosing)
    if dark and not (mode == "update" and allow_regression):
        print(
            f"lint ratchet: {len(dark)} member(s) the baseline attributes keys to produced no "
            f"diagnostic in this stream: {', '.join(dark)}. Every one of their entries would drop "
            "to 0, which is what a run that never linted them looks like. Re-run the whole gate; if "
            "they really are clean, re-run --update with --allow-regression to record the drop",
            file=sys.stderr,
        )
        return 1
    if dark:
        print(f"lint ratchet: --allow-regression accepting {len(dark)} member(s) gone quiet: {', '.join(dark)}")
    if mode == "update":
        payload = {code: count for code, count in sorted(live.items()) if count > 0}
        # C-018, ahead of the rise gate and deaf to --allow-regression: that
        # flag records a backlog `-D warnings` once allowed, never a new code.
        foreign = foreign_keys(payload, by_file, allow_codes)
        if foreign:
            print(
                f"lint ratchet: refusing to write {baseline_path.name} — {len(foreign)} key(s) carry a "
                f"code outside the allow-list ({', '.join(allow_codes)}): {', '.join(foreign)}. "
                "Fix it; this code cannot be baselined, even with --allow-regression",
                file=sys.stderr,
            )
            return 1
        rises = raised(live, read_baseline(baseline_path))
        if rises and not allow_regression:
            print(
                f"lint ratchet: refusing to raise {baseline_path.name} — "
                f"{len(rises)} key(s) are above their committed entry:",
                file=sys.stderr,
            )
            for rise in rises:
                print(f"lint ratchet:   {rise}", file=sys.stderr)
            print(
                "lint ratchet: fix them, or re-run with --allow-regression to "
                "record the raise deliberately",
                file=sys.stderr,
            )
            return 1
        if rises:
            print(f"lint ratchet: --allow-regression raising {len(rises)} key(s):")
            for rise in rises:
                print(f"lint ratchet:   {rise}")
        baseline_path.write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        print(f"lint ratchet: wrote {baseline_path}: {payload}")
        return 0
    # `--check` still reads the file directly: a missing baseline is a wiring
    # fault the gate should crash on, not an empty set every live key then
    # reds against — and a stream with no diagnostics would read as clean.
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    failures, notices = compare(live, baseline, workspace_lints(), by_file)
    failures += [
        f"{key}: a baseline entry outside the allow-list — fix it; this code cannot be baselined"
        for key in foreign_keys(baseline, by_file, allow_codes)
    ]
    for notice in notices:
        print(f"lint ratchet: {notice}")
    if failures:
        for failure in failures:
            print(f"lint ratchet: {failure}", file=sys.stderr)
        return 1
    summary = (
        f"{sum(live.values())} diagnostics over {len(live)} keys"
        if by_file
        else dict(sorted(live.items()))
    )
    print(f"lint ratchet: {summary} within {baseline_path.name}")
    return 0


# ---------------------------------------------------------------------------
# Bazel input mode (plan_bazel_cargo_port.md C-012/C-013/C-017/C-018).
# ---------------------------------------------------------------------------

class Refusal(Exception):
    """A fail-closed cause; `run_bep` prints it and exits 1."""


def label_member(label: str) -> str | None:
    """The workspace member a Bazel label's package belongs to — `crates/<name>`
    or a subpackage of it — or `None` for any other package or repository.

    C-012 attributes a diagnostics file by the target that PRODUCED it, never
    by the span paths inside it: a span names whatever file the lint fired in,
    and a clean crate has no span at all, yet still has to count as covered."""
    repo, separator, rest = label.partition("//")
    if not separator or repo not in ("", "@", "@@"):
        return None
    parts = rest.partition(":")[0].split("/")
    return parts[1] if len(parts) > 1 and parts[0] == "crates" and parts[1] else None


def unread_lints(root: Path = REPO_ROOT) -> list[str]:
    """C-017: every `[lints]` entry Bazel does not read, named.

    rules_rust reads no Cargo `[lints]` table; the clippy aspect sees only the
    `.bazelrc` `clippy_flag` levels. The ratchet reproduces exactly one entry —
    `rust.warnings = "deny"`, because any key above its baseline fails — so
    anything else in `[workspace.lints]`, or a crate `[lints]` that is not
    `workspace = true`, would be silently ignored by the gate that claims to
    enforce it."""
    found: list[str] = []
    table = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8")).get("workspace", {}).get("lints", {})
    for tool, entries in sorted(table.items()):
        for name in sorted(entries) if isinstance(entries, dict) else [""]:
            if (tool, name) != ("rust", "warnings"):
                found.append(f"Cargo.toml: workspace.lints.{tool}.{name}".rstrip("."))
    for manifest in sorted((root / "crates").glob("*/Cargo.toml")):
        lints = tomllib.loads(manifest.read_text(encoding="utf-8")).get("lints", {})
        rel = manifest.relative_to(root).as_posix()
        for key, value in sorted(lints.items()):
            if key == "workspace" and value is True:
                continue
            names = sorted(value) if isinstance(value, dict) else [""]
            found += [f"{rel}: lints.{key}.{name}".rstrip(".") for name in names]
    return found


def code_allowed(code: str, allow_codes: Iterable[str]) -> bool:
    """C-018: whether `code` may sit in a baseline. An allow-list entry ending
    `::` is a prefix (`rustdoc::`); any other entry is an exact code."""
    return any(code == entry or (entry.endswith("::") and code.startswith(entry)) for entry in allow_codes)


def foreign_keys(keys: Iterable[str], by_file: bool, allow_codes: list[str]) -> list[str]:
    """The keys whose code is outside `allow_codes`."""
    return sorted(key for key in keys if not code_allowed(code_of(key, by_file), allow_codes))


_REMOTE_HINT = (
    "under --remote_download_minimal a cached output stays remote unless `build:ci "
    "--remote_download_regex` in .bazelrc matches it (it must cover `.clippy.diagnostics` "
    "and `.rustdoc.diagnostics`)"
)


def bep_census(bep: Path, suffix: str = DEFAULT_SUFFIX, by_file: bool = False) -> Census:
    """C-012: the census of every `*<suffix>` file this build event stream
    names, read from the output-group file sets of its `targetCompleted`
    events — the files THIS invocation built or took from cache, never a
    `bazel-bin` glob, which would read stale files and whichever configuration
    the symlink points at.

    Fails closed (`Refusal`) on: an unreadable stream, a line that is not JSON,
    no successful `buildFinished` or no `lastMessage` (the stream was cut
    short), a file set it
    references but never declares, no matching file, a named file missing on
    disk, and a diagnostics line that is not JSON. Coverage (every member owns
    a file) is `judge`'s census check, through `Census.reported`."""
    try:
        text = bep.read_text(encoding="utf-8")
    except OSError as error:
        raise Refusal(
            f"the build event stream {bep} is unreadable ({error}) — restore `--build_event_json_file` on the build"
        ) from error
    sets: dict[str, dict] = {}
    produced: list[tuple[str, str]] = []
    configured: dict[str, tuple[str, list[str]]] = {}
    succeeded = last = False
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise Refusal(f"{bep}:{number} is not JSON ({error}) — a torn or foreign build event stream") from error
        identifier = event.get("id", {})
        if "namedSet" in identifier:
            sets[str(identifier["namedSet"].get("id"))] = event.get("namedSetOfFiles", {})
        elif "targetConfigured" in identifier and "aspect" not in identifier["targetConfigured"]:
            payload = event.get("configured", {})
            configured[str(identifier["targetConfigured"].get("label", ""))] = (
                str(payload.get("targetKind", "")).removesuffix(" rule"),
                [str(tag) for tag in payload.get("tag", [])],
            )
        elif "targetCompleted" in identifier:
            label = str(identifier["targetCompleted"].get("label", ""))
            for group in event.get("completed", {}).get("outputGroup", []):
                produced += [(label, str(file_set.get("id"))) for file_set in group.get("fileSets", [])]
        elif "buildFinished" in identifier:
            succeeded = event.get("finished", {}).get("overallSuccess") is True
        last = last or event.get("lastMessage") is True
    if not succeeded or not last:
        causes = []
        if not succeeded:
            causes.append("no `buildFinished` event reporting success")
        if not last:
            causes.append("no `lastMessage` event, so the stream was cut short")
        raise Refusal(
            f"the build did not finish: {bep} carries {' and '.join(causes)}. A partial stream names "
            "only the diagnostics it reached, so every other key would read as a decrease"
        )
    files: dict[Path, str] = {}
    linted: set[str] = set()
    for label, root_set in produced:
        member = label_member(label)
        if member is None:
            continue
        pending, visited = [root_set], set()
        while pending:
            set_id = pending.pop()
            if set_id in visited:
                continue
            visited.add(set_id)
            if set_id not in sets:
                raise Refusal(f"{bep} references file set {set_id} it never declares — a truncated stream")
            pending += [str(child.get("id")) for child in sets[set_id].get("fileSets", [])]
            for entry in sets[set_id].get("files", []):
                if not str(entry.get("name", "")).endswith(suffix):
                    continue
                uri = str(entry.get("uri", ""))
                if not uri.startswith("file:"):
                    raise Refusal(f"{entry.get('name')} ({member}) is missing on disk (uri {uri!r}) — {_REMOTE_HINT}")
                files.setdefault(Path(urllib.request.url2pathname(urllib.parse.urlparse(uri).path)), member)
                linted.add(label)
    if not files:
        raise Refusal(
            f"{bep} names no diagnostics file ending in {suffix!r} under //crates — the build did not "
            "request the output group, or the aspect is not registered in .bazelrc"
        )
    # Per TARGET, not per member: a member's library owning a file says nothing
    # about its tests or binaries, and the aspect silently skips a target
    # carrying an opt-out tag — its sources would stop being linted while the
    # member still counted as covered. `manual` targets never reach this stream
    # (`//crates/...` does not expand them); that residual is named in the WP-2
    # report, not guarded here.
    aspect = ASPECTS[suffix]
    crate_targets = {label: shape for label, shape in configured.items() if label_member(label)}
    opted_out = sorted(
        f"{label} ({tag})"
        for label, (_, tags) in crate_targets.items()
        for tag in tags
        if tag.replace("-", "_").lower() in aspect.opt_out
    )
    if opted_out:
        raise Refusal(
            f"{', '.join(opted_out)} carry an opt-out tag, so {aspect.name} skips them and "
            "their sources are not linted — remove the tag; a lint exemption belongs in an "
            "`#[allow]` the suppression cap counts, not in BUILD metadata"
        )
    expected = {label for label, (kind, _) in crate_targets.items() if kind in aspect.kinds}
    if not expected:
        raise Refusal(
            f"{bep} carries no `targetConfigured` event for a //crates Rust target, so the per-target "
            "coverage check has no subject — a torn or foreign build event stream"
        )
    unlinted = sorted(expected - linted)
    if unlinted:
        raise Refusal(
            f"{len(unlinted)} //crates Rust target(s) left no diagnostics file: {', '.join(unlinted)} — "
            "the aspect did not run on them, so their sources were not linted"
        )
    messages: list[tuple[str, dict]] = []
    for path, member in sorted(files.items()):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as error:
            raise Refusal(f"{path} ({member}) is named by the build event stream but missing on disk — {_REMOTE_HINT}") from error
        for number, line in enumerate(lines, 1):
            if not line.strip():
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as error:
                raise Refusal(
                    f"{path}:{number} ({member}) is not JSON ({error}) — was it written without "
                    "`--@rules_rust//rust/settings:clippy_error_format=json`?"
                ) from error
            if isinstance(record, dict) and record.get("$message_type") == "diagnostic":
                messages.append((member, record))
    counts, records, diagnosing = tally(messages, by_file)
    return Census(counts, records, set(files.values()), diagnosing)


def run_bep(
    mode: str,
    bep: Path,
    baseline_path: Path,
    *,
    suffix: str = DEFAULT_SUFFIX,
    by_file: bool = False,
    allow_regression: bool = False,
    allow_codes: Iterable[str] = DEFAULT_ALLOW_CODES,
    root: Path = REPO_ROOT,
) -> int:
    """C-017's guard, then the BEP census, then the verdict, restricted to
    `allow_codes` (C-018)."""
    try:
        unread = unread_lints(root)
        if unread:
            raise Refusal(
                f"Bazel does not read {'; '.join(unread)} — rules_rust ignores Cargo [lints], so this "
                "gate would not enforce it. Wire it through `.bazelrc`'s "
                "`--@rules_rust//rust/settings:clippy_flag` (or `extract_cargo_lints`) and extend the "
                "ratchet's allow-list"
            )
        census = bep_census(bep, suffix, by_file)
    except Refusal as refusal:
        print(f"lint ratchet: {refusal}", file=sys.stderr)
        return 1
    return judge(mode, census, baseline_path, by_file, allow_regression, list(allow_codes))

# ---------------------------------------------------------------------------
# Proofs. Every mutation below is checked to have landed before its result is
# trusted (`scripts/tests/test_lint_ratchet.py`), the same discipline the
# module docstring asks of the ratchet itself.
# ---------------------------------------------------------------------------


def prove_compare() -> int:
    """`compare()`'s clauses: an exact match is silent, a regression and an
    absent key's implicit 0 both fail, a decrease is a notice, a zero baseline
    entry is refused, and LINT-16's double listing is refused."""
    live = Counter({"unreachable_pub": 2})
    ok, notices = compare(live, {"unreachable_pub": 2}, set())
    assert ok == [] and notices == [], f"an exact baseline must be silent: {(ok, notices)}"
    red, _ = compare(live, {"unreachable_pub": 1}, set())
    assert red == ["unreachable_pub: 2 live, baseline 1 (+1)"], f"regression not reported: {red}"
    red, _ = compare(Counter({"clippy::x": 1}), {}, set())
    assert red == ["clippy::x: 1 live, baseline 0 (+1)"], f"implicit 0 entry not applied: {red}"
    _, notice = compare(live, {"unreachable_pub": 3}, set())
    assert notice == ["unreachable_pub: dropped to 2 (baseline 3) — run `--update` and commit"], (
        f"decrease notice wrong: {notice}"
    )
    red, _ = compare(Counter(), {"unreachable_pub": 0}, set())
    assert red == [
        "unreachable_pub: baseline entry is 0 — move it into [workspace.lints] and delete the entry"
    ], f"zero entry not refused: {red}"
    red, _ = compare(live, {"unreachable_pub": 2}, {"unreachable_pub"})
    assert red == ["unreachable_pub: listed in both the baseline and [workspace.lints]"], (
        f"LINT-16 double listing not refused: {red}"
    )
    return 1


def prove_by_file() -> int:
    """`--by-file` keys each entry `<file>::<code>`, so a hit that MOVED
    between files reds even though the bare per-code census stays blind to it
    — that blindness is what `--by-file` buys out — and `code_of` / LINT-16
    both still work on the finer key."""
    messages = [
        ("ocx_util", json.loads(_diag("unreachable_pub", "crates/ocx_util/src/a.rs", 4))),
        ("ocx_util", json.loads(_diag("unreachable_pub", "crates/ocx_util/src/a.rs", 4))),
        ("ocx_util", json.loads(_diag("unreachable_pub", "crates/ocx_util/src/b.rs", 9))),
    ]
    per_file, *_ = tally(messages, by_file=True)
    assert per_file == Counter(
        {
            "crates/ocx_util/src/a.rs::unreachable_pub": 1,
            "crates/ocx_util/src/b.rs::unreachable_pub": 1,
        }
    ), f"--by-file grouping wrong: {per_file}"
    # Both hits now in a.rs: the per-code census is unchanged at 2, so the bare
    # per-code ratchet is silent. That blindness is what --by-file buys out.
    moved = Counter({"crates/ocx_util/src/a.rs::unreachable_pub": 2})
    assert compare(
        Counter({"unreachable_pub": sum(moved.values())}),
        {"unreachable_pub": sum(per_file.values())},
        set(),
    ) == ([], []), "the per-code census must be blind to the move — that is what --by-file is for"
    red, notices = compare(moved, dict(per_file), set(), by_file=True)
    assert red == ["crates/ocx_util/src/a.rs::unreachable_pub: 2 live, baseline 1 (+1)"] and notices == [
        (
            "crates/ocx_util/src/b.rs::unreachable_pub: dropped to 0 (baseline 1) — "
            "run `--update` and commit"
        )
    ], f"--by-file must red on a hit that moved between files: {(red, notices)}"
    assert (
        code_of("crates/ocx_util/src/a.rs::clippy::foo", True) == "clippy::foo"
        and code_of("clippy::foo", False) == "clippy::foo"
    ), "a `clippy::`-prefixed code must survive the file split"
    # A `--by-file` key with no `::` used to yield `""`, and `"" in enforced`
    # is always false — the LINT-16 check skipped it in silence.
    try:
        code_of("unreachable_pub", True)
    except SystemExit as refusal:
        assert "carries no `::`" in str(refusal), f"the refusal must name the cause: {refusal}"
    else:
        raise AssertionError("a --by-file key with no `::` must be refused, not read as a code-less key")
    red, _ = compare(
        Counter(), {"crates/ocx_util/src/a.rs::unreachable_pub": 1}, {"unreachable_pub"}, by_file=True
    )
    assert "listed in both the baseline and [workspace.lints]" in red[0], (
        f"LINT-16 must still fire on a --by-file key: {red}"
    )
    assert "warnings" in workspace_lints(), f"[workspace.lints] not read: {workspace_lints()}"
    return 1


def prove_raised() -> int:
    """`raised()` is the same upward-move rule `compare` applies on --check,
    kept as its own function so --update's direction gate cannot drift from
    it."""
    assert raised(Counter({"a": 1}), {"a": 2}) == [] and raised(Counter(), {"a": 2}) == [], (
        "a downward move must not read as a raise"
    )
    assert raised(Counter({"a": 3}), {"a": 2}) == ["a: 3 live, baseline 2 (+1)"], (
        f"an upward move must be named: {raised(Counter({'a': 3}), {'a': 2})}"
    )
    assert raised(Counter({"a": 1}), {}) == ["a: 1 live, baseline 0 (+1)"], (
        "a key absent from the baseline rises from an implicit 0"
    )
    return 1


def prove_workspace_members() -> int:
    """The member set is read off the manifest and the filesystem; a silent
    shrink would make the coverage gate easier to satisfy, which is the
    direction that never reds."""
    members = workspace_members()
    assert len(members) > 1 and {"ocx_util", "ocx_cli"} <= members, (
        f"workspace_members() looks wrong: {sorted(members)}"
    )
    assert all((REPO_ROOT / "crates" / name / "Cargo.toml").is_file() for name in members), (
        "every member the census names must be a crate directory that exists"
    )
    return 1


def prove_bep_update_gate(tmp_path: Path) -> int:
    """--update refuses a raise without --allow-regression, accepts a decrease
    unflagged on both the per-code and --by-file paths — identical, which is
    the whole of "both ratchets behave the same" — and creating a baseline
    that does not exist yet is itself an upward move."""
    b_rs = "crates/ocx_util/src/b.rs"
    two = [_diag("unreachable_pub", _UTIL_A, 4), _diag("unreachable_pub", b_rs, 9)]
    stream = _bep_fixture(tmp_path / "two", {"ocx_util": two})
    raising = _bep_fixture(tmp_path / "three", {"ocx_util": [*two, _diag("unreachable_pub", "crates/ocx_util/src/c.rs")]})
    path = tmp_path / "baseline.json"
    path.write_text(json.dumps({"unreachable_pub": 2}) + "\n", encoding="utf-8")
    before = path.read_text(encoding="utf-8")
    assert run_bep("update", raising, path) == 1, "--update must refuse a raise"
    assert path.read_text(encoding="utf-8") == before, (
        "the refusal must leave the baseline byte-identical — a message is not a gate"
    )
    assert run_bep("update", raising, path, allow_regression=True) == 0, "--allow-regression must let the raise through"
    assert json.loads(path.read_text(encoding="utf-8")) == {"unreachable_pub": 3}, (
        f"the raised baseline was not written: {path.read_text(encoding='utf-8')}"
    )
    # A downward move still needs no flag: locking in an improvement is the
    # point of the ratchet.
    assert run_bep("update", stream, path) == 0, "--update must accept a decrease unflagged"
    assert json.loads(path.read_text(encoding="utf-8")) == {"unreachable_pub": 2}, "the decrease was not written"
    # Identical on the `--by-file` path both tasks drive.
    per_file_path = tmp_path / "by-file.json"
    per_file_path.write_text(json.dumps({f"{_UTIL_A}::unreachable_pub": 1}) + "\n", encoding="utf-8")
    before = per_file_path.read_text(encoding="utf-8")
    assert run_bep("update", stream, per_file_path, by_file=True) == 1, (
        "--by-file --update must refuse a raise too (b.rs is new to this baseline)"
    )
    assert per_file_path.read_text(encoding="utf-8") == before, "--by-file refusal must leave the baseline byte-identical"
    assert run_bep("update", stream, per_file_path, by_file=True, allow_regression=True) == 0, (
        "--by-file --allow-regression must let the raise through"
    )
    # A baseline that does not exist yet reads as `{}`, so creating one is
    # itself a raise and takes the flag once.
    fresh = tmp_path / "absent.json"
    assert run_bep("update", stream, fresh) == 1, "creating a baseline is an upward move"
    assert not fresh.exists(), "the refusal must not create the file"
    assert run_bep("update", stream, fresh, allow_regression=True) == 0, "the flag creates it"
    return 1

def _diag(code: str | None, file: str | None, line: int = 1, level: str = "warning", text: str | None = None) -> str:
    """A rustc-native diagnostic line, as the clippy aspect writes it. `file`
    None is a spanless record — rustc's "N warnings emitted" summary shape."""
    spans = (
        [{"is_primary": True, "file_name": file, "line_start": line, "column_start": 1}] if file else []
    )
    return json.dumps(
        {
            "$message_type": "diagnostic",
            "message": text if text is not None else f"{code} at {file}:{line}",
            "code": {"code": code, "explanation": None} if code else None,
            "level": level,
            "spans": spans,
            "children": [],
            "rendered": f"{level}: {code}",
        }
    )


_ARTIFACT = json.dumps({"$message_type": "artifact", "artifact": "x.rmeta", "emit": "metadata"})


def _bep_fixture(
    tmp_path: Path,
    records: dict[str, list[str]] | None = None,
    *,
    members: set[str] | None = None,
    finished: bool | None = True,
    last_message: bool = True,
    absent: frozenset[str] = frozenset(),
    suffix: str = DEFAULT_SUFFIX,
    nested: bool = True,
    configured: bool = True,
    tags: dict[str, list[str]] | None = None,
    extra_targets: tuple[tuple[str, str], ...] = (),
) -> Path:
    """A BEP naming one `<member><suffix>` per member (default: every workspace
    member), each holding an artifact notification plus `records[member]`.
    `finished` None drops `buildFinished`, False reports it failed; a member in
    `absent` is named by the BEP but never written to disk. `nested` puts every
    file set behind a parent set, the transitive shape Bazel emits for deps.
    Each member library gets a `targetConfigured` event (`configured` False
    drops them all), carrying `tags[member]`; `extra_targets` adds configured
    `(label, kind)` targets that own no diagnostics file."""
    records = records or {}
    members = workspace_members() if members is None else members
    out = tmp_path / "bazel-bin"
    out.mkdir(parents=True, exist_ok=True)
    events: list[dict] = []
    for label, kind in extra_targets:
        events.append({"id": {"targetConfigured": {"label": label}}, "configured": {"targetKind": f"{kind} rule"}})
    for index, member in enumerate(sorted(members)):
        if configured:
            payload: dict = {"targetKind": "rust_library rule"}
            if (tags or {}).get(member):
                payload["tag"] = tags[member]
            events.append({"id": {"targetConfigured": {"label": f"//crates/{member}:{member}"}}, "configured": payload})
        path = out / f"{member}{suffix}"
        if member not in absent:
            path.write_text("\n".join([_ARTIFACT, *records.get(member, [])]) + "\n", encoding="utf-8")
        leaf, parent = f"{index}f", f"{index}p"
        events.append(
            {
                "id": {"namedSet": {"id": leaf}},
                "namedSetOfFiles": {"files": [{"name": path.name, "uri": path.as_uri()}]},
            }
        )
        if nested:
            events.append({"id": {"namedSet": {"id": parent}}, "namedSetOfFiles": {"fileSets": [{"id": leaf}]}})
        events.append(
            {
                "id": {
                    "targetCompleted": {
                        "label": f"//crates/{member}:{member}",
                        "aspect": "@@rules_rust+//rust/private:clippy.bzl%rust_clippy_aspect",
                    }
                },
                "completed": {
                    "success": True,
                    "outputGroup": [{"name": "clippy_output", "fileSets": [{"id": parent if nested else leaf}]}],
                },
            }
        )
    if finished is not None:
        events.append(
            {"id": {"buildFinished": {}}, "finished": {"overallSuccess": finished, "exitCode": {"name": "SUCCESS" if finished else "BUILD_FAILURE"}}}
        )
    last = {"id": {"buildMetrics": {}}, "buildMetrics": {}}
    if last_message:
        last["lastMessage"] = True
    events.append(last)
    bep = tmp_path / "bep.json"
    bep.write_text("".join(json.dumps(event) + "\n" for event in events), encoding="utf-8")
    return bep


_UTIL_A = "crates/ocx_util/src/a.rs"


def _refusal(bep: Path, *causes: str, suffix: str = DEFAULT_SUFFIX) -> None:
    """`bep_census` must refuse `bep`, naming every one of `causes`."""
    try:
        bep_census(bep, suffix, by_file=True)
    except Refusal as refusal:
        for cause in causes:
            assert cause in str(refusal), f"the refusal must name {cause!r}: {refusal}"
    else:
        raise AssertionError(f"{bep} must be refused ({causes})")


def prove_label_member() -> int:
    """The member is the Bazel package's `crates/<name>`, never a span path."""
    assert label_member("//crates/ocx_util:ocx_util") == "ocx_util"
    assert label_member("@@//crates/ocx_util:ocx_util_test") == "ocx_util"
    assert label_member("@//crates/ocx_cli:ocx") == "ocx_cli"
    assert label_member("//crates/ocx_cli/sub:x") == "ocx_cli", "a subpackage belongs to its member"
    for outside in ("//test:foo", "//:root", "@@rules_rust+//rust:x", "//crates:x", "crates/ocx_util:x"):
        assert label_member(outside) is None, f"{outside} names no member"
    return 1


def prove_bep_fail_closed(tmp_path: Path) -> int:
    """C-012: every fail-closed cause is refused and named, and the same
    fixture with the cause removed passes — else the red says nothing."""
    whole = _bep_fixture(tmp_path / "whole", {"ocx_util": [_diag("unreachable_pub", _UTIL_A)]})
    census = bep_census(whole, by_file=True)
    assert census.counts == Counter({f"{_UTIL_A}::unreachable_pub": 1}), f"green fixture wrong: {census}"
    assert census.reported == workspace_members(), "every member must own a file"
    _refusal(tmp_path / "no-such.json", "unreadable")
    garbage = tmp_path / "garbage.json"
    garbage.write_text(whole.read_text(encoding="utf-8") + "not json\n", encoding="utf-8")
    _refusal(garbage, "not JSON")
    _refusal(whole, "no diagnostics file", suffix=".rustdoc.diagnostics")
    _refusal(_bep_fixture(tmp_path / "absent", absent=frozenset({"ocx_util"})), "ocx_util", "missing on disk", "remote_download")
    torn = _bep_fixture(tmp_path / "torn", {"ocx_util": ["{not json"]})
    _refusal(torn, "ocx_util", "not JSON")
    _refusal(_bep_fixture(tmp_path / "unfinished", finished=None), "did not finish")
    _refusal(_bep_fixture(tmp_path / "failed", finished=False), "did not finish")
    _refusal(_bep_fixture(tmp_path / "no-last", last_message=False), "lastMessage")
    flat = _bep_fixture(tmp_path / "flat", {"ocx_util": [_diag("unreachable_pub", _UTIL_A)]}, nested=False)
    assert bep_census(flat, by_file=True).counts == census.counts, "a flat file set must read the same"
    # End to end: `run_bep` turns each refusal into exit 1 and a clean run into 0.
    baseline = tmp_path / "baseline.json"
    payload = json.dumps({f"{_UTIL_A}::unreachable_pub": 1}) + "\n"
    baseline.write_text(payload, encoding="utf-8")
    for mode in ("check", "update"):
        assert run_bep(mode, tmp_path / "unfinished" / "bep.json", baseline, by_file=True) == 1
    assert baseline.read_text(encoding="utf-8") == payload, "a refusal must leave the baseline byte-identical"
    assert run_bep("check", whole, baseline, by_file=True) == 0, "the whole fixture must pass"
    return 1


def prove_bep_gate_semantics(tmp_path: Path) -> int:
    """C-013: a key at its baseline count passes, a new code reds, a spanned
    uncoded warning reds as `<file>::<uncoded>`, the spanless summary is
    ignored, and the lib/test duplicate is one key."""
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps({f"{_UTIL_A}::unreachable_pub": 1}) + "\n", encoding="utf-8")
    at_count = [
        _diag("unreachable_pub", _UTIL_A, 4),
        _diag(None, None, text="2 warnings emitted"),
        _diag(None, None, text="1 warning emitted"),
    ]
    green = _bep_fixture(tmp_path / "green", {"ocx_util": at_count, "ocx_cli": [_diag("unreachable_pub", _UTIL_A, 4)]})
    assert bep_census(green, by_file=True).counts == Counter({f"{_UTIL_A}::unreachable_pub": 1}), (
        "the summary must be ignored and the duplicate span counted once"
    )
    assert run_bep("check", green, baseline, by_file=True) == 0, "a key at its count must pass"
    new_code = _bep_fixture(tmp_path / "new", {"ocx_util": [*at_count, _diag("dead_code", _UTIL_A, 9)]})
    assert run_bep("check", new_code, baseline, by_file=True) == 1, "a new code must red"
    uncoded = _bep_fixture(tmp_path / "uncoded", {"ocx_util": [*at_count, _diag(None, _UTIL_A, 7)]})
    assert bep_census(uncoded, by_file=True).counts[f"{_UTIL_A}::{UNCODED}"] == 1, "uncoded must be keyed"
    assert run_bep("check", uncoded, baseline, by_file=True) == 1, "a spanned uncoded warning must red"
    # A span-less, code-less warning that is NOT rustc's summary is a real
    # warning of that shape, keyed rather than dropped.
    bare = _bep_fixture(tmp_path / "bare", {"ocx_util": [*at_count, _diag(None, None, text="some lint fired")]})
    assert bep_census(bare, by_file=True).counts[f"<no span>::{UNCODED}"] == 1, "span-less uncoded must be keyed"
    assert run_bep("check", bare, baseline, by_file=True) == 1, "a span-less non-summary warning must red"
    raised_count = _bep_fixture(tmp_path / "raised", {"ocx_util": [*at_count, _diag("unreachable_pub", _UTIL_A, 8)]})
    assert run_bep("check", raised_count, baseline, by_file=True) == 1, "a key above its count must red"
    # Every member's file holding only rustc's summary is a stream that linted
    # nothing: both modes refuse it and the baseline is untouched (B5R-2).
    summary_only = _bep_fixture(tmp_path / "summary", {"ocx_util": at_count[1:]})
    payload = baseline.read_text(encoding="utf-8")
    for mode in ("check", "update"):
        assert run_bep(mode, summary_only, baseline, by_file=True) == 1, f"--{mode} must refuse a summary-only stream"
    assert baseline.read_text(encoding="utf-8") == payload, "the refusal must leave the baseline byte-identical"
    return 1


def prove_bep_dark_member(tmp_path: Path) -> int:
    """A member the baseline attributes keys to that produced no diagnostic
    reds; a member that owns no diagnostics file at all reds on coverage."""
    baseline = tmp_path / "baseline.json"
    console = "crates/ocx_console/src/z.rs"
    payload = json.dumps({f"{_UTIL_A}::unreachable_pub": 1, f"{console}::unreachable_pub": 1}) + "\n"
    baseline.write_text(payload, encoding="utf-8")
    util = [_diag("unreachable_pub", _UTIL_A)]
    dark = _bep_fixture(tmp_path / "dark", {"ocx_util": util})
    for mode in ("check", "update"):
        assert run_bep(mode, dark, baseline, by_file=True) == 1, f"--{mode} must refuse a dark member"
    assert baseline.read_text(encoding="utf-8") == payload, "the refusal must leave the baseline byte-identical"
    speaking = _bep_fixture(tmp_path / "speaking", {"ocx_util": util, "ocx_console": [_diag("unreachable_pub", console)]})
    assert run_bep("check", speaking, baseline, by_file=True) == 0, "no member is quiet, so it passes"
    uncovered = _bep_fixture(
        tmp_path / "uncovered",
        {"ocx_util": util, "ocx_console": [_diag("unreachable_pub", console)]},
        members=workspace_members() - {"ocx_exit"},
    )
    assert run_bep("check", uncovered, baseline, by_file=True) == 1, "a member with no diagnostics file must red"
    # A per-code baseline attributes nothing, so the quiet-member rule cannot
    # fire on it and refuse a run it knows nothing about.
    per_code = tmp_path / "per-code.json"
    per_code.write_text(json.dumps({"unreachable_pub": 1}) + "\n", encoding="utf-8")
    assert run_bep("check", dark, per_code) == 0, "a per-code baseline names no member, so the rule must not fire"
    assert (
        member_of(f"{_UTIL_A}::unreachable_pub") == "ocx_util"
        and member_of("unreachable_pub") is None
        and member_of("external/fork/src/x.rs::dead_code") is None
    ), "member_of must read a member out of a by-file key and out of nothing else"
    return 1


def prove_bep_target_coverage(tmp_path: Path) -> int:
    """Per-target coverage: a //crates target carrying a clippy opt-out tag
    (any spelling the aspect normalises) reds; a configured Rust target that
    left no diagnostics file reds; a non-Rust target and a stream with the
    configured events present pass; a stream with none reds on its floor."""
    util = {"ocx_util": [_diag("unreachable_pub", _UTIL_A)]}
    assert bep_census(_bep_fixture(tmp_path / "green", util), by_file=True).counts, "the green fixture reads"
    for tag in ("no-clippy", "NoLint", "noclippy", "no_lint"):
        _refusal(_bep_fixture(tmp_path / f"opt-{tag}", util, tags={"ocx_exit": [tag]}), "//crates/ocx_exit:ocx_exit", tag)
    benign = _bep_fixture(tmp_path / "benign", util, tags={"ocx_exit": ["manual-review", "no-sandbox"]})
    assert bep_census(benign, by_file=True).counts, "an unrelated tag must pass"
    _refusal(
        _bep_fixture(tmp_path / "unlinted", util, extra_targets=(("//crates/ocx_util:ocx_util_test", "rust_test"),)),
        "//crates/ocx_util:ocx_util_test",
        "no diagnostics file",
    )
    other = _bep_fixture(tmp_path / "other", util, extra_targets=(("//crates/ocx_schema:schemas", "genrule"),))
    assert bep_census(other, by_file=True).counts, "a non-Rust target owes no diagnostics file"
    _refusal(_bep_fixture(tmp_path / "no-configured", util, configured=False), "targetConfigured")
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps({f"{_UTIL_A}::unreachable_pub": 1}) + "\n", encoding="utf-8")
    assert run_bep("check", tmp_path / "opt-no-clippy" / "bep.json", baseline, by_file=True) == 1
    assert run_bep("check", tmp_path / "green" / "bep.json", baseline, by_file=True) == 0
    # Scoped per suffix: rustdoc documents no test target and honours no tag,
    # so a test with no `.rustdoc.diagnostics` and a clippy opt-out tag both
    # pass there — while a binary with no file still reds.
    doc = ".rustdoc.diagnostics"
    test = (("//crates/ocx_util:ocx_util_test", "rust_test"),)
    assert bep_census(_bep_fixture(tmp_path / "doc-test", util, suffix=doc, extra_targets=test), doc, by_file=True).counts
    assert bep_census(_bep_fixture(tmp_path / "doc-tag", util, suffix=doc, tags={"ocx_exit": ["no-clippy"]}), doc).counts
    binary = (("//crates/ocx_cli:ocx", "rust_binary"),)
    _refusal(_bep_fixture(tmp_path / "doc-bin", util, suffix=doc, extra_targets=binary), "//crates/ocx_cli:ocx", suffix=doc)
    return 1


def _lint_tree(tmp_path: Path, root_lints: str, crate_lints: str) -> Path:
    (tmp_path / "crates" / "a").mkdir(parents=True)
    (tmp_path / "Cargo.toml").write_text(f'[workspace]\nmembers = ["crates/*"]\n{root_lints}', encoding="utf-8")
    (tmp_path / "crates" / "a" / "Cargo.toml").write_text(f'[package]\nname = "a"\n{crate_lints}', encoding="utf-8")
    return tmp_path


def prove_unread_lints(tmp_path: Path) -> int:
    """C-017: a `[workspace.lints]` entry beyond `rust.warnings`, or a crate
    `[lints]` beyond `workspace = true`, is named as unread by Bazel."""
    clean_root = '[workspace.lints.rust]\nwarnings = "deny"\n[workspace.lints.clippy]\n'
    clean_crate = "[lints]\nworkspace = true\n"
    assert unread_lints(_lint_tree(tmp_path / "clean", clean_root, clean_crate)) == [], "the live shape passes"
    assert unread_lints() == [], "this repository's manifests must pass"
    root = unread_lints(_lint_tree(tmp_path / "root", clean_root + 'pedantic = "warn"\n', clean_crate))
    assert root and "workspace.lints.clippy.pedantic" in root[0], f"root entry not named: {root}"
    crate = unread_lints(_lint_tree(tmp_path / "crate", clean_root, clean_crate + '[lints.rust]\nx = "warn"\n'))
    assert crate and "crates/a/Cargo.toml" in crate[0] and "lints.rust" in crate[0], f"crate entry not named: {crate}"
    bep = _bep_fixture(tmp_path / "bep", {"ocx_util": [_diag("unreachable_pub", _UTIL_A)]})
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps({f"{_UTIL_A}::unreachable_pub": 1}) + "\n", encoding="utf-8")
    assert run_bep("check", bep, baseline, by_file=True, root=tmp_path / "root") == 1, "root entry must red"
    assert run_bep("check", bep, baseline, by_file=True, root=tmp_path / "crate") == 1, "crate entry must red"
    assert run_bep("check", bep, baseline, by_file=True, root=tmp_path / "clean") == 0, "clean tree passes"
    return 1


def prove_allow_codes(tmp_path: Path) -> int:
    """C-018: an update that would write a code outside the allow-list is
    refused even with --allow-regression; a baseline already parking one reds."""
    assert code_allowed("unreachable_pub", ["unreachable_pub"]) and not code_allowed("dead_code", ["unreachable_pub"])
    assert code_allowed("rustdoc::broken_intra_doc_links", ["rustdoc::"]), "`::` suffix is a prefix"
    assert not code_allowed("rustdoc_x", ["rustdoc::"]) and not code_allowed(UNCODED, ["unreachable_pub"])
    baseline = tmp_path / "baseline.json"
    payload = json.dumps({f"{_UTIL_A}::unreachable_pub": 1}) + "\n"
    baseline.write_text(payload, encoding="utf-8")
    parked = _bep_fixture(tmp_path / "parked", {"ocx_util": [_diag("unreachable_pub", _UTIL_A), _diag("dead_code", _UTIL_A, 3)]})
    assert run_bep("update", parked, baseline, by_file=True, allow_regression=True) == 1, (
        "--allow-regression must not park a code outside the allow-list"
    )
    assert baseline.read_text(encoding="utf-8") == payload, "the refusal must leave the baseline byte-identical"
    allowed = _bep_fixture(
        tmp_path / "allowed", {"ocx_util": [_diag("unreachable_pub", _UTIL_A), _diag("unreachable_pub", _UTIL_A, 3)]}
    )
    assert run_bep("update", allowed, baseline, by_file=True, allow_regression=True) == 0, "an allowed raise writes"
    assert json.loads(baseline.read_text(encoding="utf-8")) == {f"{_UTIL_A}::unreachable_pub": 2}
    baseline.write_text(json.dumps({f"{_UTIL_A}::unreachable_pub": 2, f"{_UTIL_A}::dead_code": 1}) + "\n", encoding="utf-8")
    assert run_bep("check", parked, baseline, by_file=True) == 1, "a baseline parking a foreign code must red"
    return 1



def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--check", action="store_true", help="fail on any code above its baseline entry"
    )
    mode.add_argument(
        "--update", action="store_true", help="rewrite the baseline from the stream"
    )
    parser.add_argument(
        "--bep",
        type=Path,
        required=True,
        help="a Bazel build event stream (--build_event_json_file); reads the diagnostics files it names",
    )
    parser.add_argument(
        "--suffix",
        default=DEFAULT_SUFFIX,
        choices=sorted(ASPECTS),
        help=f"the diagnostics file suffix (default {DEFAULT_SUFFIX})",
    )
    parser.add_argument(
        "--allow-codes",
        default=",".join(DEFAULT_ALLOW_CODES),
        help="comma-separated codes a baseline may hold; an entry ending `::` is a prefix",
    )
    parser.add_argument("--baseline", type=Path, default=BASELINE)
    parser.add_argument(
        "--by-file",
        action="store_true",
        help="key each entry `<file>::<code>` instead of `<code>` alone",
    )
    parser.add_argument(
        "--allow-regression",
        action="store_true",
        help="let --update raise an entry; prints every key it raises",
    )
    args = parser.parse_args()
    return run_bep(
        "update" if args.update else "check",
        args.bep,
        args.baseline,
        suffix=args.suffix,
        by_file=args.by_file,
        allow_regression=args.allow_regression,
        allow_codes=[code for code in args.allow_codes.split(",") if code],
    )


if __name__ == "__main__":
    raise SystemExit(main())
