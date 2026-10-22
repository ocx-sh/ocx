#!/usr/bin/env python3
"""Run a command in a pty, send each line of a script file once the output goes quiet, print the ANSI-stripped output.

    python3 -I ptydrive.py SCRIPTFILE -- CMD [ARGS...]

A TAB written as a literal tab character in SCRIPTFILE is sent as a real TAB keypress, so the line `demo <TAB><TAB>`
exercises the live completion engine. Replies to the terminal queries fish 4 and nu send are answered.
"""

import contextlib
import os
import pty
import re
import select
import signal
import sys
import time
from pathlib import Path

ANSI = r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][A-Z0-9]|\x1b[=>]|\r"


def main(argv):
    if len(argv) < 4 or argv[2] != "--":
        sys.exit(__doc__)
    script = Path(argv[1]).read_text().splitlines()
    cmd = argv[3:]
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm"
        os.execvp(cmd[0], cmd)  # noqa: S606  the command is the check's operand
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
                    os.write(fd, b"\x1b[?62;c")
                if b"\x1b[6n" in d:
                    os.write(fd, b"\x1b[1;1R")
            elif time.time() - last > quiet:
                return True
        return True

    alive = drain(1.5, 20)
    for line in script:
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
    sys.stdout.write(re.sub(ANSI, "", out.decode("utf-8", "replace")))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
