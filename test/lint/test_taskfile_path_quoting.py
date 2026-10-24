# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""A path interpolated into a path-taking command is quoted in every taskfile.

`rm -rf {{.JUNIT_DIR}}` under a checkout at `/home/u/ocx work` deletes `/home/u/ocx`;
`--basetemp={{.X}}` makes pytest delete the shortened path. Task's shell (mvdan/sh)
quotes with `'…'` and `"…"` on every platform, so an unquoted `{{…}}` argument of a
`GUARDED` command or `GUARDED_ARGS` form is the defect. Quote state is scanned per
parsed command, so `rm` in a `desc:` or as a `docker compose rm` argument is skipped.
"""

from __future__ import annotations

import re
import subprocess
from collections.abc import Iterator
from pathlib import Path

import pytest
import yaml  # a suite dependency (test/pyproject.toml)

REPO_ROOT = Path(__file__).resolve().parents[2]

GUARDED = frozenset({"rm", "mkdir", "cp", "mv", "tee", "chmod", "tar", "install", "test"})
# Argument forms guarded whatever command carries them.
GUARDED_ARGS = ("--basetemp=",)
# Template actions that print nothing themselves; the text between them is the command.
CONTROL = re.compile(r"\{\{-?\s*(?:if|else|end|range|with|define)\b[^}]*\}\}")
VALUE = re.compile(r"\{\{.*?\}\}")
# After one of these the next word is a command again.
KEYWORDS = frozenset({"if", "while", "until", "then", "do", "else", "elif", "!", "{", "(", "time"})
ENV_PREFIX = re.compile(r"^[A-Za-z_]\w*=")
COMMAND_KEYS = frozenset({"cmd", "defer", "sh"})
LIST_KEYS = frozenset({"cmds", "status"})

# Live: 25 taskfiles, 319 command strings, 88 invocations of a GUARDED word. A reader
# that stopped matching (a renamed key, a changed loader) must red, not go clean.
COMMAND_FLOOR = 319
GUARDED_FLOOR = 88


def _taskfiles() -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "-z", "--", "*taskfile.yml"], cwd=REPO_ROOT, capture_output=True, check=True
    ).stdout
    return [REPO_ROOT / name.decode() for name in out.split(b"\0") if name]


def _commands(node: object, key: str = "") -> Iterator[str]:
    """Every shell command string in a parsed taskfile, wherever task runs one."""
    if isinstance(node, str):
        if key in COMMAND_KEYS:
            yield node
    elif isinstance(node, list):
        for item in node:
            if isinstance(item, str) and key in LIST_KEYS:
                yield item
            else:
                yield from _commands(item, key)
    elif isinstance(node, dict):
        for k, v in node.items():
            yield from _commands(v, str(k))


def unquoted_in_guarded(command: str) -> tuple[list[str], int, bool]:
    """Guarded-command words carrying a template value outside any quote.

    Also returns how many guarded commands ran, and whether every quote and
    `$(` closed: an open one means the scan lost the shell's thread.
    """
    text = VALUE.sub("\0", CONTROL.sub("", command))
    # Context stack: "c" = command text, "s" = '…', "d" = "…"; `$(` opens a nested "c".
    stack = ["c"]
    names = [""]  # the current command's name, one per open "c" context
    saved: list[tuple[str, bool, bool]] = []  # the outer word while a `$(` runs
    word, live, quoted = "", False, False  # live: holds a value outside quotes
    seen = 0
    found: list[str] = []

    def end_word() -> None:
        nonlocal word, live, quoted, seen
        if word or quoted:
            if names[-1]:
                if live and (names[-1] in GUARDED or word.startswith(GUARDED_ARGS)):
                    found.append(word.replace("\0", "{{…}}"))
            elif word not in KEYWORDS and not ENV_PREFIX.match(word):
                names[-1] = word
                seen += word in GUARDED
        word, live, quoted = "", False, False

    i = 0
    while i < len(text):
        ch = text[i]
        top = stack[-1]
        if ch == "\\" and top != "s" and i + 1 < len(text):
            i += 1
            if text[i] != "\n":  # backslash-newline is a continuation, not a character
                word += text[i]
        elif top == "s":
            if ch == "'":
                stack.pop()
            else:
                word += ch
        elif text.startswith("$(", i):
            saved.append((word, live, quoted))
            word, live, quoted = "", False, False
            stack.append("c")
            names.append("")
            i += 1
        elif top == "d":
            if ch == '"':
                stack.pop()
            else:
                word += ch
        elif ch in "'\"":
            stack.append("s" if ch == "'" else "d")
            quoted = True
        elif ch == "#" and not word and not quoted:
            i = text.find("\n", i) - 1 if "\n" in text[i:] else len(text)
        elif ch == "\0":
            word += ch
            live = True
        elif ch == ")" and len(names) > 1:
            end_word()
            stack.pop()
            names.pop()
            word, live, quoted = saved.pop()
        elif ch in ";|&\n":
            end_word()
            names[-1] = ""
        elif ch.isspace():
            end_word()
        else:
            word += ch
        i += 1
    end_word()
    return found, seen, stack == ["c"]


def _scan() -> tuple[int, int, list[str]]:
    commands = guarded = 0
    problems: list[str] = []
    for path in _taskfiles():
        doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        for command in _commands(doc):
            commands += 1
            found, seen, closed = unquoted_in_guarded(command)
            guarded += seen
            where = f"{path.relative_to(REPO_ROOT)}: {command.strip()[:90]!r}"
            problems += [f"unquoted {word} <- {where}" for word in found]
            if not closed:
                problems.append(f"scan ended inside a quote or $( <- {where}")
    return commands, guarded, problems


def test_guarded_commands_quote_every_interpolated_path():
    commands, guarded, problems = _scan()
    assert commands >= COMMAND_FLOOR, f"read {commands} taskfile commands, floor {COMMAND_FLOOR}"
    words = ", ".join(sorted(GUARDED))
    assert guarded >= GUARDED_FLOOR, f"saw {guarded} commands of ({words}), floor {GUARDED_FLOOR}"
    assert not problems, "template path unquoted as a guarded-command or --basetemp= argument:\n" + "\n".join(problems)


@pytest.mark.parametrize(
    ("command", "unquoted"),
    [
        # The finding: a checkout path with a space splits into two arguments.
        ("{{if not .KEEP_JUNIT}}rm -rf {{.JUNIT_DIR}}{{end}}", 1),
        ("mkdir -p {{.ROOT_DIR}}/target/bazel", 1),
        ("cp -RLf {{.A}}/x/. {{.B}}/", 2),
        ("cd /tmp && mv {{.A}} {{.B}}", 2),
        ("x=$(rm {{.X}})", 1),
        ("FOO=1 rm {{.X}}", 1),
        ("rm -f \\\n  {{.X}}", 1),
        # The widened set: every command that takes a path, and the pytest basetemp argument.
        ("uv run pytest --basetemp={{.TEST_BASETEMP}} --junit-xml=x.xml", 1),
        ("uv run pytest --basetemp={{.TEST_BASETEMP}}-lint", 1),
        ("pytest 2>&1 | tee {{.TEST_BASETEMP}}-lint.log", 1),
        ("chmod -R u+w {{.CASTS_DIR}}", 1),
        ("tar -C {{.CASTS_DIR}} -xf a.tar", 1),
        ("install -m 0755 scripts/pre-push.sh {{.HOOKS}}/hooks/pre-push", 1),
        ("test -d {{.FONTS_DIR}} || exit 1", 1),
        # A command in an `if` condition is still in command position.
        ("if rm -rf {{.X}}; then echo gone; fi", 1),
        ("if ! rm -rf {{.X}}; then echo kept; fi", 1),
        ("while rm -f {{.X}}; do :; done", 1),
        ("until rm -f {{.X}}; do :; done", 1),
        # Quoted, or not a guarded command position.
        ('{{if not .KEEP_JUNIT}}rm -rf -- "{{.JUNIT_DIR}}"{{end}}', 0),
        ("rm -rf '{{.BEP_DIR}}'", 0),
        ('mkdir -p "{{.A}}/cargo" "{{.A}}/xwin"', 0),
        ("mkdir -p \"$(dirname '{{.JUNIT}}')\"", 0),
        ("mkdir -p \"$(dirname {{.X}})\"", 0),  # dirname is not guarded
        ("docker compose -f x.yml --profile bench rm -f bench-proxy {{.SERVICE}}", 0),
        ("echo rm {{.X}}", 0),
        ("# don't rm {{.X}}\nmkdir -p '{{.Y}}'", 0),
        ("uv run pytest --junit-xml={{.JUNIT_DIR}}/a.xml", 0),
        ('uv run pytest --basetemp="{{.TEST_BASETEMP}}"-lint --junit-xml=x.xml', 0),
        ('pytest 2>&1 | tee "{{.TEST_BASETEMP}}-lint.log"', 0),
        ('chmod -R u+w "{{.CASTS_DIR}}"', 0),
        ("tar -C '{{.CASTS_DIR}}' -xf a.tar", 0),
        ('install -m 0755 scripts/pre-push.sh "{{.HOOKS}}/hooks/pre-push"', 0),
        ('test -d "{{.FONTS_DIR}}" || exit 1', 0),
        ('if rm -rf "{{.X}}"; then echo gone; fi', 0),
        ("cargo install --path {{.X}}", 0),  # install as an argument is not a command position
    ],
)
def test_scanner_is_red_on_an_unquoted_value_and_green_on_a_quoted_one(command: str, unquoted: int):
    found, _, closed = unquoted_in_guarded(command)
    assert closed
    assert len(found) == unquoted, f"{command!r}: {found}"
