#!/usr/bin/env python3
"""errors-signal.py: SH-ERR-16 and SH-ERR-22, judge how a script ends when a signal reaches it.

Usage: python3 -I errors-signal.py [--delay SECONDS] [--clean-dir DIR] MODE SIGNAL SCRIPT [INTERP...]
  MODE    exit   the script must exit with 128 plus the signal number (a handler ending in exit 143)
          raise  the script must die of the signal itself (a handler that re-raises)
  SIGNAL  HUP, INT or TERM
  SCRIPT  a script that installs its traps and then blocks, for example with sleep 30 and wait
  INTERP  interpreter commands, default sh, quoted when they carry a flag
  --clean-dir DIR  the script creates DIR as its temp state, a leftover DIR is a violation

A violation prints one line, a pass prints nothing. The signal goes out after --delay seconds (default 1)
and the script gets 4 more seconds to end.
Exit: 0 every interpreter ended as MODE requires, 1 a violation, 64 usage, 69 no interpreter found,
      79 SCRIPT not found.
Needs Python 3.10 or newer. Watched red and green on dash, busybox ash 1.37 and bash 5.3 (measured 2026-10-11).
"""

import argparse
import shlex
import shutil
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

GRACE = 4.0


@dataclass
class Case:
    script: Path
    mode: str
    sig: signal.Signals
    delay: float
    clean_dir: Path | None


class Parser(argparse.ArgumentParser):
    def error(self, message):
        self.print_usage(sys.stderr)
        print(f"errors-signal: {message}", file=sys.stderr)
        sys.exit(64)


def judge(interp: str, case: Case) -> str | None:
    """Return a violation message for one interpreter, or None."""
    if case.clean_dir is not None:
        shutil.rmtree(case.clean_dir, ignore_errors=True)
    proc = subprocess.Popen(  # noqa: S603  the interpreter and script are the check's operands
        [*shlex.split(interp), str(case.script)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(case.delay)
    if proc.poll() is not None:
        return f"{interp} ended on its own before the signal (status {proc.returncode})"
    proc.send_signal(case.sig)
    try:
        status = proc.wait(timeout=GRACE)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
        return f"{interp} outlived {case.sig.name} by {GRACE:g} s"
    if case.mode == "exit" and status != 128 + case.sig:
        return f"{interp} exit {status} after {case.sig.name}, wanted {128 + case.sig}"
    if case.mode == "raise" and status != -case.sig:
        return f"{interp} status {status} after {case.sig.name}, wanted death by the signal"
    if case.clean_dir is not None and case.clean_dir.exists():
        return f"{interp} left {case.clean_dir} after {case.sig.name}"
    return None


def main() -> int:
    parser = Parser(prog="errors-signal.py")
    parser.add_argument("--delay", type=float, default=1.0)
    parser.add_argument("--clean-dir", type=Path)
    parser.add_argument("mode", choices=["exit", "raise"])
    parser.add_argument("signal", choices=["HUP", "INT", "TERM"])
    parser.add_argument("script", type=Path)
    parser.add_argument("interps", nargs="*")
    args = parser.parse_args()
    if not args.script.is_file():
        print(f"errors-signal: no such script: {args.script}", file=sys.stderr)
        return 79
    # A parent that started with SIGINT ignored would pass the ignore on, and a shell cannot trap it.
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    case = Case(
        args.script.resolve(),
        args.mode,
        signal.Signals[f"SIG{args.signal}"],
        args.delay,
        args.clean_dir,
    )
    ran, bad = 0, 0
    for interp in args.interps or ["sh"]:
        words = shlex.split(interp)
        if not words or shutil.which(words[0]) is None:
            continue
        ran += 1
        message = judge(interp, case)
        if message:
            print(f"errors-signal: {message}")
            bad = 1
    if ran == 0:
        print("errors-signal: no interpreter found", file=sys.stderr)
        return 69
    return bad


if __name__ == "__main__":
    sys.exit(main())
