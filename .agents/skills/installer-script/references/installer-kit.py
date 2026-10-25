#!/usr/bin/env python3
"""installer-kit.py CHECK INSTALLER [--shell CMD] [--profile FILE] [--nth N]

Behavioural checks for a hand-written install.sh against stub curl, wget, uname, ldd, sysctl
and sudo, in a throwaway HOME. Nothing touches the network or the real HOME.
Run it as: python3 -I installer-kit.py CHECK INSTALLER --shell dash

CHECK is one of:
  parse     the shell parses the file under LC_ALL=C (a non-ASCII byte kills yash there)
  trunc     every line prefix, four byte prefixes and the last 60 byte prefixes, piped to the
            shell, leave either the seeded old install or the finished one, never a mix
  tamper    a swapped archive, no hash tool on PATH and a blank digest each refuse and install nothing
  hash      tamper once per hash tool: every other hash tool is removed from PATH, so the sha256sum, shasum
            and openssl branches each run alone; a swapped archive must refuse and the good payload must
            install (a tool the host lacks reads n/a)
  flags     curl carries --proto =https and --tlsv1.2 only when listed, BusyBox wget gets no
            option it rejects (piped and on a tty), an overridden http URL is refused
  platform  14 uname, ldd and sysctl scenarios: each maps to a target or fails loudly
  root      no sudo call as an unprivileged user
  rc        a second run leaves the rc files byte-identical, the opt-out flag touches none,
            a symlinked rc survives
  tty       piped install under a controlling terminal and under none: no /dev/tty leak
  trap      INT, TERM and HUP mid-download leave a signal status and no temp directory
  all       every check above, then red=K of N

Profile (JSON, every key optional, defaults describe install-template.sh):
  name, version, asset ("{name}-{version}-{target}.tar.gz"), prefix (directory inside the
  archive), digest ("sidecar" or "embedded"), sidecar ("{asset}.sha256", also {name} {version} {target}), extra ({served file name: body}), installed (path under
  HOME), args, env, http_env (URL variables that must refuse http), no_modify_flag, targets,
  platform (list of [label, uname -s, uname -m, ldd, sysctl, expected token or FAIL]).
Exit: 0 pass, 1 a finding, 69 a tool or the shell is missing, 74 I/O failed, 78 bad invocation or profile.
Tested on Linux with dash, busybox ash and bash --posix. A pty is needed for tty and the tty part of flags.
"""

import contextlib
import fcntl
import hashlib
import io
import json
import os
import pty
import re
import select
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import termios
import time
import zipfile
from pathlib import Path

