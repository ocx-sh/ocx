#!/usr/bin/env python3
"""Round-trip the hostile corpus through cmd.exe `SET "K=v"` on a real Windows host, in four channels.

    python3 -I batch-set.py plant|twin [--stage DIR] [--cmd PATH]
    python3 -I batch-set.py --shim CMD [--stage DIR] [--cmd PATH]

Run it from WSL (or any Python that can start cmd.exe) with --stage pointing at a directory on the Windows drive,
for example /mnt/c/Users/NAME/batch-run. cmd.exe is started with that directory as the working directory because
Windows processes cannot use a \\\\wsl$ cwd. The 35 round-trip cases of corpus.txt plus 8 extra C0 controls are tried.
plant and twin are two reference predicates for which values a SET line may carry:
  plant  refuses % " LF CR only (the common four)
  twin   also refuses ! NUL U+001A, the empty value, any non-ASCII character, and a statement over 8000 characters
--shim CMD runs your own emitter: CMD is split like a shell line and called as `CMD NAME HEX`. It prints the whole
SET statement and exits 0, or exits 65 with empty stdout to refuse the value.
Every admitted value is emitted raw and must read back byte for byte in all four channels (a .bat file, console
stdin, FOR /F over a file in a .bat, FOR /F typed at the console), delayed expansion off and on, code page 437.
A run is OK, CORRUPT (value differs), NOVAR (variable absent) or PWNED (a payload created pwned.txt).
Exit 1 and print each failing run when an admitted value does not round-trip.
"""

import contextlib
import os
import shlex
import shutil
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import NamedTuple

HERE = Path(__file__).absolute().parent
NAME = "XBATK"
CHANNELS = ("file", "stdin", "forf-file", "forf-stdin")
EXTRA = [
    ("c0-bel", "a\x07b"),
    ("c0-bs", "a\x08b"),
    ("c0-vt", "a\x0bb"),
    ("c0-ff", "a\x0cb"),
    ("c0-so", "a\x0eb"),
    ("c0-fs", "a\x1cb"),
    ("c0-us", "a\x1fb"),
    ("c0-esc", "a\x1b[31mb"),
]


def cases():
    out = []
    with (HERE.parent / "corpus.txt").open(encoding="utf-8") as f:
        for line in f:
            name, _, hx = line.rstrip("\n").partition("\t")
            if name != "nul":  # NUL is refused for every target before any predicate runs
                out.append((name, bytes.fromhex(hx).decode("utf-8")))
    return out + EXTRA


def plant_refuses(v):
    return any(c in v for c in '%"\n\r')


def twin_refuses(v):
    return (
        plant_refuses(v)
        or any(c in v for c in "\x00\x1a!")
        or v == ""
        or any(ord(c) > 127 for c in v)
        or len(f'SET "{NAME}={v}"') > 8000
    )


def statement(mode, shim, name, v):
    """The SET line to run for value v, or None when the emitter refuses it."""
    if shim:
        r = subprocess.run(  # noqa: S603  the shim is the emitter under test, called once per value
            [*shim, NAME, v.encode("utf-8", "surrogatepass").hex()],
            capture_output=True,
            check=False,
        )
        if r.returncode == 65 and not r.stdout:
            return None
        if r.returncode != 0:
            raise SystemExit(f"shim failed on {name}: exit {r.returncode}")
        return r.stdout.decode("utf-8").rstrip("\r\n")
    refuses = plant_refuses if mode == "plant" else twin_refuses
    return None if refuses(v) else f'SET "{NAME}={v}"'


class Job(NamedTuple):
    stage: str
    cmd: str
    name: str
    v: str
    stmt: str
    ch: str
    dl: str


def run_one(job):
    for _ in range(
        3
    ):  # a cmd.exe start through WSL interop sometimes yields nothing under load: retry NOVAR
        r = one(job)
        if r[1] != "NOVAR":
            return r
    return r


def enc(lines):
    return ("\r\n".join(lines) + "\r\n").encode("utf-8", "surrogatepass")


