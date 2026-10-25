#!/usr/bin/env python3
"""SH-CI-04 and SH-CI-13: one line per violating workflow step. Run from the repo root as python3 -I.

SH-CI-04: a container job step with no shell: (step, job or workflow default) and bash syntax in its body.
SH-CI-13: a macos job (runs-on or strategy text) with bash 4+ syntax in a step that runs bash.
Reads top-level .github/workflows files from git only. Needs PyYAML in the interpreter's own site.
Exit: 0 clean, 1 finding, 69 PyYAML or git missing, 78 not a git work tree."""

import re
import subprocess
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.stderr.write("ci-job-shell-lint: PyYAML missing\n")
    sys.exit(69)

BASH4 = re.compile(
    r"declare\s+-[A-Za-z]*[Ag]|\bmapfile\b|\breadarray\b|\$\{[A-Za-z_]+(?:,,?|\^\^?)[^}]*\}"
    r"|\$\{[A-Za-z_]+@[QEPAaUuLK]\}|&>>|\|&|;;?&|\bwait\s+-n\b|\$EPOCHREALTIME|\blocal\s+-[nA]\b"
    r"|\[\[\s+-v\s|%\([^)]*\)T|\bcoproc\b|\bshopt\s+-s\s+globstar"
)
BASHISM = re.compile(
    r"\[\[|\bpipefail\b|\bdeclare\b|\bmapfile\b|\$'|<\(|\bsource\b|\(\(|\$\{[A-Za-z_]+(?:,,|\^\^|/)"
)
SPECS = [":(glob).github/workflow[s]/*.yml", ":(glob).github/workflow[s]/*.yaml"]


def tracked():
    try:
        res = subprocess.run(  # noqa: S603  listing the work tree through git is what the check does
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", *SPECS],  # noqa: S607
            capture_output=True,
            check=False,
        )
    except FileNotFoundError:
        sys.stderr.write("ci-job-shell-lint: git missing\n")
        sys.exit(69)
    if res.returncode:
        sys.stderr.write("ci-job-shell-lint: not a git work tree\n")
        sys.exit(78)
    return [p.decode() for p in res.stdout.split(b"\0") if p]


def dget(node, *keys):
    for key in keys:
        node = node.get(key) if isinstance(node, dict) else None
    return node


def main():
    found = 0
    for path in tracked():
        try:
            with Path(path).open(encoding="utf-8") as handle:
                doc = yaml.safe_load(handle)
        except Exception as exc:
            print(f"{path}: unparsable: {exc}")
            found += 1
            continue
        if not isinstance(doc, dict):
            continue
        wf_shell = dget(doc, "defaults", "run", "shell")
        for jid, job in (doc.get("jobs") or {}).items():
            if not isinstance(job, dict):
                continue
            default = dget(job, "defaults", "run", "shell") or wf_shell
            blob = yaml.dump({"r": job.get("runs-on"), "s": job.get("strategy")})
            for i, step in enumerate(job.get("steps") or []):
                run = step.get("run") if isinstance(step, dict) else None
                if not run:
                    continue
                shell = step.get("shell") or default
                if job.get("container") and not shell and BASHISM.search(run):
                    print(
                        f"{path}: job={jid} step#{i} SH-CI-04 container job, no shell:, bash syntax in body"
                    )
                    found += 1
                if (
                    "macos" in blob
                    and BASH4.search(run)
                    and (not shell or str(shell).startswith("bash"))
                ):
                    print(
                        f"{path}: job={jid} step#{i} SH-CI-13 macos leg, bash 4+ construct in body"
                    )
                    found += 1
    sys.exit(1 if found else 0)


main()
