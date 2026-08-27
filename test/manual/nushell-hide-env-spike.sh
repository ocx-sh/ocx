#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
#
# Red+green spike for the one nushell question the shell-env overhaul could not
# settle by reading: does `hide-env` inside an `env_change.PWD` hook block reach
# the caller's environment, or is it scoped to the block?
#
# The answer decides whether nushell ships FULL reconciler parity or documented-
# partial parity (C-048 / A-23). It is not settleable from the ADR, the code, or
# reasoning — upstream nushell has carried the scoping hazard unresolved since
# 2022 (nushell#6593, #11818, #15872) — so this script executes it.
#
# ANSWER, measured on nushell 0.113.1 (the version test/docker/shells.Dockerfile
# pins) on 2026-08-25: **YES.** Inside an `env_change.PWD` hook closure,
# `hide-env` unsets in the caller's environment and `load-env` sets in it.
# Nushell ships full parity. The required form is `hide-env --ignore-errors`:
# bare `hide-env` is a hard error when the variable is already absent, which
# fires on the second and every later prompt.
#
# Two things make this harness necessary rather than a one-liner:
#   * `env_change` hooks DO NOT FIRE in a nushell script (`nu file.nu`). They run
#     only in the REPL loop, before a prompt. A script-based spike returns
#     "did not propagate" for both the working and the broken form — a green
#     indistinguishable from the check never running.
#   * The REPL needs a pty and answers DSR (ESC[6n) cursor queries before
#     reedline will settle, so plain piped stdin is not enough either.
#
# Re-run it against a newer nushell before widening any parity claim:
#     test/manual/nushell-hide-env-spike.sh              # uses `nu` from PATH
#     NU_BIN=/path/to/nu test/manual/nushell-hide-env-spike.sh
#
# Exits 0 only when the green case propagates AND the red case does not — a
# green alone would not tell a working `hide-env` from a hook that never ran.

set -euo pipefail

NU_BIN="${NU_BIN:-$(command -v nu || true)}"
if [ -z "${NU_BIN}" ] || [ ! -x "${NU_BIN}" ]; then
    echo "nushell not found. Install it, or point NU_BIN at a binary:" >&2
    echo "  NU_BIN=/path/to/nu $0" >&2
    echo "The shell zoo builds one: task test:shells (test/docker/shells.Dockerfile)." >&2
    exit 127
fi

echo "nushell under test: ${NU_BIN} ($("${NU_BIN}" --version))"

workdir="$(mktemp -d)"
trap 'rm -rf "${workdir}"' EXIT

# Drives the REPL over a pty, one line at a time, answering DSR so the prompt
# settles between lines. Without the DSR answer reedline never draws and the
# input is echoed but never evaluated.
#
# The explicit window size is not cosmetic. `pty.fork()` hands the child a pty
# whose winsize is 0x0, and reedline then wraps the long hook-assignment line at
# an unpredictable column - which splits the very `print` output the assertions
# grep for, so the spike reports "did not propagate" for a hook that worked.
# That failure is cwd-sensitive (the prompt carries $PWD), which makes it look
# like a nushell difference when it is a terminal-geometry one. Pin it wide.
#
# The driver is NOT named `pty.py`: a script by that name shadows the stdlib
# `pty` module it imports, so `pty.fork` raises AttributeError, the driver
# writes nothing, and the spike reports "hide-env did not propagate" — a
# nushell verdict produced by a broken harness. It happened here once already.
cat >"${workdir}/nu_repl_drive.py" <<'PYEOF'
import fcntl, os, pty, re, select, struct, sys, termios, time

script_file, cmd, *args = sys.argv[1:]
lines = open(script_file).read().splitlines()

pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "dumb"
    os.execvp(cmd, [cmd] + args)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 400, 0, 0))

out = bytearray()
DSR = re.compile(rb"\x1b\[6n")


def pump(seconds):
    end = time.time() + seconds
    while time.time() < end:
        if not select.select([fd], [], [], 0.15)[0]:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            return False
        if not chunk:
            return False
        out.extend(chunk)
        if DSR.search(chunk):
            os.write(fd, b"\x1b[1;1R")
    return True


pump(2.0)
for line in lines:
    os.write(fd, line.encode() + b"\n")
    if not pump(1.5):
        break
pump(2.0)
sys.stdout.buffer.write(bytes(out))
PYEOF

# $1 = the statement the hook runs against SPIKE_CONST. Everything else is held
# fixed, so the two runs differ in exactly one token.
emit_case() {
    cat <<EOF
\$env.SPIKE_CONST = "initial"
\$env.SPIKE_LIST = "a b c"
\$env.SPIKE_PATH = "/x:/y"
\$env.config.hooks.env_change.PWD = ((\$env.config.hooks.env_change.PWD? | default []) ++ [{|before, after| $1; load-env { SPIKE_LIST: "b c", SPIKE_PATH: "/new:/x:/y" } }])
cd /tmp
print \$"SPIKERESULT1 const=(\$env.SPIKE_CONST? | default '<UNSET>') list=(\$env.SPIKE_LIST? | default '<UNSET>') path=(\$env.SPIKE_PATH? | default '<UNSET>')"
cd /usr
print \$"SPIKERESULT2 const=(\$env.SPIKE_CONST? | default '<UNSET>') list=(\$env.SPIKE_LIST? | default '<UNSET>') path=(\$env.SPIKE_PATH? | default '<UNSET>')"
exit
EOF
}

run_case() {
    emit_case "$1" >"${workdir}/case.txt"
    python3 "${workdir}/nu_repl_drive.py" "${workdir}/case.txt" "${NU_BIN}" --no-config-file -i 2>&1 |
        tr -d '\r' | grep -ao 'SPIKERESULT[12] const=[^ ]* list=[^ ]* [^ ]* path=[^ ]*' |
        grep -v 'SPIKE_CONST?' || true
}

echo
echo "--- GREEN: hide-env --ignore-errors inside the hook ---"
green="$(run_case 'hide-env --ignore-errors SPIKE_CONST')"
echo "${green:-<no output>}"

echo
echo "--- RED: same hook, load-env instead of hide-env (one token changed) ---"
red="$(run_case 'load-env { SPIKE_CONST: "" }')"
echo "${red:-<no output>}"

echo
fail=0

# The hook must have fired at all: `list` and `path` prove it, independently of
# whichever unset form ran. Without this the two cases could agree for the
# uninteresting reason that nothing executed.
case "${green}" in
    *"list=b"*) ;;
    *)
        echo "FAIL: the PWD hook never fired in the green case - load-env did not propagate either" >&2
        fail=1
        ;;
esac

case "${green}" in
    *"const=<UNSET>"*) echo "PASS green: hide-env inside the hook reached the caller's environment" ;;
    *)
        echo "FAIL green: hide-env did not propagate - nushell ships documented-partial parity" >&2
        fail=1
        ;;
esac

case "${red}" in
    *"const=<UNSET>"*)
        echo "FAIL red: the assertion cannot discriminate - it passes without hide-env" >&2
        fail=1
        ;;
    *"const="*) echo "PASS red: without hide-env the variable stays set - the assertion discriminates" ;;
    *)
        echo "FAIL red: no result line at all - the harness, not nushell, is what failed" >&2
        fail=1
        ;;
esac

echo
if [ "${fail}" -eq 0 ]; then
    echo "SPIKE GREEN+RED: nushell supports constant-revert inside a hook block."
    echo "Use 'hide-env --ignore-errors <KEY>' - bare hide-env errors once the key is gone."
else
    echo "SPIKE FAILED - do not widen the nushell parity claim on this version." >&2
fi
exit "${fail}"