TARGETS = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
]
PLATFORM = [
    ["linux x64 glibc", "Linux", "x86_64", "glibc", "unknown", "x86_64-unknown-linux-gnu"],
    ["linux arm64 glibc", "Linux", "aarch64", "glibc", "unknown", "aarch64-unknown-linux-gnu"],
    ["linux x64 musl", "Linux", "x86_64", "musl", "unknown", "x86_64-unknown-linux-musl"],
    ["linux arm64 musl", "Linux", "aarch64", "musl", "unknown", "aarch64-unknown-linux-musl"],
    [
        "linux x64 no ldd",
        "Linux",
        "x86_64",
        "none",
        "unknown",
        "x86_64-unknown-linux-gnu~x86_64-unknown-linux-musl",
    ],
    ["linux armv7l", "Linux", "armv7l", "glibc", "unknown", "FAIL"],
    ["linux i686", "Linux", "i686", "glibc", "unknown", "FAIL"],
    ["linux riscv64", "Linux", "riscv64", "glibc", "unknown", "FAIL"],
    ["darwin arm64", "Darwin", "arm64", "glibc", "unknown", "aarch64-apple-darwin"],
    ["darwin x64 intel", "Darwin", "x86_64", "glibc", "unknown", "x86_64-apple-darwin"],
    ["darwin x64 rosetta oid 1", "Darwin", "x86_64", "glibc", "1", "aarch64-apple-darwin"],
    ["darwin x64 oid 0", "Darwin", "x86_64", "glibc", "0", "x86_64-apple-darwin"],
    ["mingw64", "MINGW64_NT-10.0-26100", "x86_64", "glibc", "unknown", "FAIL"],
    ["freebsd amd64", "FreeBSD", "amd64", "glibc", "unknown", "FAIL"],
]
DEFAULTS = {
    "name": "toolx",
    "version": "1.2.3",
    "asset": "{name}-{version}-{target}.tar.gz",
    "prefix": "",
    "digest": "sidecar",
    "sidecar": "{asset}.sha256",
    "extra": {},
    "installed": ".local/bin/toolx",
    "args": [],
    "env": {},
    "http_env": ["TOOLX_INSTALL_BASE_URL"],
    "no_modify_flag": "--no-modify-path",
    "targets": TARGETS,
    "platform": PLATFORM,
    "hash_tools": ["sha256sum", "shasum", "openssl", "sha512sum", "sha1sum", "md5sum"],
    "hash_branches": ["sha256sum", "shasum", "openssl"],
}
BASE_TOOLS = [
    "awk",
    "cat",
    "chmod",
    "cmp",
    "cp",
    "cut",
    "grep",
    "head",
    "ls",
    "mkdir",
    "mktemp",
    "mv",
    "printf",
    "rm",
    "sed",
    "sleep",
    "tail",
    "tar",
    "tr",
    "sha256sum",
    "sha512sum",
    "sha1sum",
    "md5sum",
    "shasum",
    "openssl",
    "dirname",
    "basename",
    "id",
    "tee",
    "wc",
    "sort",
    "uniq",
    "true",
    "false",
    "env",
    "gzip",
    "xz",
    "timeout",
    "unzip",
    "rmdir",
    "ln",
    "date",
    "xargs",
    "getconf",
    "od",
    "base64",
    "find",
    "install",
    "touch",
    "readlink",
    "expr",
    "kill",
    "sh",
    "bash",
    "tput",
]
STUB_LOG = 'printf \'%s %s\\n\' "$(basename "$0")" "$*" >>"${STUB_LOG:-/dev/null}"\n'
STUBS = {
    "uname": """case ${1:-} in
    -s) echo "${FAKE_UNAME_S:?}" ;;
    -m) echo "${FAKE_UNAME_M:?}" ;;
    -sm | -ms) echo "$FAKE_UNAME_S $FAKE_UNAME_M" ;;
    -r) echo 6.1.0-fake ;;
    -o) case $FAKE_UNAME_S in MINGW* | MSYS*) echo Msys ;; Linux) echo GNU/Linux ;; *) echo "$FAKE_UNAME_S" ;; esac ;;
    *) echo "$FAKE_UNAME_S" ;;
esac
""",
    "ldd": """case ${FAKE_LDD:-glibc} in
    musl) echo "musl libc (x86_64)" >&2; echo "Version 1.2.5" >&2; exit 1 ;;
    *) echo "ldd (Debian GLIBC 2.36-9+deb12u14) 2.36"; exit 0 ;;
esac
""",
    "sysctl": """case ${FAKE_SYSCTL:-unknown} in
    unknown) echo "sysctl: unknown oid 'hw.optional.arm64'" >&2; exit 1 ;;
    *) if [ "${1:-}" = -n ]; then echo "$FAKE_SYSCTL"; else echo "hw.optional.arm64: $FAKE_SYSCTL"; fi ;;
esac
""",
    "sudo": 'echo "sudo: a password is required" >&2\nexit 1\n',
    "curl": """# Serves $STUB_ROOT/<basename of the URL>, else 404. STUB_CURL_HELP=nolist: --tlsv1.2 is neither listed nor accepted.
out=; url=; help=; wfmt=
while [ $# -gt 0 ]; do
    case $1 in
        --version) echo "curl 8.5.0 (x86_64-pc-linux-gnu) libcurl/8.5.0 OpenSSL/3.0.13"; exit 0 ;;
        --help) help=1; shift; if [ "${1:-}" = all ]; then help=all; shift; fi; continue ;;
        --tlsv1.2) if [ "${STUB_CURL_HELP:-}" = nolist ]; then echo "REJECT $1" >>"${STUB_LOG:-/dev/null}"; echo "curl: option $1: is unknown" >&2; exit 2; fi ;;
        -o | --output) out=$2; shift ;;
        -w | --write-out) wfmt=$2; shift ;;
        -H | --header | --proto | --retry | --retry-delay | --max-time | --connect-timeout | -A | --user-agent | -u | -X | -m) shift ;;
        https://* | http://*) url=$1 ;;
        --*) ;;
        -[!-]*) case $1 in -o?*) out=${1#-o} ;; *o) out=$2; shift ;; esac ;;
    esac
    shift
done
if [ -n "$help" ]; then
    echo "Usage: curl [options...] <url>"; echo " -f, --fail"; echo " -o, --output <file>"
    if [ "$help" = all ]; then
        echo "     --proto <protocols>"; echo "     --retry <num>"; echo "     --retry-connrefused"
        [ "${STUB_CURL_HELP:-}" = nolist ] || echo "     --tlsv1.2"
    fi
    exit 0
fi
[ -n "$url" ] || exit 2
[ -z "${STUB_SLEEP:-}" ] || sleep "$STUB_SLEEP"
base=${url##*/}; base=${base%%\\?*}
# -w prints the status line the godownloader family reads: the format is taken to be exactly %{http_code}
[ -f "$STUB_ROOT/$base" ] || { echo "curl: (22) The requested URL returned error: 404" >&2; [ -z "$wfmt" ] || printf 404; exit 22; }
if [ -n "$out" ] && [ "$out" != - ]; then cp "$STUB_ROOT/$base" "$out"; else cat "$STUB_ROOT/$base"; fi
[ -z "$wfmt" ] || printf 200
""",
    "wget": """# BusyBox v1.37.0 option set. Anything else is "unrecognized option", logged as REJECT. STUB_WGET=gnu also accepts --https-only.
mode=${STUB_WGET:-busybox}; out=; url=; help=; sresp=
bad() { echo "REJECT $1" >>"${STUB_LOG:-/dev/null}"; echo "wget: unrecognized option: $1" >&2; echo "BusyBox v1.37.0 (stub) multi-call binary." >&2; exit 1; }
while [ $# -gt 0 ]; do
    case $1 in
        --help) help=1 ;;
        -S | --server-response) sresp=1 ;;
        --header=* | --no-check-certificate | --spider | -q | -c | --no-verbose | -nv | --quiet | --continue | --tries=* | --user-agent=* | --timeout=*) ;;
        --header) shift ;;
        -O) out=$2; shift ;;
        -O?*) out=${1#-O} ;;
        -qO) out=$2; shift ;;
        -qO?*) out=${1#-qO} ;;
        -P | -o | -U | -T | -Y) shift ;;
        https://* | http://*) url=$1 ;;
        --output-document=*) out=${1#--output-document=} ;;
        --https-only) [ "$mode" = busybox ] && bad "$1" ;;
        --*) [ "$mode" = busybox ] && bad "$1" ;;
        -*) [ "$mode" = busybox ] && bad "$1" ;;
    esac
    shift
done
if [ -n "$help" ]; then
    if [ "$mode" = busybox ]; then echo "BusyBox v1.37.0 (stub) multi-call binary." >&2; echo "Usage: wget [-cqS] [--spider] [-O FILE] URL..." >&2; exit 1; fi
    echo "GNU Wget stub"; echo "     --https-only"; exit 0
fi
[ -n "$url" ] || exit 1
[ -z "${STUB_SLEEP:-}" ] || sleep "$STUB_SLEEP"
base=${url##*/}; base=${base%%\\?*}
[ -f "$STUB_ROOT/$base" ] || { [ -z "$sresp" ] || echo "  HTTP/1.1 404 Not Found" >&2; echo "wget: server returned error: HTTP/1.1 404 Not Found" >&2; exit 1; }
[ -z "$sresp" ] || echo "  HTTP/1.1 200 OK" >&2
if [ -n "$out" ] && [ "$out" != - ]; then cp "$STUB_ROOT/$base" "$out"; else cat "$STUB_ROOT/$base"; fi
""",
}
DOWNLOAD = ("curl", "wget")


