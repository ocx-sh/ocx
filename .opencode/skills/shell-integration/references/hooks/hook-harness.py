#!/usr/bin/env python3
"""Register an emitted prompt hook next to a user hook, apply it once and twice, count the firings.

    python3 -I hook-harness.py SHELL IMPL [--event prompt|dir] [-v] -- INTERP [ARGS...]

SHELL is bash, zsh, fish, nu or elvish. IMPL is the file your emitter produced for that
shell (the fixtures/ directory holds a compliant twin and the plants). --event dir
tests a directory-change hook (zsh, fish and elvish, driven by cd, pushd and popd; the
nu prompt run already counts its PWD hook and bash has no directory event). INTERP is the
interactive interpreter command, for example `bash --norc -i`, `zsh -f -i`, `fish -i
--no-config`, `nu`, `elvish`, or a `docker run --rm -i -v DIR:DIR IMAGE ...` line for
an old version (add -t for fish, nu and elvish, which are driven through a pty).
The hook must print the token FIRED:ours (nu: FIRED:ours_pp and FIRED:ours_pwd, and
for --event dir FIRED:ours_dir).
Every scenario is run twice, once with IMPL and once with an empty file as the
baseline. A scenario fails when a user hook fires less often than in the baseline,
when ours fires other than once per prompt, when bash reports a syntax error, or
when the session produced no marker (INVALID, never a pass). Exit 0 only when every
scenario passes. Run it once per interpreter version.
"""

import collections
import contextlib
import os
import pty
import re
import select
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import NamedTuple

PRESETS = {
    "bash": ["unset", "string", "string-semi", "array2", "twice"],
    "zsh": ["none", "func", "array", "twice"],
    "fish": ["once", "twice"],
    "nu": ["default", "list", "string", "twice"],
    "elvish": ["user", "twice"],
}
WANT = {  # token -> prompts the session produces; "=user_h" means as often as the user handler
    "bash": {"ours": 4},
    "zsh": {"ours": 3},
    "fish": {"ours": "=user_h"},
    "nu": {"ours_pp": 3, "ours_pwd": 1},
    "elvish": {"ours": 3},
}
DIRPRESETS = {
    "zsh": ["none", "func", "array", "twice"],
    "fish": ["none", "user", "twice"],
    "elvish": ["none", "user", "twice"],
}
MARK = "--- PROMPTS"


class Run(NamedTuple):
    shell: str
    event: str
    impl: str
    preset: str
    interp: list
    tmp: str


def pty_session(lines, cmd, env):
    """Send each line once the output goes quiet. Returns ANSI-stripped output."""
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(cmd[0], cmd, env)  # noqa: S606  the interpreter command is the check's operand
    out = b""

    def drain(quiet, cap):
        nonlocal out
        t0 = last = time.time()
        while time.time() - t0 < cap:
            r, _, _ = select.select([fd], [], [], 0.1)
            if r:
                try:
                    d = os.read(fd, 65536)
                except OSError:
                    return False
                if not d:
                    return False
                out += d
                last = time.time()
                if b"\x1b[c" in d or b"\x1b[0c" in d:
                    os.write(fd, b"\x1b[?62;c")  # fish 4 waits for this reply
                if b"\x1b[6n" in d:
                    os.write(fd, b"\x1b[1;1R")
            elif time.time() - last > quiet:
                return True
        return True

    alive = drain(1.5, 20)
    for line in lines:
        if not alive:
            break
        os.write(fd, line.encode() + b"\r")
        alive = drain(0.8, 12)
    with contextlib.suppress(OSError):
        os.write(fd, b"exit\r")
    drain(0.5, 3)
    with contextlib.suppress(OSError):
        os.kill(pid, signal.SIGKILL)
    os.waitpid(pid, 0)
    txt = out.decode("utf-8", "replace")
    ansi = (
        r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][A-Z0-9]|\x1b[=>]|\r"
    )
    return re.sub(ansi, "", txt)


