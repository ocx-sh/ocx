#!/usr/bin/env python3
"""SH-CI-12: one line per pwsh or powershell run step with two or more native commands and no exit-code guard.

Covers explicit shell: pwsh/powershell steps, composite action steps, and steps with no shell: whose job can run on
Windows (the default there is pwsh). Reads top-level .github/workflows files and every action.yml from git.
Needs PyYAML in the interpreter's own site. Run from the repo root as python3 -I.
Exit: 0 clean, 1 finding, 69 PyYAML or git missing, 78 not a git work tree."""

import re
import subprocess
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.stderr.write("ci-pwsh-run-lint: PyYAML missing\n")
    sys.exit(69)

KW = re.compile(
    r"^(if|else|elseif|foreach|for|while|switch|try|catch|finally|function|return|exit|throw|param|\$|\{|\}|#|\(|\)|@|\"|'|\[|\.|-|\|)",
    re.IGNORECASE,
)
CMDLET = re.compile(r"^[A-Za-z]+-[A-Za-z]+")
ALIAS = re.compile(
    r"^(echo|cd|mkdir|cp|rm|ls|cat|sleep|type|copy|del|dir|set|chmod)\b", re.IGNORECASE
)
GUARD = re.compile(r"LASTEXITCODE|PSNativeCommandUseErrorActionPreference|Invoke-Native|\$\?")
KEYVAL = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*\s*=")
SPECS = [
    ":(glob).github/workflow[s]/*.yml",
    ":(glob).github/workflow[s]/*.yaml",
    ":(glob)**/action.yml",
    ":(glob)**/action.yaml",
]


def natives(run):
    run = re.sub(r'@"\r?\n.*?\r?\n\s*"@', "", run, flags=re.DOTALL)
    run = re.sub(r"@'\r?\n.*?\r?\n\s*'@", "", run, flags=re.DOTALL)
    out = []
    for raw in run.splitlines():
        line = raw.strip()
        if not line or KW.match(line) or CMDLET.match(line) or ALIAS.match(line):
            continue
        if KEYVAL.match(line) or line in ("break", "continue"):
            continue
        out.append(line)
    return out


def strs(node):
    if isinstance(node, str):
        yield node
    elif isinstance(node, dict):
        for value in node.values():
            yield from strs(value)
    elif isinstance(node, list):
        for value in node:
            yield from strs(value)


def tracked():
    try:
        res = subprocess.run(  # noqa: S603  listing the work tree through git is what the check does
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", *SPECS],  # noqa: S607
            capture_output=True,
            check=False,
        )
    except FileNotFoundError:
        sys.stderr.write("ci-pwsh-run-lint: git missing\n")
        sys.exit(69)
    if res.returncode:
        sys.stderr.write("ci-pwsh-run-lint: not a git work tree\n")
        sys.exit(78)
    return [p.decode() for p in res.stdout.split(b"\0") if p]


def default_shell(node):
    defaults = node.get("defaults") if isinstance(node, dict) else None
    run = defaults.get("run") if isinstance(defaults, dict) else None
    return run.get("shell") if isinstance(run, dict) else None


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
        wf = default_shell(doc)
        jobs = doc.get("jobs") or {}
        if isinstance(doc.get("runs"), dict):
            jobs = {"composite": {"steps": doc["runs"].get("steps", []), "runs-on": "any"}}
        for jname, job in jobs.items():
            if not isinstance(job, dict):
                continue
            js = default_shell(job)
            runs_on = str(job.get("runs-on", ""))
            strategy = job.get("strategy")  # a string when the whole strategy is an expression
            matrix = " ".join(
                strs(strategy.get("matrix", {}) if isinstance(strategy, dict) else strategy)
            ).lower()
            win = (
                "windows" in runs_on.lower()
                or ("matrix" in runs_on and "windows" in matrix)
                or runs_on == "any"
            )
            for i, step in enumerate(job.get("steps") or []):
                if not isinstance(step, dict) or not isinstance(step.get("run"), str):
                    continue
                shell = step.get("shell") or js or wf
                if shell in ("pwsh", "powershell") or (shell is None and win):
                    count = len(natives(step["run"]))
                    if count >= 2 and not GUARD.search(step["run"]):
                        print(
                            f"{path}: job={jname} step#{i} shell={shell or 'default'} natives={count} unguarded"
                        )
                        found += 1
    sys.exit(1 if found else 0)


main()