class Harness:
    """One throwaway world: stub dir, tool dir, fake release server, HOME, TMPDIR and a call log."""

    def __init__(self, inst, shell, prof):
        self.inst = str(Path(inst).absolute())
        parts = shell.split()
        exe = shutil.which(parts[0])
        if exe is None:
            sys.exit(69)
        self.shell = [exe, *parts[1:]]
        self.shname = Path(exe).name
        self.p = prof
        self.w = Path(tempfile.mkdtemp(prefix="ikit-"))
        for d in ("base", "st", "srv", "tmp", "home"):
            (self.w / d).mkdir(parents=True)
        self.home = self.w / "home"
        self.log = self.w / "log"
        for t in BASE_TOOLS:
            src = shutil.which(t, path="/usr/bin:/bin:/usr/sbin:/sbin")
            if src:
                (self.w / "base" / t).symlink_to(src)
        for name, body in STUBS.items():
            self.write_stub(name, body)

    def write_stub(self, name, body):
        path = self.w / "st" / name
        path.write_text("#!/bin/sh\n" + STUB_LOG + body)
        path.chmod(0o755)  # a stub tool has to be executable

    def reset_log(self):
        self.log.write_text("")
        self.log.chmod(0o666)  # the root check runs the installer as another user

    def calls(self, *names):
        if not self.log.exists():
            return []
        lines = self.log.read_text().splitlines()
        return [ln for ln in lines if not names or ln.split(" ", 1)[0] in names]

    def downloads(self):
        return [
            ln
            for ln in self.calls(*DOWNLOAD)
            if " --help" not in ln and " --version" not in ln and "://" in ln
        ]

    def env(self, extra=None):
        e = {
            "HOME": str(self.home),
            "PATH": f"{self.w / 'st'}{os.pathsep}{self.w / 'base'}",
            "TMPDIR": str(self.w / "tmp"),
            "STUB_ROOT": str(self.w / "srv"),
            "STUB_LOG": str(self.log),
            "FAKE_UNAME_S": "Linux",
            "FAKE_UNAME_M": "x86_64",
            "FAKE_LDD": "glibc",
            "TERM": "xterm",
        }
        e.update(self.p["env"])
        e.update(extra or {})
        return e

    def set_hash_tools(self, keep=()):
        """Remove every hash tool from PATH, then link back the host's copy of each tool named in keep."""
        for t in self.p["hash_tools"]:
            for d in ("base", "st"):
                p = self.w / d / t
                if os.path.lexists(p):
                    p.unlink()
        for t in keep:
            src = shutil.which(t, path="/usr/bin:/bin:/usr/sbin:/sbin")
            if src:
                (self.w / "base" / t).symlink_to(src)

    def only_stubs(self, keep_out):
        """Make a second stub dir without the named downloaders and return it as the PATH front."""
        d = self.w / ("st-" + "-".join(keep_out))
        d.mkdir(parents=True, exist_ok=True)
        for n in (self.w / "st").iterdir():
            if n.name not in keep_out:
                shutil.copy2(n, d / n.name)
        return str(d)

    # --- fake release server -------------------------------------------------
    def asset(self, target):
        return self.p["asset"].format(name=self.p["name"], version=self.p["version"], target=target)

    def archive_bytes(self, asset, target, content):
        member = self.p["prefix"].format(
            target=target, name=self.p["name"], version=self.p["version"]
        )
        member += Path(self.p["installed"]).name
        data = f"#!/bin/sh\necho {content}\n".encode()
        buf = io.BytesIO()
        if asset.endswith(".zip"):
            with zipfile.ZipFile(buf, "w") as z:
                zi = zipfile.ZipInfo(member)
                zi.external_attr = 0o755 << 16
                z.writestr(zi, data)
        else:
            mode = "w:xz" if asset.endswith(".tar.xz") else "w:gz"
            with tarfile.open(fileobj=buf, mode=mode) as t:
                ti = tarfile.TarInfo(member)
                ti.size = len(data)
                ti.mode = 0o755
                t.addfile(ti, io.BytesIO(data))
        return buf.getvalue()

    def serve(self, evil=False, blank=False):
        srv = self.w / "srv"
        shutil.rmtree(srv, ignore_errors=True)
        srv.mkdir(parents=True)
        for t in self.p["targets"]:
            a = self.asset(t)
            good = self.archive_bytes(a, t, f"{self.p['name']} {self.p['version']}")
            digest = hashlib.sha256(good).hexdigest()
            body = self.archive_bytes(a, t, "evil") if evil else good
            (srv / a).write_bytes(body)
            if self.p["digest"] == "sidecar":
                sc = self.p["sidecar"].format(
                    asset=a, name=self.p["name"], version=self.p["version"], target=t
                )
                (srv / sc).write_text("" if blank else f"{digest}  {a}\n")
        for n, body in self.p["extra"].items():  # files the installer fetches besides the archive
            (srv / n).write_text(body)

    # --- state ----------------------------------------------------------------
    def installed(self):
        f = self.home / self.p["installed"]
        return f.is_file() and os.access(f, os.X_OK)

    def fresh_home(self, old=True):
        shutil.rmtree(self.home, ignore_errors=True)
        (self.home / ".local" / "bin").mkdir(parents=True)
        (self.home / ".bashrc").write_text("# rc\n")
        if old:
            f = self.home / self.p["installed"]
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text("OLD\n")
            f.chmod(0o755)  # the seeded old install has to be executable
        shutil.rmtree(self.w / "tmp", ignore_errors=True)
        (self.w / "tmp").mkdir(parents=True)

    def snapshot(self):
        h = hashlib.sha256()
        for root, dirs, files in os.walk(self.home):
            dirs.sort()
            for d in dirs:
                h.update(b"D" + str((Path(root) / d).relative_to(self.home)).encode())
            for n in sorted(files + [d for d in dirs if (Path(root) / d).is_symlink()]):
                p = Path(root) / n
                h.update(str(p.relative_to(self.home)).encode())
                if p.is_symlink():
                    h.update(b"L" + str(p.readlink()).encode())
                else:
                    h.update(b"X" if os.access(p, os.X_OK) else b"-")
                    h.update(p.read_bytes())
        return h.hexdigest()

    # --- running ---------------------------------------------------------------
    def run(self, args=None, extra=None, pipe=None, timeout=60, preexec=None):
        """Run the installer as a file, or piped in (pipe=True: the file, pipe=bytes: those bytes).

        Returns (rc, out, err).
        """
        a = self.p["args"] if args is None else args
        self.reset_log()
        stdin_arg = ["/dev/stdin"] if self.shname == "posh" else ["-s", "--"]
        piped = pipe is not None
        cmd = [*self.shell, *(stdin_arg if piped else [self.inst]), *a]
        data = Path(self.inst).read_bytes() if pipe is True else pipe
        try:
            r = subprocess.run(  # noqa: S603  the shell under test is the operand
                cmd,
                input=data,
                stdin=None if piped else subprocess.DEVNULL,
                capture_output=True,
                env=self.env(extra),
                timeout=timeout,
                preexec_fn=preexec,
                check=False,
            )
        except subprocess.TimeoutExpired:
            return 124, "", "timeout"
        return r.returncode, r.stdout.decode(errors="replace"), r.stderr.decode(errors="replace")

    def close(self):
        shutil.rmtree(self.w, ignore_errors=True)


