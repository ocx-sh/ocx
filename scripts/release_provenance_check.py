#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Refuse to publish an `ocx` binary that carries test provenance (ADR C-PROV).

    scripts/release_provenance_check.py --scan <file>... [--min-files N]
    scripts/release_provenance_check.py --exec <binary>
    scripts/release_provenance_check.py --link <file>... [--min-files N]

Test builds (`--features ocx/__testing`) bake fixed placeholder provenance into
the binary, so the acceptance binary is byte-identical across commits. That
binary must never ship. Two checks keep it out:

`--scan` reads every named file's bytes for the three string markers the
placeholder table plants (`placeholder-g00000000`, `ci.invalid`,
`placeholder/placeholder`). It runs on every target's binary, including the
ones the runner cannot execute. Each hit prints the file, the marker and its
byte offset. The all-zero SHA is deliberately NOT a marker: dependency code
already carries 40-zero runs (gix's null object id), so it would red every
real release. `--min-files` is the reader floor: reading fewer files than
there are release targets is a red, because a scan over nothing is clean.

`--exec` runs `<binary> --format json version` — only ever on a binary native
to the runner — and requires release provenance: `channel` is not `test`, a
40-hex non-zero `commit.sha`, `commit.dirty` false, a `ci.run_url` under
`https://github.com/ocx-sh/ocx/actions/runs/<digits>`, and a non-epoch
`build.timestamp`. It can only be green in CI, where a run URL exists.

`--link` is the hermetic-link gate for the shipped Linux binaries. It runs
`readelf` over each file (binutils on PATH; absent is exit 2, never a pass) and
requires every `GLIBC_x.y` version it needs to be at or below `GLIBC_FLOOR` and
every `NEEDED` library to be on `NEEDED_ALLOWED`. A static musl binary has
neither and is clean. `--min-files` floors the number of binaries read.

Its proofs run as pytest, from `scripts/tests/test_release_provenance_check.py`,
and show every red on synthetic fixtures; their greens exercise the parser only
(ADR AM-2): the green on a real release build — the one proof that dependency
bytes stay clear of the markers — is `task release:provenance:proof`, a step of
`task release:gate`. `scan-binaries.yml` runs `--scan` and `--exec` over the binaries
that `release.yml` and `deploy-dev.yml` are about to publish, and `--link` over the
cargo-dist Linux archives; no deep or pull-request job scans, because the
Bazel-built `ocx` carries the placeholders by design.

Exit codes: 0 clean, 1 finding, 2 usage (bad arguments, unreadable file, no readelf).
Stdlib only.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

MARKERS: tuple[bytes, ...] = (b"placeholder-g00000000", b"ci.invalid", b"placeholder/placeholder")
RUN_URL = re.compile(r"^https://github\.com/ocx-sh/ocx/actions/runs/\d+$")
SHA = re.compile(r"^[0-9a-f]{40}$")
EPOCH_PREFIX = "1970-01-01T00:00:00"
EXEC_TIMEOUT_SECONDS = 60

# The `-gnu` targets build on ubuntu-22.04 runners (glibc 2.35, no zigbuild); v0.6.5's x86_64 and aarch64
# binaries both top out at GLIBC_2.34. Raising the floor ships a binary that stops starting on older distros.
GLIBC_FLOOR = (2, 34)
# A new NEEDED soname is a new runtime dependency on every host: v0.6.5 links these three, and its
# aarch64 build also lists its loader. The x86_64 loader is allowed for when a toolchain lists it too.
NEEDED_ALLOWED = frozenset(
    {"libc.so.6", "libm.so.6", "libgcc_s.so.1", "ld-linux-x86-64.so.2", "ld-linux-aarch64.so.1"}
)
READELF = "readelf"
NEEDED_LINE = re.compile(r"\(NEEDED\)\s+Shared library: \[([^\]]+)\]")
VERSION_NEED = re.compile(r"\bName: (GLIBC_\S+)")
GLIBC_VERSION = re.compile(r"GLIBC_(\d+(?:\.\d+)*)")

EXIT_CLEAN, EXIT_FINDING, EXIT_USAGE = 0, 1, 2


class UsageError(Exception):
    """A file that cannot be read, or arguments that cannot mean a check."""


def scan(paths: list[Path]) -> list[str]:
    """One line per marker occurrence across `paths`: `<file>: marker '<m>' at byte offset <n>`."""
    findings: list[str] = []
    for path in paths:
        try:
            data = path.read_bytes()
        except OSError as error:
            raise UsageError(f"cannot read {path}: {error}") from error
        for marker in MARKERS:
            offset = data.find(marker)
            while offset != -1:
                findings.append(f"{path}: marker '{marker.decode()}' at byte offset {offset}")
                offset = data.find(marker, offset + 1)
    return findings


def evaluate_report(report: object) -> list[str]:
    """Every reason the parsed `ocx --format json version` report is not a release build."""
    if not isinstance(report, dict):
        return [f"the version report is not a JSON object: {report!r}"]

    def field(*path: str) -> object:
        node: object = report
        for key in path:
            if not isinstance(node, dict) or key not in node:
                return None
            node = node[key]
        return node

    findings: list[str] = []
    # A release build carries no `channel` at all (v0.6.2 reports none); a dev
    # build says `dev`. Only the test placeholder says `test`.
    if field("channel") == "test":
        findings.append("channel is 'test' — a `__testing` build")
    sha = field("commit", "sha")
    if not isinstance(sha, str) or not SHA.fullmatch(sha) or set(sha) == {"0"}:
        findings.append(f"commit.sha is {sha!r}, not a 40-hex non-zero commit id")
    dirty = field("commit", "dirty")
    if dirty is not False:
        findings.append(f"commit.dirty is {dirty!r}, not false")
    run_url = field("ci", "run_url")
    if not isinstance(run_url, str) or not RUN_URL.fullmatch(run_url):
        findings.append(f"ci.run_url is {run_url!r}, not a run of github.com/ocx-sh/ocx")
    timestamp = field("build", "timestamp")
    if not isinstance(timestamp, str) or not timestamp or timestamp.startswith(EPOCH_PREFIX):
        findings.append(f"build.timestamp is {timestamp!r}, not a real build time")
    return findings


def check_exec(binary: Path) -> list[str]:
    """Run `<binary> --format json version` and evaluate its report."""
    if not binary.is_file():
        raise UsageError(f"cannot execute {binary}: not a file")
    try:
        result = subprocess.run(
            [str(binary), "--format", "json", "version"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=EXEC_TIMEOUT_SECONDS,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        return [f"{binary} --format json version did not run: {error}"]
    if result.returncode != 0:
        return [f"{binary} --format json version exited {result.returncode}: {result.stderr.strip()}"]
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        return [f"{binary} --format json version printed no JSON ({error}): {result.stdout[:200]!r}"]
    return [f"{binary}: {finding}" for finding in evaluate_report(report)]


def _dotted(version: tuple[int, ...]) -> str:
    return ".".join(map(str, version))


def evaluate_readelf(text: str) -> list[str]:
    """Every reason the `readelf --version-info --dynamic` output of a Linux ELF breaks the link gate."""
    # A static binary prints one of these two; any other output means the reader saw nothing.
    if "Dynamic section at offset" not in text and "There is no dynamic section" not in text:
        return ["readelf printed no dynamic-section verdict — nothing was read"]
    findings: list[str] = []
    needed = NEEDED_LINE.findall(text)
    for soname in needed:
        if soname not in NEEDED_ALLOWED:
            findings.append(f"NEEDED {soname} is not on the allow-list")
    # `Name:` also heads a version-definition section; only the needs section says what this binary demands.
    _, _, needs = text.partition("Version needs section")
    names = VERSION_NEED.findall(needs)
    if "libc.so.6" in needed and not names:
        findings.append("libc.so.6 is NEEDED but readelf listed no GLIBC_ version — the reader parsed nothing")
    over: set[tuple[int, ...]] = set()
    for name in names:
        match = GLIBC_VERSION.fullmatch(name)
        if match is None:
            findings.append(f"needs {name}, a glibc-internal version")
        elif (version := tuple(map(int, match[1].split(".")))) > GLIBC_FLOOR:
            over.add(version)
    if over:
        listed = ", ".join(f"GLIBC_{_dotted(v)}" for v in sorted(over))
        findings.append(f"needs {listed}, above the floor GLIBC_{_dotted(GLIBC_FLOOR)}")
    return findings


def check_link(binary: Path) -> list[str]:
    """Run `readelf` over `binary` and evaluate its NEEDED libraries and glibc version needs."""
    try:
        result = subprocess.run(
            [READELF, "-W", "--version-info", "--dynamic", str(binary)],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            env={**os.environ, "LC_ALL": "C"},  # readelf translates its headings; the parser reads the C ones
            timeout=EXEC_TIMEOUT_SECONDS,
            check=False,
        )
    except FileNotFoundError as error:
        raise UsageError(f"cannot run {READELF} (binutils): {error}") from error
    except (OSError, subprocess.TimeoutExpired) as error:
        return [f"{binary}: {READELF} did not run: {error}"]
    if result.returncode != 0:
        return [f"{binary}: {READELF} exited {result.returncode}: {result.stderr.strip()}"]
    return [f"{binary}: {finding}" for finding in evaluate_readelf(result.stdout)]


# --------------------------------------------------------------------------
# self-test
# --------------------------------------------------------------------------

# A report shaped like a real release build's (v0.6.2's, fields renamed only
# where they carry real run identifiers). Every red below is this report with
# one field broken, so each red is attributable to that one field.
_GOOD_REPORT: dict = {
    "version": "0.6.2",
    "commit": {
        "sha": "3538b755f55cefddced47cca6b8cf963315f91c9",
        "short": "3538b755",
        "describe": "v0.6.2",
        "dirty": False,
        "timestamp": "2026-09-15T22:41:39.000000000Z",
    },
    "build": {
        "timestamp": "2026-09-15T22:46:14.830537970Z",
        "profile": "release",
        "target": "x86_64-unknown-linux-musl",
        "rustc": "1.95.0",
    },
    "ci": {
        "provider": "github-actions",
        "run_url": "https://github.com/ocx-sh/ocx/actions/runs/35032267742",
        "workflow": "Release",
        "ref": "refs/tags/v0.6.2",
        "sha": "3538b755f55cefddced47cca6b8cf963315f91c9",
    },
}


def _broken(path: tuple[str, ...], value: object) -> dict:
    """`_GOOD_REPORT` with the field at `path` set to `value` (`_DELETE` removes it)."""
    report = json.loads(json.dumps(_GOOD_REPORT))
    node = report
    for key in path[:-1]:
        node = node[key]
    if value is _DELETE:
        del node[path[-1]]
    else:
        node[path[-1]] = value
    return report


_DELETE = object()

# (name, report, substring the finding must carry). The substring pins the
# red to the field it is about: a report broken in `dirty` that reds only
# because some other predicate misfired is a red for the wrong reason.
_REPORT_REDS: list[tuple[str, object, str]] = [
    ("channel test", _broken(("channel",), "test"), "channel"),
    ("ci missing", _broken(("ci",), _DELETE), "ci.run_url"),
    ("zero sha", _broken(("commit", "sha"), "0" * 40), "commit.sha"),
    ("short sha", _broken(("commit", "sha"), "3538b755"), "commit.sha"),
    ("non-hex sha", _broken(("commit", "sha"), "Z" * 40), "commit.sha"),
    # `re.match` with `$` accepts a trailing newline; only `fullmatch` refuses it.
    ("sha with newline", _broken(("commit", "sha"), "3538b755f55cefddced47cca6b8cf963315f91c9\n"), "commit.sha"),
    ("commit missing", _broken(("commit",), _DELETE), "commit.sha"),
    ("dirty true", _broken(("commit", "dirty"), True), "commit.dirty"),
    ("dirty missing", _broken(("commit", "dirty"), _DELETE), "commit.dirty"),
    ("dirty as string", _broken(("commit", "dirty"), "false"), "commit.dirty"),
    (
        "foreign run_url",
        _broken(("ci", "run_url"), "https://github.com/evil/ocx/actions/runs/1"),
        "ci.run_url",
    ),
    (
        "placeholder run_url",
        _broken(("ci", "run_url"), "https://ci.invalid/placeholder/placeholder/actions/runs/0"),
        "ci.run_url",
    ),
    (
        "run_url with suffix",
        _broken(("ci", "run_url"), "https://github.com/ocx-sh/ocx/actions/runs/1/attempts/2"),
        "ci.run_url",
    ),
    (
        "run_url with newline",
        _broken(("ci", "run_url"), "https://github.com/ocx-sh/ocx/actions/runs/1\n"),
        "ci.run_url",
    ),
    ("epoch timestamp", _broken(("build", "timestamp"), "1970-01-01T00:00:00.000000000Z"), "build.timestamp"),
    ("timestamp missing", _broken(("build", "timestamp"), _DELETE), "build.timestamp"),
    ("not an object", ["not", "a", "report"], "JSON object"),
]

# `readelf -W --version-info --dynamic` of v0.6.5's x86_64-unknown-linux-gnu `ocx`, trimmed to the
# lines the parser reads. GLIBC_2.9 sits below the floor numerically but above it as a string.
_GOOD_READELF = """
Dynamic section at offset 0x19388d8 contains 29 entries:
  Tag        Type                         Name/Value
 0x0000000000000001 (NEEDED)             Shared library: [libgcc_s.so.1]
 0x0000000000000001 (NEEDED)             Shared library: [libm.so.6]
 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]

Version symbols section '.gnu.version' contains 227 entries:
 Addr: 0x0000000000001500  Offset: 0x00001500  Link: 6 (.dynsym)
  000:   0 (*local*)       2 (GLIBC_2.34)    0 (*local*)       0 (*local*)

Version needs section '.gnu.version_r' contains 3 entries:
 Addr: 0x0000000000001a50  Offset: 0x00001a50  Link: 8 (.dynstr)
  000000: Version: 1  File: libgcc_s.so.1  Cnt: 2
  0x0010:   Name: GCC_3.3  Flags: none  Version: 17
  0x0020:   Name: GCC_3.0  Flags: none  Version: 5
  0x0030: Version: 1  File: libm.so.6  Cnt: 1
  0x0040:   Name: GLIBC_2.29  Flags: none  Version: 11
  0x0050: Version: 1  File: libc.so.6  Cnt: 4
  0x0060:   Name: GLIBC_2.34  Flags: none  Version: 2
  0x0070:   Name: GLIBC_2.9  Flags: none  Version: 23
  0x0080:   Name: GLIBC_2.2.5  Flags: none  Version: 3
  0x0090:   Name: GLIBC_2.14  Flags: none  Version: 4
"""


def _edited(old: str, new: str) -> str:
    """`_GOOD_READELF` with `old` replaced; a no-op edit would make its red a red for no reason."""
    assert old in _GOOD_READELF, old
    return _GOOD_READELF.replace(old, new)


_NEEDED_LIBC = " 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]\n"


def _with_needed(soname: str) -> str:
    """`_GOOD_READELF` plus one more NEEDED library."""
    return _edited(_NEEDED_LIBC, _NEEDED_LIBC + _NEEDED_LIBC.replace("libc.so.6", soname))


# (name, readelf output, substring the finding must carry), as `_REPORT_REDS`.
_LINK_REDS: list[tuple[str, str, str]] = [
    ("glibc 2.99", _edited("GLIBC_2.14  Flags", "GLIBC_2.99  Flags"), "GLIBC_2.99"),
    ("one minor above the floor", _edited("GLIBC_2.14  Flags", "GLIBC_2.35  Flags"), "GLIBC_2.35"),
    ("three-part version above the floor", _edited("GLIBC_2.14  Flags", "GLIBC_2.34.1  Flags"), "GLIBC_2.34.1"),
    ("glibc-internal version", _edited("GLIBC_2.14  Flags", "GLIBC_PRIVATE  Flags"), "GLIBC_PRIVATE"),
    ("unexpected NEEDED", _with_needed("libssl.so.3"), "libssl.so.3"),
    ("libpthread NEEDED", _with_needed("libpthread.so.0"), "libpthread.so.0"),
    ("dynamic musl NEEDED", _with_needed("libc.musl-x86_64.so.1"), "libc.musl"),
    ("another arch's loader", _with_needed("ld-linux-armhf.so.3"), "ld-linux-armhf.so.3"),
    ("empty output", "", "no dynamic-section verdict"),
    ("not readelf output", "readelf: Error: Not an ELF file\n", "no dynamic-section verdict"),
    (
        "libc NEEDED but no version needs parsed",
        _GOOD_READELF.partition("Version needs section")[0],
        "parsed nothing",
    ),
]

# Shapes that must stay green: aarch64 gnu lists its loader, a static-pie musl binary has a dynamic
# section and no NEEDED, a fully static one has no dynamic section, and the floor itself is allowed.
_LINK_GREENS: list[tuple[str, str]] = [
    ("v0.6.5 x86_64 gnu", _GOOD_READELF),
    ("aarch64 gnu with its loader", _with_needed("ld-linux-aarch64.so.1")),
    ("static-pie musl", "\nDynamic section at offset 0x19683e0 contains 20 entries:\n  Tag        Type  Name/Value\n"),
    ("fully static musl", "\nThere is no dynamic section in this file.\n"),
]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--scan", nargs="+", type=Path, metavar="FILE", help="byte-scan these files for the markers")
    mode.add_argument("--exec", dest="exec_binary", type=Path, metavar="BINARY", help="run and check a native binary")
    mode.add_argument("--link", nargs="+", type=Path, metavar="FILE", help="readelf-check these Linux binaries")
    parser.add_argument("--min-files", type=int, help="with --scan or --link: red below this many files (default 1)")
    try:
        args = parser.parse_args(argv)
        if args.min_files is not None and args.exec_binary is not None:
            parser.error("--min-files applies to --scan and --link only")
    except SystemExit as exit_:
        return EXIT_USAGE if exit_.code else EXIT_CLEAN  # `--help` exits 0

    try:
        if args.exec_binary is not None:
            findings = check_exec(args.exec_binary)
            for finding in findings:
                print(f"release_provenance_check: {finding}", file=sys.stderr)
            if not findings:
                print(f"release_provenance_check: {args.exec_binary} reports release provenance")
            return EXIT_FINDING if findings else EXIT_CLEAN

        min_files = 1 if args.min_files is None else args.min_files
        if min_files < 1:
            raise UsageError(f"--min-files {min_files}: a floor below 1 lets a scan over nothing pass")
        linking = args.link is not None
        # Distinct files, resolved: one binary named twice is one binary read.
        files = list(dict.fromkeys(path.resolve() for path in (args.link if linking else args.scan)))
        for path in files:
            if not path.is_file():
                raise UsageError(f"cannot {'link-check' if linking else 'scan'} {path}: not a file")
        findings = [finding for path in files for finding in check_link(path)] if linking else scan(files)
    except UsageError as error:
        print(f"release_provenance_check: {error}", file=sys.stderr)
        return EXIT_USAGE

    for finding in findings:
        print(f"release_provenance_check: {finding}", file=sys.stderr)
    if len(files) < min_files:
        print(
            f"release_provenance_check: read {len(files)} file(s), below the floor of {min_files} — "
            "a check that read fewer binaries than there are targets is no evidence",
            file=sys.stderr,
        )
        return EXIT_FINDING
    if findings:
        return EXIT_FINDING
    if linking:
        print(
            f"release_provenance_check: {len(files)} file(s) link-checked, "
            f"glibc at or below {_dotted(GLIBC_FLOOR)}, NEEDED within the allow-list"
        )
    else:
        print(f"release_provenance_check: {len(files)} file(s) scanned, no test-provenance marker")
    return EXIT_CLEAN


if __name__ == "__main__":
    sys.exit(main())
