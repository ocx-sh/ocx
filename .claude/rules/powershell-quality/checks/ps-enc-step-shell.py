#!/usr/bin/env python3
"""checks/ps-enc-step-shell.py: PS-ENC-10, the effective shell of the workflow step that holds a given line.

Usage: python3 -I ps-enc-step-shell.py FILE LINE...
Output: "FILE:LINE job=NAME runs-on=X shell=S" per line, or "FILE:LINE NOSTEP" when no step holds it.
The shell is the step's own `shell:`, else the job's `defaults.run.shell`, else the workflow's, else "(default)".
A composite action's `runs.steps` count as a job (their `shell:` is required).
Exit: 0 printed, 1 the file is not a workflow or action, 64 usage. Needs PyYAML.
Watched red and green on a `shell: powershell` plant and a `shell: pwsh` twin (measured 2026-10-11).
"""

import sys
from pathlib import Path

import yaml


class Loader(yaml.SafeLoader):
    pass


def construct(loader, node):
    mapping = loader.construct_mapping(node, deep=True)
    mapping["__line__"] = node.start_mark.line + 1
    mapping["__end__"] = node.end_mark.line + 1
    return mapping


Loader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, construct)


def default_shell(doc):
    return ((doc.get("defaults") or {}).get("run") or {}).get("shell")


def main(argv):
    if len(argv) < 3:
        print("usage: ps-enc-step-shell.py FILE LINE...", file=sys.stderr)
        return 64
    path, wanted = argv[1], [int(x) for x in argv[2:]]
    doc = yaml.load(Path(path).read_text(encoding="utf-8"), Loader)  # noqa: S506  Loader extends SafeLoader
    if not isinstance(doc, dict):
        print(f"{path}: not a mapping", file=sys.stderr)
        return 1
    jobs = [(n, j) for n, j in (doc.get("jobs") or {}).items() if isinstance(j, dict)]
    if isinstance(doc.get("runs"), dict) and doc["runs"].get("steps"):
        jobs.append(("composite", doc["runs"]))
    seen = set()
    for name, job in jobs:
        fallback = default_shell(job) or default_shell(doc)
        for step in job.get("steps") or []:
            if not isinstance(step, dict):
                continue
            for line in wanted:
                if step["__line__"] <= line <= step["__end__"]:
                    seen.add(line)
                    shell = step.get("shell") or fallback or "(default)"
                    print(f"{path}:{line} job={name} runs-on={job.get('runs-on')} shell={shell}")
    for line in wanted:
        if line not in seen:
            print(f"{path}:{line} NOSTEP")
    return 0


sys.exit(main(sys.argv))