def last(s):
    ln = [x for x in s.splitlines() if x.strip()]
    return ln[-1][:110] if ln else ""


# --- checks: each returns (lines, red) --------------------------------------------------------


def check_parse(h, opts):
    env = dict(os.environ, LC_ALL="C")
    r = subprocess.run(  # noqa: S603  the shell under test is the operand
        [*h.shell, "-n", h.inst], capture_output=True, env=env, check=False
    )
    non_ascii = sum(
        1 for b in Path(h.inst).read_bytes() if b > 126 or (b < 32 and b not in (9, 10, 13))
    )
    if r.returncode == 0:
        return [f"ok   parses under LC_ALL=C (non-ASCII bytes: {non_ascii})"], 0
    why = last(r.stderr.decode(errors="replace"))
    return [
        f"RED  does not parse under LC_ALL=C (rc={r.returncode}, non-ASCII bytes: {non_ascii}): {why}"
    ], 1


def check_trunc(h, opts):
    out, bad, noise = [], 0, 0
    h.serve()
    full = Path(h.inst).read_bytes()
    h.fresh_home()
    seed = h.snapshot()
    h.fresh_home()
    rc, _, err = h.run(pipe=True, timeout=30)
    final = h.snapshot()
    vacuous = final == seed
    if vacuous:
        out.append(
            f"RED  VACUOUS: the uncut script changes nothing here (rc={rc}: {last(err)}), "
            "so the sweep proves nothing"
        )
    lines = full.count(b"\n")
    step = max(1, lines // 1000)
    cuts = []
    n = 1
    while n < lines:
        cuts.append((f"line-cut {n}/{lines}", b"".join(full.splitlines(True)[:n])))
        n += step
    for pct in (20, 40, 60, 80):
        cuts.append((f"byte-cut {pct}%", full[: len(full) * pct // 100]))
    for k in range(1, min(60, len(full)) + 1):
        cuts.append((f"tail-byte-cut -{k}", full[: len(full) - k]))
    for label, data in cuts:
        h.fresh_home()
        h.run(pipe=data, timeout=30)
        net = len(h.downloads())
        if net or any((h.w / "tmp").iterdir()):
            noise += 1
        s = h.snapshot()
        if s not in (seed, final):
            bad += 1
            if bad <= 3:
                out.append(f"RED  PARTIAL at {label}")
    mark = "RED " if bad or vacuous else "ok  "
    out.append(
        f"{mark}  cuts={len(cuts)} partial_cuts={bad} noise_cuts={noise} vacuous={int(vacuous)}"
    )
    return out, int(bad > 0 or vacuous)


INTEGRITY = re.compile(
    r"checksum|sha-?256|sha-?512|digest|hash|verif|mismatch|integrity|tamper", re.IGNORECASE
)


def refused(h, rc):
    return rc != 0 and not h.installed()


def verdict(h, out, label, rc, err):
    """Append one refusal run to out; return 1 when it is a finding."""
    if refused(h, rc) and (h.p["digest"] != "embedded" or INTEGRITY.search(err)):
        out.append(f"ok   {label}: rc={rc}, nothing installed")
        bad = 0
    elif refused(h, rc):
        # embedded digests have no good-payload control, so a failure of any kind looks like a refusal
        bad = 1
        out.append(
            f"RED  {label}: rc={rc}, nothing installed, but no integrity message in stderr "
            f"({last(err)}); the failure may be unrelated, serve the installer's other files through the profile key extra"
        )
    else:
        bad = 1
        out.append(f"RED  {label}: rc={rc} installed={h.installed()}")
    out.append("     stderr: " + last(err))
    return bad


def check_tamper(h, opts):
    out, bad = [], 0
    h.fresh_home(old=False)
    h.serve(evil=True)
    rc, _, err = h.run()
    bad += verdict(h, out, "A swapped archive", rc, err)
    h.fresh_home(old=False)
    # embedded digests: only a swapped archive can show a fail-open
    h.serve(evil=h.p["digest"] == "embedded")
    h.set_hash_tools()
    rc, _, err = h.run()
    bad += verdict(h, out, "B no hash tool on PATH", rc, err)
    h.set_hash_tools(h.p["hash_tools"])
    if h.p["digest"] == "sidecar":
        h.fresh_home(old=False)
        h.serve(blank=True)
        rc, _, err = h.run()
        bad += verdict(h, out, "C blank digest", rc, err)
    else:
        out.append("n/a  C blank digest: digest is embedded in the installer")
    if h.p["digest"] == "embedded":
        out.append(
            "n/a  control: the harness cannot build a payload that matches digests embedded in the installer"
        )
        return out, int(bad > 0)
    h.fresh_home(old=False)
    h.serve()
    rc, _, err = h.run()
    if rc == 0 and h.installed():
        out.append("ok   control: the good payload installs (rc=0)")
    else:
        bad += 1
        out.append(f"RED  control broken, the harness cannot judge: rc={rc} {last(err)}")
    return out, int(bad > 0)


def check_hash(h, opts):
    """tamper with exactly one hash tool on PATH per run, so each fallback branch of the installer runs alone."""
    out, bad = [], 0
    for tool in h.p["hash_branches"]:
        if not shutil.which(tool, path="/usr/bin:/bin:/usr/sbin:/sbin"):
            out.append(f"n/a  only {tool}: not installed on this host")
            continue
        h.set_hash_tools([tool])
        h.fresh_home(old=False)
        h.serve(evil=True)
        rc, _, err = h.run()
        bad += verdict(h, out, f"only {tool}: swapped archive", rc, err)
        if h.p["digest"] == "sidecar":
            h.fresh_home(old=False)
            h.serve()
            rc, _, err = h.run()
            if rc == 0 and h.installed():
                out.append(f"ok   only {tool}: the good payload installs (rc=0)")
            else:
                bad += 1
                out.append(
                    f"RED  only {tool}: the good payload does not install, rc={rc} {last(err)}"
                )
    h.set_hash_tools(h.p["hash_tools"])
    return out, int(bad > 0)


def pty_run(h, mode, answer="n", wget_only=False, extra=None):
    """Pipe the installer into the shell with stdout and stderr on a pty. mode: ctty or noctty. Returns (rc, transcript, prompted)."""
    master, slave = pty.openpty()

    def child():
        os.setsid()
        if mode == "ctty":
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    stdin_arg = ["/dev/stdin"] if h.shname == "posh" else ["-s", "--"]
    env = h.env(extra)
    if wget_only:
        env["PATH"] = f"{h.only_stubs(['curl'])}{os.pathsep}{h.w / 'base'}"
    p = subprocess.Popen(  # noqa: S603  the shell under test is the operand
        ["/bin/sh", "-c", 'cat "$0" | "$@"', h.inst, *h.shell, *stdin_arg, *h.p["args"]],
        stdin=subprocess.DEVNULL,
        stdout=slave,
        stderr=slave,
        env=env,
        # setsid plus TIOCSCTTY has to run in the child between fork and exec, and the kit is single-threaded
        preexec_fn=child,  # noqa: PLW1509
        close_fds=True,
    )
    os.close(slave)
    buf, answered, deadline = b"", False, time.time() + 20
    while time.time() < deadline:
        r, _, _ = select.select([master], [], [], 0.2)
        if r:
            try:
                d = os.read(master, 4096)
            except OSError:
                d = b""
            if not d and p.poll() is not None:
                break
            buf += d
            if not answered and (b"[Y/n]" in buf or b"[y/N]" in buf):
                os.write(master, answer.encode() + b"\n")
                answered = True
        if p.poll() is not None and not r:
            break
    rc = p.poll()
    if rc is None:
        p.kill()
        rc = 124
    os.close(master)
    return rc, buf.decode(errors="replace").replace("\r\n", "\n"), answered


def check_flags(h, opts):
    out, bad = [], 0
    h.fresh_home(old=False)
    h.serve()
    # 1 curl whose basic help lacks the flags and whose "help all" lists them
    h.run(extra={"STUB_CURL_HELP": "modern"})
    dl = [ln for ln in h.downloads() if ln.startswith("curl ")]
    if not dl:
        out.append("na   1 curl: the installer made no curl download")
    else:
        noproto = [ln for ln in dl if "--proto =https" not in ln]
        notls = [ln for ln in dl if "--tlsv1.2" not in ln]
        if noproto or notls:
            bad += 1
            out.append(
                f"RED  1 curl: {len(dl)} download calls, {len(noproto)} lack --proto =https, "
                f"{len(notls)} lack --tlsv1.2"
            )
        else:
            out.append(
                f"ok   1 curl: all {len(dl)} download calls carry --proto =https and --tlsv1.2"
            )
        # 1b curl that does not list --tlsv1.2 anywhere: the flag must not be sent
        h.fresh_home(old=False)
        h.run(extra={"STUB_CURL_HELP": "nolist"})
        if h.calls("REJECT"):
            bad += 1
            out.append("RED  1b curl without --tlsv1.2 in its help: the flag was sent anyway")
        else:
            out.append("ok   1b curl without --tlsv1.2 in its help: the flag was not sent")
    # 2 wget only (BusyBox option set)
    wget_only = f"{h.only_stubs(['curl'])}{os.pathsep}{h.w / 'base'}"
    h.fresh_home(old=False)
    rc, _, err = h.run(extra={"STUB_WGET": "busybox", "PATH": wget_only})
    rejected = h.calls("REJECT")
    if rejected:
        bad += 1
        out.append("RED  2 BusyBox wget: option rejected: " + "; ".join(rejected[:2]))
    elif not h.calls("wget"):
        out.append(f"na   2 BusyBox wget: the installer has no wget path (rc={rc}: {last(err)})")
    elif rc == 0 and h.installed():
        out.append("ok   2 BusyBox wget: installed, no unsupported option sent")
    else:
        bad += 1
        out.append(
            f"RED  2 BusyBox wget: no option rejected but the install failed rc={rc}: {last(err)}"
        )
    # 2t the same on a tty
    h.fresh_home(old=False)
    h.reset_log()
    rc, _, _ = pty_run(h, "ctty", wget_only=True)
    rejected = h.calls("REJECT")
    if rejected:
        bad += 1
        out.append("RED  2t BusyBox wget on a tty: " + "; ".join(rejected[:2]))
    elif h.installed():
        out.append("ok   2t BusyBox wget on a tty: installed, no unsupported option sent")
    else:
        out.append(
            f"na   2t BusyBox wget on a tty: no wget path or the install did not finish (rc={rc})"
        )
    # 4 an overridden http URL must be refused before any request
    if h.p["http_env"]:
        for v in h.p["http_env"]:
            h.fresh_home(old=False)
            rc, _, err = h.run(extra={v: "http://downloads.example.invalid/x"})
            plain = [ln for ln in h.downloads() if "http://" in ln]
            if rc != 0 and not plain:
                out.append(f"ok   4 {v}=http://: refused before any request (rc={rc})")
            else:
                bad += 1
                first = plain[0][:90] if plain else "none"
                out.append(f"RED  4 {v}=http://: rc={rc}, plain-http request: {first}")
    else:
        out.append("na   4 no overridable download URL")
    return out, int(bad > 0)


def check_platform(h, opts):
    out, bad, loud, r7 = [], 0, 0, 0
    hint = [h.asset(t) for t in h.p["targets"]]
    for label, s, m, ldd, sc, want in h.p["platform"]:
        h.fresh_home(old=False)
        h.serve()
        extra = {"FAKE_UNAME_S": s, "FAKE_UNAME_M": m, "FAKE_LDD": ldd, "FAKE_SYSCTL": sc}
        if ldd == "none":
            d = h.w / "st-noldd"
            shutil.rmtree(d, ignore_errors=True)
            shutil.copytree(h.w / "st", d)
            (d / "ldd").unlink()
            rc, _, _ = h.run(extra=dict(extra, PATH=f"{d}{os.pathsep}{h.w / 'base'}"))
        else:
            rc, _, _ = h.run(extra=extra)
        reqs = " ".join(ln.split("://", 1)[1].split()[0] for ln in h.downloads() if "://" in ln)
        asked = any(a in reqs for a in hint)
        if want == "FAIL":
            if rc == 0:
                bad += 1
                out.append(f"RED  {label}: want a failure, got rc=0 requested: {reqs[:90]}")
            else:
                loud += 1
                r7 += rc == 7
                guessed = " (an archive URL was guessed)" if asked else ""
                out.append(f"ok   {label}: rc={rc}{guessed}")
        elif not any(tok in reqs for tok in want.split("~")):
            bad += 1
            out.append(f"RED  {label}: want {want}, rc={rc} requested: {reqs[:90]}")
    mark = "RED " if bad else "ok  "
    out.append(f"{mark} scenarios={len(h.p['platform'])} wrong={bad} loud_fail={loud} rc7={r7}")
    return out, int(bad > 0)


def check_root(h, opts):
    h.fresh_home(old=False)
    h.serve()
    pre = None
    if os.geteuid() == 0:
        for root, dirs, files in os.walk(h.w):
            for n in dirs + files:
                os.lchown(Path(root) / n, 65534, 65534)
        os.chown(h.w, 65534, 65534)

        def pre():
            os.setgid(65534)
            os.setuid(65534)

    rc, _, _ = h.run(preexec=pre)
    sudo = h.calls("sudo")
    uid = 65534 if pre else os.geteuid()
    line = f"uid={uid} rc={rc} sudo-calls={len(sudo)} installed={h.installed()}"
    if sudo:
        return [f"RED  escalated: {sudo[0][:80]} | {line}"], 1
    return ["ok   no sudo call | " + line], 0


def rc_text(h):
    return {
        n: (h.home / n).read_bytes()
        for n in (".profile", ".bashrc", ".zshrc", ".zprofile", ".bash_profile")
        if (h.home / n).is_file()
    }


def check_rc(h, opts):
    out, bad = [], 0
    h.fresh_home(old=False)
    h.serve()
    (h.home / ".bashrc").write_text('alias ll="ls -l"')  # no trailing newline on purpose
    bash_user = {"SHELL": "/bin/bash"}
    rc1, _, _ = h.run(extra=bash_user)
    a = rc_text(h)
    rc2, _, _ = h.run(extra=bash_user)
    b = rc_text(h)
    if rc1 == 0 and rc2 == 0 and a == b:
        files = ", ".join(sorted(a)) or "none"
        out.append(
            f"ok   second run leaves the rc files byte-identical (rc {rc1}/{rc2}, files: {files})"
        )
    else:
        bad += 1
        files = ", ".join(sorted(b))
        out.append(
            f"RED  rc files differ after run 2 or a run failed (rc {rc1}/{rc2}, files: {files})"
        )
    written = sorted(n for n in b if n != ".bashrc" or b[n] != b'alias ll="ls -l"')
    foreign = [
        n for n in (".zprofile", ".zshrc", ".zshenv", ".config") if os.path.lexists(h.home / n)
    ]
    if foreign:
        bad += 1
        out.append("RED  files of other shells were created for a bash user: " + ", ".join(foreign))
    out.append(
        "info files written or changed for a bash user: "
        + (", ".join(written) or "none")
        + " (read: only the login shell's file may change)"
    )
    flag = h.p["no_modify_flag"]
    if flag:
        h.fresh_home(old=False)
        h.serve()
        (h.home / ".bashrc").write_text("x=1\n")
        rc, _, _ = h.run(args=[*h.p["args"], flag], extra=bash_user)
        if rc == 0 and rc_text(h) == {".bashrc": b"x=1\n"}:
            out.append(f"ok   {flag}: no rc file created or changed")
        else:
            bad += 1
            touched = ", ".join(sorted(rc_text(h)))
            out.append(f"RED  {flag} touched rc files (rc={rc}: {touched})")
    h.fresh_home(old=False)
    h.serve()
    (h.home / "dot").mkdir(parents=True)
    (h.home / "dot" / "profile").write_text("export A=1\n")
    (h.home / ".profile").symlink_to("dot/profile")
    rc, _, _ = h.run(extra=bash_user)
    if (h.home / ".profile").is_symlink():
        out.append(f"ok   a symlinked .profile is still a symlink (rc={rc})")
    else:
        bad += 1
        out.append("RED  a symlinked .profile was replaced by a regular file")
    return out, int(bad > 0)


def check_tty(h, opts):
    out, bad = [], 0
    h.fresh_home(old=False)
    h.serve()
    rc, text, prompted = pty_run(h, "ctty")
    seen = "appeared and n was sent" if prompted else "did not appear"
    out.append(f"info ctty: prompt {seen}")
    if h.installed():
        out.append("ok   ctty: the install completed")
    else:
        bad += 1
        out.append(f"RED  ctty: the install did not complete (rc={rc}): {last(text)}")
    h.fresh_home(old=False)
    rc, text, prompted = pty_run(h, "noctty")
    leak = [ln for ln in text.splitlines() if "/dev/tty" in ln or "No such device" in ln]
    if prompted or leak:
        bad += 1
        what = leak[0][:100] if leak else "prompt"
        out.append(f"RED  noctty: a prompt was printed or /dev/tty leaked: {what}")
    else:
        out.append("ok   noctty: silent, no prompt, no /dev/tty error")
    if not h.installed():
        bad += 1
        out.append(f"RED  noctty: the install did not complete (rc={rc}): {last(text)}")
    return out, int(bad > 0)


def check_trap(h, opts):
    out, bad = [], 0
    nth = int(opts.get("nth", 1))
    for name in ("INT", "TERM", "HUP"):
        signum = getattr(signal, "SIG" + name)
        h.fresh_home(old=False)
        h.serve()
        h.reset_log()
        tmpd = h.w / "tmp"
        p = subprocess.Popen(  # noqa: S603  the shell under test is the operand
            [*h.shell, h.inst, *h.p["args"]],
            env=h.env({"STUB_SLEEP": "6"}),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        t0 = time.time()
        while time.time() - t0 < (10 if nth == 1 else 25) and len(h.downloads()) < nth:
            time.sleep(0.05)
        began = len(h.downloads()) >= nth
        time.sleep(0.3)
        with contextlib.suppress(ProcessLookupError):
            os.killpg(p.pid, signum)
        try:
            rc = p.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)
            rc = "TIMEOUT"
        time.sleep(0.2)
        leaked = [q.name for q in tmpd.iterdir()]
        ok_status = isinstance(rc, int) and rc in (-signum, 128 + signum)
        ran = h.installed()
        red = (not began) or (not ok_status) or bool(leaked) or ran
        bad += red
        mark = "RED " if red else "ok  "
        out.append(
            f"{mark} {name}: download-started={began} rc={rc} (want {-signum} or {128 + signum}) "
            f"leaked-tmp={leaked or 'none'} ran-on={ran}"
        )
    return out, int(bad > 0)


CHECKS = [
    ("parse", check_parse),
    ("trunc", check_trunc),
    ("tamper", check_tamper),
    ("hash", check_hash),
    ("flags", check_flags),
    ("platform", check_platform),
    ("root", check_root),
    ("rc", check_rc),
    ("tty", check_tty),
    ("trap", check_trap),
]


def main(argv):
    args, opts, pos = argv[1:], {"shell": "sh", "profile": None, "nth": 1}, []
    i = 0
    while i < len(args):
        if args[i] in ("--shell", "--profile", "--nth") and i + 1 < len(args):
            opts[args[i][2:]] = args[i + 1]
            i += 2
        else:
            pos.append(args[i])
            i += 1
    if len(pos) != 2 or (pos[0] != "all" and pos[0] not in dict(CHECKS)):
        print(__doc__.split("Profile")[0])
        return 78
    check, inst = pos
    if not Path(inst).is_file():
        print("installer-kit: no such file: " + inst, file=sys.stderr)
        return 78
    prof = dict(DEFAULTS)
    if opts["profile"]:
        try:
            prof.update(json.loads(Path(opts["profile"]).read_text()))
        except (OSError, ValueError) as e:
            print(f"installer-kit: bad profile: {e}", file=sys.stderr)
            return 78
    h = Harness(inst, opts["shell"], prof)
    red = 0
    try:
        todo = CHECKS if check == "all" else [(check, dict(CHECKS)[check])]
        for name, fn in todo:
            lines, r = fn(h, opts)
            print(f"== {name} [{opts['shell']}]")
            print("\n".join(lines))
            red += r
    except OSError as e:
        print(f"installer-kit: I/O failed: {e}", file=sys.stderr)
        return 74
    finally:
        h.close()
    print(f"SUMMARY {check} shell={opts['shell']}: red={red} of {len(todo)}")
    return 1 if red else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