def session(run):
    shell, event, impl, preset, interp, tmp = run
    env = dict(os.environ, HOME=tmp, ZDOTDIR=tmp, TERM="xterm")
    env.update(
        XDG_CONFIG_HOME=tmp + "/cfg", XDG_DATA_HOME=tmp + "/data", XDG_STATE_HOME=tmp + "/state"
    )
    for d in ("cfg/nushell/autoload", "cfg/elvish", "data", "state"):
        (Path(tmp) / d).mkdir(parents=True, exist_ok=True)
    twice = preset == "twice"
    if event == "dir":
        return dir_session(run, env)
    if shell in ("bash", "zsh"):
        L = []
        if shell == "bash":
            L += [
                'user_a() { echo "FIRED:user_a rc=$?"; }',
                'user_b() { echo "FIRED:user_b rc=$?"; }',
            ]
            L += {"string": ["PROMPT_COMMAND='user_a'"], "twice": ["PROMPT_COMMAND='user_a'"]}.get(
                preset, []
            )
            L += {
                "string-semi": ["PROMPT_COMMAND='user_a;'"],
                "array2": ["PROMPT_COMMAND=(user_a user_b)"],
            }.get(preset, [])
            tail = [f'echo "{MARK}"', "false", ":", ":", "exit"]
        else:
            L += {
                "func": ['precmd() { print "FIRED:user_precmd" }'],
                "twice": ['precmd() { print "FIRED:user_precmd" }'],
            }.get(preset, [])
            L += {"array": ['user_b() { print "FIRED:user_b" }', "precmd_functions=(user_b)"]}.get(
                preset, []
            )
            tail = [f'print -r -- "{MARK}"', ":", ":", "exit"]
        L += [". " + impl] * (2 if twice else 1) + tail
        r = subprocess.run(  # noqa: S603  the interpreter command is the check's operand
            interp,
            input=("\n".join(L) + "\n").encode(),
            capture_output=True,
            env=env,
            timeout=120,
            check=False,
        )
        txt = (r.stdout + r.stderr).decode("utf-8", "replace")
    elif shell == "fish":
        L = [
            'function fish_prompt; echo "FIRED:user_prompt> "; end',
            "function user_h --on-event fish_prompt; echo FIRED:user_h; end",
        ]
        L += ["source " + impl] * (2 if twice else 1) + [f'echo "{MARK}"', ":", ":"]
        txt = pty_session(L, interp, env)
    elif shell == "nu":
        cfg = {
            "list": "[ {|| print 'FIRED:user_pp' } ]",
            "twice": "[ {|| print 'FIRED:user_pp' } ]",
            "string": "[ 'print FIRED:user_pp' ]",
        }
        config = ""
        if preset in cfg:
            config = "$env.config.hooks.pre_prompt = " + cfg[preset] + "\n"
            config += "$env.config.hooks.env_change.PWD = [ {|b, a| print 'FIRED:user_pwd' } ]\n"
        (Path(tmp) / "cfg/nushell/config.nu").write_text(config)
        for n in ("x.nu", "y.nu")[: 2 if twice else 1]:
            (Path(tmp) / "cfg/nushell/autoload" / n).write_text(Path(impl).read_text())
        txt = pty_session([f'print "{MARK}"', "cd ..", "print z"], interp, env)
    else:
        rc = ""
        if preset in ("user", "twice"):
            rc = "set edit:before-readline = [ { echo FIRED:user_h } ]\n"
        rc += Path(impl).read_text() * (2 if twice else 1)
        (Path(tmp) / "cfg/elvish/rc.elv").write_text(rc)
        txt = pty_session([f"echo '{MARK}'", ":", ":"], interp, env)
    if MARK not in txt:
        return None
    syntax = "SYNTAX" if "syntax error" in txt else ""
    tail = txt.split(MARK, 1)[1]
    toks = re.findall(r"FIRED:(\w+)(?: rc=(\d+))?", tail)
    return syntax, [t + ("@" + rc if rc else "") for t, rc in toks]


def tokens(txt):
    if MARK not in txt:
        return None
    toks = re.findall(r"FIRED:(\w+)(?: rc=(\d+))?", txt.split(MARK, 1)[1])
    return "", [t + ("@" + rc if rc else "") for t, rc in toks]


