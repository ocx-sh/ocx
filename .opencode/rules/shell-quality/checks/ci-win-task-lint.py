#!/usr/bin/env python3
"""SH-CI-20 and SH-CI-25: Taskfile tasks that use a command absent from a plain Windows PATH. Run as python3 -I.

usage: ci-win-task-lint.py [--all] [ROOT...]   (ROOT defaults to the current directory)
Default (SH-CI-20 scope): tasks that a `task NAME` call in a windows-runner step with a non-bash shell reaches,
whose cmds use an ABSENT command and that are not platforms-gated. --all (SH-CI-25 scope): every ungated task.
ABSENT is the task 3.54.0 measurement on a plain Windows PATH. A GitHub-hosted Windows image puts Git's usr\\bin on the
PATH after System32, so there a hit is advisory for the GNU names and real for find, sort, timeout and tar semantics.
Needs PyYAML in the interpreter's own site. Exit: 0 clean, 1 finding, 69 PyYAML missing."""

import re
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.stderr.write("ci-win-task-lint: PyYAML missing\n")
    sys.exit(69)

ABSENT = {
    "uname",
    "sed",
    "grep",
    "egrep",
    "awk",
    "head",
    "tail",
    "wc",
    "cut",
    "tr",
    "uniq",
    "which",
    "env",
    "date",
    "sha256sum",
    "md5sum",
    "readlink",
    "realpath",
    "dirname",
    "basename",
    "tee",
    "diff",
    "sleep",
    "id",
    "nproc",
    "stat",
    "du",
    "ln",
    "install",
    "xxd",
    "od",
    "unzip",
    "zip",
    "make",
    "sh",
    "jq",
    "wget",
}
SPLIT = re.compile(r"\|\||&&|[|;&\n]|\$\(|`")


def words(cmd):
    for seg in SPLIT.split(cmd):
        text = seg.strip().lstrip("({!").strip()
        text = re.sub(r"^(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)+", "", text)
        if not text or text.startswith("#"):
            continue
        word = text.split()[0].strip("'\"")
        if word in ("if", "then", "do", "while", "else", "elif"):
            rest = text.split(None, 1)
            if len(rest) > 1:
                word = rest[1].split()[0].strip("'\"")
            else:
                continue
        yield word


def join(base, name):
    """base/name as a display string, so a finding prints ./Taskfile.yml for the default root."""
    if name.startswith("/") or not base:
        return name
    return f"{base.rstrip('/')}/{name}"


def load_yaml(path):
    """The parsed document, or None for a file that cannot be read or parsed (the scan skips it)."""
    try:
        with Path(path).open(encoding="utf-8") as handle:
            return yaml.safe_load(handle)
    except Exception:
        return None


def load_tasks(root):
    out = {}

    def add(path, prefix):
        data = load_yaml(path) or {}
        for name, task in (data.get("tasks") or {}).items():
            body = (
                task
                if isinstance(task, dict)
                else {"cmds": [task] if isinstance(task, str) else []}
            )
            out[prefix + name] = (body, path)
        for ns, inc in (data.get("includes") or {}).items():
            target = inc if isinstance(inc, str) else (inc or {}).get("taskfile")
            if not target:
                continue
            target = join(str(Path(path).parent), target)
            if Path(target).is_dir():
                for cand in ("Taskfile.yml", "taskfile.yml"):
                    if Path(join(target, cand)).exists():
                        target = join(target, cand)
                        break
            if Path(target).exists():
                add(target, prefix + ns + ":")

    for cand in ("Taskfile.yml", "taskfile.yml", "Taskfile.yaml"):
        if Path(join(root, cand)).exists():
            add(join(root, cand), "")
            break
    return out


def bad_words(task, tasks, prefix_ns, seen):
    res = []
    if task.get("platforms"):
        return res
    for cmd in task.get("cmds") or []:
        if isinstance(cmd, str):
            text, plat, ref = cmd, None, None
        else:
            text, plat, ref = cmd.get("cmd"), cmd.get("platforms"), cmd.get("task")
        if plat:
            continue
        if ref:
            name = (ref if ref.startswith(":") else prefix_ns + ref).lstrip(":")
            if name in tasks and name not in seen:
                seen.add(name)
                res += bad_words(
                    tasks[name][0], tasks, name.rsplit(":", 1)[0] + ":" if ":" in name else "", seen
                )
            continue
        res += [w for w in words(text or "") if w in ABSENT]
    for dep in task.get("deps") or []:
        name = dep if isinstance(dep, str) else (dep or {}).get("task")
        if name and name in tasks and name not in seen:
            seen.add(name)
            res += bad_words(tasks[name][0], tasks, "", seen)
    return res


def windows_calls(root):
    calls = []
    workflows = Path(join(root, ".github/workflows"))
    for path in sorted(
        join(root, f".github/workflows/{p.name}")
        for p in workflows.glob("*.y*ml")
        if p.name[0] != "."
    ):
        doc = load_yaml(path)
        if not isinstance(doc, dict):
            continue
        wf = ((doc.get("defaults") or {}).get("run") or {}).get("shell")
        for jid, job in (doc.get("jobs") or {}).items():
            if not isinstance(job, dict):
                continue
            if (
                "windows"
                not in yaml.dump({"r": job.get("runs-on"), "s": job.get("strategy")}).lower()
            ):
                continue
            jd = ((job.get("defaults") or {}).get("run") or {}).get("shell") or wf
            for i, step in enumerate(job.get("steps") or []):
                run = step.get("run") if isinstance(step, dict) else None
                if not run:
                    continue
                shell = str(step.get("shell") or jd or "pwsh")
                if re.search(
                    r"==\s*'(?:Linux|macOS|Darwin)'|!=\s*'Windows'", str(step.get("if") or "")
                ):
                    continue
                if shell.startswith("bash"):
                    continue
                for m in re.finditer(
                    r"(?:^|[\s;&|(])task\s+((?:-[^\s]+\s+)*)([A-Za-z0-9_:.-]+)", run
                ):
                    calls.append((path, jid, i, m.group(2)))
    return calls


def main():
    allmode = "--all" in sys.argv
    roots = [a for a in sys.argv[1:] if a != "--all"] or ["."]
    found = 0
    for root in roots:
        tasks = load_tasks(root)
        if allmode:
            for name, (task, path) in sorted(tasks.items()):
                bad = bad_words(
                    task, tasks, name.rsplit(":", 1)[0] + ":" if ":" in name else "", {name}
                )
                if bad:
                    print(f"{path}: task {name} uses {sorted(set(bad))} with no platforms:")
                    found += 1
        else:
            for path, jid, i, name in windows_calls(root):
                if name in tasks:
                    prefix = name.rsplit(":", 1)[0] + ":" if ":" in name else ""
                    bad = bad_words(tasks[name][0], tasks, prefix, {name})
                    if bad:
                        print(
                            f"{path}: job={jid} step#{i} runs `task {name}` on Windows (non-bash shell); cmds use {sorted(set(bad))}"
                        )
                        found += 1
    sys.exit(1 if found else 0)


main()