def one(job):
    stage, cmd, name, v, stmt, ch, dl = job
    rid = f"{name}.{ch}.{dl}"
    d = Path(stage) / rid
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir(parents=True)
    rb = ["chcp 65001>nul", f"set {NAME}>out.txt"]
    delayed = ["setlocal EnableDelayedExpansion"] if dl == "on" else []
    (d / "stmt.txt").write_bytes(enc([stmt]))
    argv = [cmd, "/d", "/q", *(["/v:on"] if dl == "on" else [])]
    stdin = None
    if ch == "file":
        (d / "t.bat").write_bytes(enc(["@echo off", *delayed, stmt, *rb]))
        argv = [cmd, "/d", "/c", "t.bat"]
    elif ch == "forf-file":
        forf = 'FOR /F "usebackq delims=" %%i IN ("stmt.txt") DO @%%i'
        (d / "t.bat").write_bytes(enc(["@echo off", *delayed, forf, *rb]))
        argv = [cmd, "/d", "/c", "t.bat"]
    elif ch == "stdin":
        (d / "in.txt").write_bytes(enc([stmt, *rb, "exit"]))
        stdin = d / "in.txt"
    else:
        (d / "in.txt").write_bytes(
            enc(['FOR /F "usebackq delims=" %i IN ("stmt.txt") DO @%i', *rb, "exit"])
        )
        stdin = d / "in.txt"
    with contextlib.ExitStack() as stack:
        feed = stack.enter_context(stdin.open("rb")) if stdin else subprocess.DEVNULL
        try:
            subprocess.run(  # noqa: S603  cmd.exe is the interpreter under test
                argv,
                cwd=d,
                stdin=feed,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=60,
                check=False,
            )
        except subprocess.TimeoutExpired:
            return rid, "TIMEOUT", ""
    out = d / "out.txt"
    got = out.read_bytes() if out.exists() else None
    want = NAME.encode() + b"=" + v.encode("utf-8", "surrogatepass") + b"\r\n"
    if (d / "pwned.txt").exists():
        res = "PWNED"
    elif got is None or got.strip() == b"":
        res = "NOVAR"
    elif got == want:
        res = "OK"
    else:
        res = "CORRUPT"
    shutil.rmtree(d, ignore_errors=True)
    return (
        rid,
        res,
        (got or b"")[:70].decode("utf-8", "replace").replace("\r", "\\r").replace("\n", "\\n"),
    )


def main(argv):
    mode = shim = None
    stage = os.environ.get("BATCH_STAGE", "")
    cmd = "/mnt/c/Windows/System32/cmd.exe"
    i = 0
    while i < len(argv):
        a = argv[i]
        if a in ("plant", "twin"):
            mode = a
        elif a == "--shim":
            shim = shlex.split(argv[i + 1])
            i += 1
        elif a == "--stage":
            stage = argv[i + 1]
            i += 1
        elif a == "--cmd":
            cmd = argv[i + 1]
            i += 1
        else:
            sys.exit(__doc__)
        i += 1
    if not (mode or shim) or not stage or not Path(cmd).exists():
        sys.exit(__doc__)
    Path(stage).mkdir(parents=True, exist_ok=True)
    cs = cases()
    plan = [(n, v, statement(mode, shim, n, v)) for n, v in cs]
    adm = [(n, v, s) for n, v, s in plan if s is not None]
    print("admitted:", " ".join(n for n, _, _ in adm))
    print("refused :", " ".join(n for n, _, s in plan if s is None))
    jobs = [
        Job(stage, cmd, n, v, s, ch, dl)
        for n, v, s in adm
        for ch in CHANNELS
        for dl in ("off", "on")
    ]
    with ThreadPoolExecutor(4) as ex:
        res = list(ex.map(run_one, jobs))
    bad = [r for r in res if r[1] != "OK"]
    for rid, r, got in bad:
        print("FAIL", rid, r, repr(got))
    print(
        f"{mode or 'shim'}: admitted {len(adm)} of {len(cs)} values, {len(res)} runs, {len(bad)} failing"
    )
    return 1 if bad else 0


if __name__ == "__main__":
    sys.dont_write_bytecode = True
    sys.exit(main(sys.argv[1:]))
