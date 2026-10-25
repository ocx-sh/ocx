#!/usr/bin/env python3
"""SH-CI-19: print the shell bodies of a Taskfile as one bash file for ShellCheck. Run as python3 -I.

usage: ci-tf-extract.py Taskfile.yml > out.sh
Covers cmds, status, defer, preconditions[].sh, vars.NAME.sh at Taskfile and task level, and a task-level cmd. Each body
is wrapped in a subshell and {{ }} templates become the word TMPL. Needs PyYAML in the interpreter's own site.
Exit: 0 written, 1 unreadable or unparsable input, 64 bad usage, 69 PyYAML missing."""

import re
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.stderr.write("ci-tf-extract: PyYAML missing\n")
    sys.exit(69)

if len(sys.argv) != 2:
    sys.stderr.write("usage: ci-tf-extract.py Taskfile.yml > out.sh\n")
    sys.exit(64)


def tmpl(text):
    return re.sub(r"\{\{.*?\}\}", "TMPL", str(text))


def shells(task, top=False):
    for value in (task.get("vars") or {}).values():
        if isinstance(value, dict) and isinstance(value.get("sh"), str):
            yield "vars.sh", value["sh"]
    if top:
        return
    for key in ("cmds", "status"):
        for cmd in task.get(key) or []:
            if isinstance(cmd, str):
                yield key, cmd
            elif isinstance(cmd, dict):
                if isinstance(cmd.get("cmd"), str):
                    yield key, cmd["cmd"]
                defer = cmd.get("defer")
                if isinstance(defer, str):
                    yield "defer", defer
                elif isinstance(defer, dict) and isinstance(defer.get("cmd"), str):
                    yield "defer", defer["cmd"]
    for pre in task.get("preconditions") or []:
        if isinstance(pre, str):
            yield "preconditions", pre
        elif isinstance(pre, dict) and isinstance(pre.get("sh"), str):
            yield "preconditions", pre["sh"]
    if isinstance(task.get("cmd"), str):
        yield "cmd", task["cmd"]


def main():
    try:
        with Path(sys.argv[1]).open(encoding="utf-8") as handle:
            data = yaml.safe_load(handle) or {}
    except Exception as exc:
        sys.stderr.write(f"ci-tf-extract: {sys.argv[1]}: {exc}\n")
        sys.exit(1)
    print("#!/usr/bin/env bash")
    for kind, body in shells(data, top=True):
        print(f"# --- taskfile ({kind})")
        print("(\n" + tmpl(body) + "\n)")
    for name, task in (data.get("tasks") or {}).items():
        if isinstance(task, str):
            spec = {"cmds": [task]}
        elif isinstance(task, list):
            spec = {"cmds": task}
        else:
            spec = task
        for kind, body in shells(spec):
            print(f"# --- task {name} ({kind})")
            print("(\n" + tmpl(body) + "\n)")


main()
