#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""Refuse to publish an `ocx` binary that carries test provenance (ADR C-PROV).

    scripts/release_provenance_check.py --scan <file>... [--min-files N]
    scripts/release_provenance_check.py --exec <binary>
    scripts/release_provenance_check.py --self-test

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

`--self-test` shows every red on synthetic fixtures. Its greens exercise the
parser only (ADR AM-2): the green on a real release build — the one proof that
dependency bytes stay clear of the markers — is `task release:provenance:proof`,
and the `cross-compile` job in `verify-deep.yml` repeats it on every push to
`main`.

Exit codes: 0 clean, 1 finding, 2 usage (bad arguments, unreadable file).
Stdlib only.
"""

from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

MARKERS: tuple[bytes, ...] = (b"placeholder-g00000000", b"ci.invalid", b"placeholder/placeholder")
RUN_URL = re.compile(r"^https://github\.com/ocx-sh/ocx/actions/runs/\d+$")
SHA = re.compile(r"^[0-9a-f]{40}$")
EPOCH_PREFIX = "1970-01-01T00:00:00"
EXEC_TIMEOUT_SECONDS = 60

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


def _run(argv: list[str]) -> int:
    """`main(argv)`, its output swallowed: the self-test reports its own verdict."""
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        return main(argv)


def _expect(condition: bool, message: str, failures: list[str]) -> None:
    if not condition:
        failures.append(message)


def self_test() -> int:
    failures: list[str] = []
    with tempfile.TemporaryDirectory(prefix="release-provenance-") as tmp:
        root = Path(tmp)

        # --scan reds: one fixture per marker, marker at a known offset. The
        # markers are spelled out here, not read from MARKERS: a marker
        # dropped from MARKERS would otherwise drop its own red with it.
        for marker in (b"placeholder-g00000000", b"ci.invalid", b"placeholder/placeholder"):
            fixture = root / f"red-{marker.decode().replace('/', '_')}"
            offset = 4096 + len(marker)
            fixture.write_bytes(b"\x7fELF" + b"\0" * (offset - 4) + marker + b"\0" * 64)
            findings = scan([fixture])
            _expect(
                len(findings) == 1 and marker.decode() in findings[0] and f"offset {offset}" in findings[0]
                and str(fixture) in findings[0],
                f"scan: a fixture carrying {marker!r} at offset {offset} gave {findings}",
                failures,
            )
            _expect(
                _run(["--scan", str(fixture)]) == EXIT_FINDING,
                f"--scan over a fixture carrying {marker!r} did not exit {EXIT_FINDING}",
                failures,
            )

        # Every occurrence is reported, not only the first.
        twice = root / "red-twice"
        twice.write_bytes(MARKERS[1] + b"\0" * 10 + MARKERS[1])
        _expect(len(scan([twice])) == 2, f"scan: two occurrences gave {scan([twice])}", failures)

        # --scan green (parser only): the all-zero SHA and near-misses are not markers.
        clean = root / "clean"
        clean.write_bytes(
            b"\x7fELF" + b"0" * 40 + b"\0" * 32 + b"placeholder-g0000000\0" + b"placeholder/\0" + b"ci.inval1d"
        )
        _expect(scan([clean]) == [], f"scan: the clean fixture gave {scan([clean])}", failures)
        _expect(_run(["--scan", str(clean)]) == EXIT_CLEAN, "--scan over the clean fixture did not exit 0", failures)

        # Reader floor: fewer files than --min-files reds, even when every file is clean.
        _expect(
            _run(["--scan", str(clean), "--min-files", "2"]) == EXIT_FINDING,
            "--scan read 1 file under --min-files 2 and did not red",
            failures,
        )
        _expect(
            _run(["--scan", str(clean), str(clean), "--min-files", "2"]) == EXIT_FINDING,
            "--scan counted one file named twice as two files read",
            failures,
        )
        second = root / "clean-2"
        second.write_bytes(clean.read_bytes())
        _expect(
            _run(["--scan", str(clean), str(second), "--min-files", "2"]) == EXIT_CLEAN,
            "--scan over 2 clean files under --min-files 2 did not exit 0",
            failures,
        )

        # Usage: an unreadable file and a floor below one are not a verdict.
        _expect(
            _run(["--scan", str(root / "absent")]) == EXIT_USAGE,
            "--scan over a missing file did not exit 2",
            failures,
        )
        _expect(
            _run(["--scan", str(root)]) == EXIT_USAGE,
            "--scan over a directory did not exit 2",
            failures,
        )
        _expect(
            _run(["--scan", str(clean), "--min-files", "0"]) == EXIT_USAGE,
            "--min-files 0 was accepted",
            failures,
        )
        _expect(
            _run(["--exec", str(clean), "--min-files", "2"]) == EXIT_USAGE,
            "--min-files was accepted beside --exec",
            failures,
        )

        # --exec reds on synthetic reports, one broken field each.
        for name, report, needle in _REPORT_REDS:
            findings = evaluate_report(report)
            _expect(
                bool(findings) and any(needle in finding for finding in findings),
                f"evaluate_report[{name}]: expected a finding naming {needle!r}, got {findings}",
                failures,
            )
        # --exec green (parser only): the release-shaped report, with and without `channel`.
        _expect(evaluate_report(_GOOD_REPORT) == [], f"evaluate_report[good]: {evaluate_report(_GOOD_REPORT)}", failures)
        with_channel = _broken(("channel",), "dev")
        _expect(evaluate_report(with_channel) == [], f"evaluate_report[channel dev]: {evaluate_report(with_channel)}", failures)

        # --exec end to end, through a fake binary that prints a report.
        if os.name == "posix":
            for name, report, expected in (
                ("good", _GOOD_REPORT, EXIT_CLEAN),
                ("placeholder", _broken(("channel",), "test"), EXIT_FINDING),
            ):
                fake = root / f"fake-ocx-{name}"
                fake.write_text(
                    f"#!{sys.executable}\nimport sys\n"
                    f"assert sys.argv[1:] == ['--format', 'json', 'version'], sys.argv\n"
                    f"print({json.dumps(json.dumps(report))})\n",
                    encoding="utf-8",
                )
                fake.chmod(0o755)
                _expect(_run(["--exec", str(fake)]) == expected, f"--exec over the {name} fake did not exit {expected}", failures)
            broken = root / "fake-ocx-fails"
            # A release-shaped report on stdout, so only the exit status can red it.
            broken.write_text(
                f"#!{sys.executable}\nimport sys\nprint({json.dumps(json.dumps(_GOOD_REPORT))})\nsys.exit(3)\n",
                encoding="utf-8",
            )
            broken.chmod(0o755)
            _expect(_run(["--exec", str(broken)]) == EXIT_FINDING, "--exec over a failing binary did not red", failures)
            garbage = root / "fake-ocx-garbage"
            garbage.write_text(f"#!{sys.executable}\nprint('not json')\n", encoding="utf-8")
            garbage.chmod(0o755)
            _expect(_run(["--exec", str(garbage)]) == EXIT_FINDING, "--exec over non-JSON output did not red", failures)
        else:
            # A shebang fake cannot execute here; say so rather than pass as if it had.
            print(
                f"release_provenance_check self-test: SKIP --exec end-to-end fakes (os.name={os.name!r}, needs posix)",
                file=sys.stderr,
            )

    if failures:
        for failure in failures:
            print(f"release_provenance_check self-test: FAIL {failure}", file=sys.stderr)
        return EXIT_FINDING
    print(
        f"release_provenance_check self-test: {len(MARKERS)} marker reds, the floor red, "
        f"{len(_REPORT_REDS)} report reds; greens exercise the parser only (the real-binary green is "
        "`task release:provenance:proof`)"
    )
    return EXIT_CLEAN


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--scan", nargs="+", type=Path, metavar="FILE", help="byte-scan these files for the markers")
    mode.add_argument("--exec", dest="exec_binary", type=Path, metavar="BINARY", help="run and check a native binary")
    mode.add_argument("--self-test", action="store_true", help="show every red on synthetic fixtures")
    parser.add_argument("--min-files", type=int, help="with --scan: red when fewer files are read (default 1)")
    try:
        args = parser.parse_args(argv)
        if args.min_files is not None and args.scan is None:
            parser.error("--min-files applies to --scan only")
    except SystemExit as exit_:
        return EXIT_USAGE if exit_.code else EXIT_CLEAN  # `--help` exits 0

    if args.self_test:
        return self_test()
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
        # Distinct files, resolved: one binary named twice is one binary read.
        files = list(dict.fromkeys(path.resolve() for path in args.scan))
        for path in files:
            if not path.is_file():
                raise UsageError(f"cannot scan {path}: not a file")
        findings = scan(files)
    except UsageError as error:
        print(f"release_provenance_check: {error}", file=sys.stderr)
        return EXIT_USAGE

    for finding in findings:
        print(f"release_provenance_check: {finding}", file=sys.stderr)
    if len(files) < min_files:
        print(
            f"release_provenance_check: read {len(files)} file(s), below the floor of {min_files} — "
            "a scan that read fewer binaries than there are targets is no evidence",
            file=sys.stderr,
        )
        return EXIT_FINDING
    if findings:
        return EXIT_FINDING
    print(f"release_provenance_check: {len(files)} file(s) scanned, no test-provenance marker")
    return EXIT_CLEAN


if __name__ == "__main__":
    sys.exit(main())