def dir_session(run, env):
    shell, _, impl, preset, interp, tmp = run
    twice = preset == "twice"
    moves = ["cd /", "cd /tmp", "pushd /usr", "popd", ":"]
    user = preset in ("func", "user", "twice")
    if shell == "zsh":
        L = []
        if user:
            L.append('chpwd() { print "FIRED:user_chpwd" }')
        if preset == "array":
            L += ['user_c() { print "FIRED:user_chpwd" }', "chpwd_functions=(user_c)"]
        L += [". " + impl] * (2 if twice else 1) + [f'print -r -- "{MARK}"'] + moves + ["exit"]
        r = subprocess.run(  # noqa: S603  the interpreter command is the check's operand
            interp,
            input=("\n".join(L) + "\n").encode(),
            capture_output=True,
            env=env,
            timeout=120,
            check=False,
        )
        return tokens((r.stdout + r.stderr).decode("utf-8", "replace"))
    if shell == "fish":
        L = []
        if user:
            L.append("function user_dir --on-variable PWD; echo FIRED:user_dir; end")
        L += ["source " + impl] * (2 if twice else 1) + [f'echo "{MARK}"']
        L += [m + " >/dev/null" if m.startswith(("pushd", "popd")) else m for m in moves]
        return tokens(pty_session(L, interp, env))
    rc = "set after-chdir = [ {|d| echo FIRED:user_dir } ]\n" if user else ""
    rc += Path(impl).read_text() * (2 if twice else 1)
    (Path(tmp) / "cfg/elvish/rc.elv").write_text(rc)
    return tokens(pty_session([f"echo '{MARK}'", "cd /", "cd /tmp", "cd /usr", ":"], interp, env))


def main(argv):
    if "--" not in argv:
        sys.exit(__doc__)
    cut = argv.index("--")
    opts, interp = argv[:cut], argv[cut + 1 :]
    event = "prompt"
    if "--event" in opts:
        event = opts[opts.index("--event") + 1]
        del opts[opts.index("--event") : opts.index("--event") + 2]
    verbose = "-v" in opts
    pos = [a for a in opts if a != "-v"]
    if (
        len(pos) != 2 or pos[0] not in PRESETS or not interp
    ):  # SHELL IMPL -- INTERP is the shortest form (4 words)
        sys.exit(__doc__)
    shell, impl = pos[0], str(Path(pos[1]).absolute())
    presets = DIRPRESETS[shell] if event == "dir" else PRESETS[shell]
    with tempfile.NamedTemporaryFile("w", suffix=".none", delete=False) as empty:
        pass
    bad = invalid = 0
    runs = {}
    for preset in presets:
        with tempfile.TemporaryDirectory() as t1, tempfile.TemporaryDirectory() as t2:
            base = session(Run(shell, event, empty.name, preset, interp, t1))
            got = session(Run(shell, event, impl, preset, interp, t2))
        runs[preset] = (base, got)
    ud = max([sum(1 for t in b[1] if t.startswith("user_")) for b, _ in runs.values() if b] or [0])
    for preset in presets:
        base, got = runs[preset]
        if base is None or got is None:
            invalid += 1
            print(f"INVALID {shell} {preset}: no {MARK!r} marker in the session output")
            continue
        why = []
        if got[0]:
            why.append("syntax error")
        cb = collections.Counter(t.split("@")[0] for t in base[1])
        cg = collections.Counter(t.split("@")[0] for t in got[1])
        for h, n in cb.items():
            if cg[h] < n:
                why.append(f"{h} fired {cg[h]}/{n} times")
        wants = {"ours_dir": ud} if event == "dir" else WANT[shell]
        for tok, want in wants.items():
            n = cb["user_h"] if want == "=user_h" else want
            if (
                event == "prompt"
                and shell == "fish"
                and (cb["user_prompt"] and not cg["user_prompt"])
            ):
                why.append("the user's fish_prompt is no longer rendered")
            if cg[tok] != n:
                why.append(f"{tok} fired {cg[tok]} times (want {n})")
        if shell == "bash" and "user_a@1" in base[1] and "user_a@1" not in got[1]:
            why.append("status lost: user_a no longer sees rc=1")
        if verbose:
            print(f"{preset:<12} baseline={' '.join(base[1])} ours={' '.join(got[1])}")
        if why:
            bad += 1
            print(f"FAIL {shell} {preset:<11} {'; '.join(why)}")
    Path(empty.name).unlink()
    n = len(presets)
    print(f"{n} scenarios, {bad} failing, {invalid} invalid")
    return 1 if bad or invalid else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
