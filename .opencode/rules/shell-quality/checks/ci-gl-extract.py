#!/usr/bin/env python3
"""SH-CI-28, SH-CI-29, SH-CI-33: split a GitLab CI file into one shell file per job for ShellCheck. Run as python3 -I.

usage: ci-gl-extract.py [--sh|--bash] FILE.yml OUTDIR
Writes OUTDIR/NN-JOB.sh (before_script and script, one shell as the runner does) and OUTDIR/NN-JOB.after.sh
(after_script, its own shell). Jobs on a Windows runner are skipped: they run PowerShell or cmd and are linted under
the PowerShell rules. A job is a Windows job when its effective tags (its own, else those of its extends chain in the
same file) hold a whole word windows or win (win10, win2019 and saas-windows-medium-amd64 too), or its image names
windows, servercore or nanoserver; with no tags known, the job name and extends names are read the same way. Words
are whole: wine, darwin and winding are Linux or macOS. Handles !reference tags and multi-document files. Template
tokens in double braces are blanked first. Needs PyYAML in the interpreter's own site.
Exit: 0 written, 1 unreadable or unparsable input, 64 bad usage, 69 PyYAML missing."""

import re
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.stderr.write("ci-gl-extract: PyYAML missing\n")
    sys.exit(69)

args = [a for a in sys.argv[1:] if not a.startswith("--")]
if len(args) != 2:
    sys.stderr.write("usage: ci-gl-extract.py [--sh|--bash] FILE.yml OUTDIR\n")
    sys.exit(64)
shell = "bash" if "--bash" in sys.argv else "sh"
path, outdir = args


class Loader(yaml.SafeLoader):
    pass


Loader.add_constructor(
    "!reference",
    lambda ld, node: "__reference__ " + " ".join(map(str, ld.construct_sequence(node))),
)


# a whole word: windows, win, win10, windows-1809 (split on punctuation and camelCase), or a known Windows image
WIN = re.compile(r"(win|windows)(\d\w*)?|windowsservercore|servercore|nanoserver")


def words(values):
    for value in values:
        for word in re.split(r"[^A-Za-z0-9]+", re.sub(r"(?<=[a-z])(?=[A-Z])", " ", str(value))):
            yield word.lower()


def names(value):
    return [value] if isinstance(value, str) else list(value or [])


def inherit(body, jobs, key, seen=()):
    """The job's own value for key, else the one from its extends chain (later entries win), else None."""
    if body.get(key) is not None:
        return body[key]
    for parent in reversed(names(body.get("extends"))):
        if parent in seen or not isinstance(jobs.get(parent), dict):
            continue
        value = inherit(jobs[parent], jobs, key, (*seen, parent))
        if value is not None:
            return value
    return None


def scalars(value):
    if isinstance(value, dict):
        value = list(value.values())
    for item in value if isinstance(value, list) else [value]:
        if isinstance(item, (dict, list)):
            yield from scalars(item)
        elif item is not None:
            yield item


def winish(job, body, jobs):
    image = body.get("image")
    image = image.get("name") if isinstance(image, dict) else image
    tags = inherit(body, jobs, "tags")
    found = list(scalars(tags))
    if tags is None or any(
        "$" in str(t) for t in found
    ):  # tags unknown or a variable: read the matrix, name, extends
        found += [job, *names(body.get("extends")), *scalars(inherit(body, jobs, "parallel"))]
    return any(WIN.fullmatch(w) for w in words([image or "", *found]))


def entries(value):
    for entry in [value] if isinstance(value, str) else value:
        text = "\n".join(map(str, entry)) if isinstance(entry, list) else str(entry)
        yield text.rstrip("\n")


def main():
    try:
        text = re.sub(r"\{\{[^}]*\}\}", "", Path(path).read_text(encoding="utf-8"))
        docs = list(yaml.load_all(text, Loader=Loader))
    except Exception as exc:
        sys.stderr.write(f"ci-gl-extract: {path}: {exc}\n")
        sys.exit(1)
    Path(outdir).mkdir(parents=True, exist_ok=True)
    count = 0
    for doc in docs:
        if not isinstance(doc, dict):
            continue
        for job, body in doc.items():
            if not isinstance(body, dict) or winish(job, body, doc):
                continue
            for suffix, keys in (("", ("before_script", "script")), (".after", ("after_script",))):
                lines = []
                for key in keys:
                    if body.get(key) is None:
                        continue
                    for entry in entries(body[key]):
                        ref = entry.startswith("__reference__ ")
                        lines.append(f"# gl {job}:{key}" + (" !reference" if ref else ""))
                        if not ref:
                            lines.append(entry)
                if lines:
                    count += 1
                    name = re.sub(r"[^A-Za-z0-9_.-]", "_", job)
                    target = Path(outdir) / f"{count:02d}-{name}{suffix}.sh"
                    target.write_text(
                        f"#!/bin/{shell}\n" + "\n".join(lines) + "\n", encoding="utf-8"
                    )


main()
