#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The OCX Authors
"""`task bazel:doctor` — the host preconditions a first Bazel build needs.

    scripts/bazel_doctor.py --check                  # every check, red/green, fix beside it
    scripts/bazel_doctor.py --print-host-block       # the ~/.bazelrc stanza, stdout
    scripts/bazel_doctor.py --write-host-block       # write it, idempotent markers
    scripts/bazel_doctor.py --set-reader-credential  # password on STDIN, never argv
    scripts/bazel_doctor.py --self-test

`.bazelrc` and `MODULE.bazel` are committed, so a checkout arrives with the
workspace half of a working build already correct. Everything that is *not*
committed — the output root, the RAM envelope, the C++ link prerequisite, the
cache reader credential — lives on the host, is invisible to every gate in this
repository, and is what a new machine actually gets wrong. This reads that half.

**The skill is the prompting, this file is the checks.**
`.claude/skills/init-bazel-config/SKILL.md` runs `--check`, reads the codes, and
asks the questions only a human can answer (the reader password, the maintainer
opt-in). Two implementations of "is this host ready" would drift within a
release; the skill therefore owns no check of its own.

**Host state goes in `~/.bazelrc`, never `.bazelrc.user`.** Owner rule, and
mechanical: this machine carries four fixed worktrees plus an agent worktree per
task, `.bazelrc.user` is per-checkout and gitignored, so the credential and the
output root would have to be re-created in every one of them — and a checkout
that missed one fails in a way that looks like a cache outage. `~/.bazelrc` is
read from all of them. The committed `.bazelrc` `try-import`s `.bazelrc.user`
last, which stays the right place for a genuinely per-checkout experiment.

**Four traps this file exists because of, all measured on this host.**

1. *The rc tokenizer splits on whitespace.* `--remote_header=authorization=Basic
   <b64>` written bare makes Bazel read `<b64>` as a target
   (`no such target '//:ZGV2…'`) and every command in the workspace dies. The
   quoted form `--remote_header="authorization=Basic <b64>"` is mandatory. The
   check is not a regex over the value — it is `shlex.split` over the line,
   which is the same tokenization Bazel performs, so the detector cannot
   disagree with the thing it detects.
2. *`Python-urllib/*` gets a Cloudflare 403 on `*.ocx.sh`.* A verdict of "any
   non-401 means the credential works" reads that 403 as a **pass** — measured:
   the naive probe returned 403 with a valid credential, with no credential, and
   with a deliberately wrong one, identically. So the probe sends a real
   User-Agent, and 403 is its own finding rather than a non-401.
3. *`fc-list` is resolved before it is run, never piped blind.*
   `fc-list | grep -qi mono` answers "no monospace font" when the binary is
   merely absent, which is byte-identical to the real negative. So the binary is
   resolved first and an unresolvable `fc-list` is reported as *unknown*, never
   as absent fonts. It is **not** off PATH on this host: an earlier note in this
   file claimed `/usr/sbin/fc-list` was the reason, and measured, `/usr/sbin` is
   a merged-usr symlink to `/usr/bin` here — both spellings are inode 466981,
   and a clean `PATH=/usr/bin` resolves it. The trap is the blind pipe, not the
   directory.
4. *A Build Event Protocol file on disk is a credential dump.* Bazel serialises
   the whole client environment and every rc-file flag value into the BEP, and
   `--build_event_json_file` creates it at the process umask — 0644 here. The
   producers were moved to a 0700 `mktemp -d` (`taskfiles/bazel.taskfile.yml`),
   and check 8 below sweeps for the ones already on disk. Measured while writing
   it: nine 0644 streams under `~/.cache/ocx`, each carrying
   `AWS_SECRET_ACCESS_KEY` five times. The matcher is deliberately wider than
   the glob that was first proposed for it — `*.bep.json` matches neither
   `bep.json` nor `test-bep.json` nor any `bep-*.json`, so it would have walked
   both roots and reported clean.

**What the probe actually returns** (measured, `/v1/ac/<64 zeros>`, reader
credential, correct `Authorization:` header): **HTTP 200**, and the reason is
not bazel-remote's — the all-zero action key holds an empty `ActionResult` the
owner PUT there by hand, so a 20-byte body comes back where an untouched cache
would 404. That is why the green side is the closed set `{200, 204, 404}` and
not `{404}` alone: the 200 records one hand-made entry, a 404 is the same realm
answering for a key nothing ever wrote, and both prove the credential was
accepted. `/v1/cas/<64 zeros>` still 404s. Stated as a closed set rather than as
`!= 401`, because trap 2 is exactly a non-401 that means the opposite.

Stdlib only (plan DEC-8), and the comparator for check 1 is imported from
`scripts/bazel_gate_proofs.py` rather than spelled again — a second copy of a
pin comparison is the copy that goes stale.
"""

from __future__ import annotations

import argparse
import base64
import dataclasses
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from collections.abc import Iterable, Sequence
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from bazel_gate_proofs import REPO_ROOT, expect, pin_drift, read_bazelversion

#: The reader realm's URL base. Overridable so `--self-test` can point the probe
#: at a local server whose status code it chose — the alternative is a proof that
#: needs the public internet, which is a green that says nothing about the code.
CACHE_URL_ENV = "OCX_BAZEL_DOCTOR_CACHE_URL"
DEFAULT_CACHE_URL = "https://bazel-cache.ocx.sh/v1"

#: `Python-urllib/3.x` is 403'd at the edge for every `*.ocx.sh` host (trap 2).
USER_AGENT = "ocx-bazel-doctor/1 (+https://ocx.sh)"

#: The all-zero action key. Nothing ever writes it, so a hit and a miss are both
#: informative and neither costs the cache anything.
ZERO_DIGEST = "0" * 64

READER_USER = "dev-read"
CACHE_HOST_DOC = "bazel-cache.ocx.sh"

#: Idempotency markers. The block is replaced between them, so a second run is a
#: no-op and a hand-edit outside them survives.
BLOCK_BEGIN = "# >>> ocx bazel doctor >>>"
BLOCK_END = "# <<< ocx bazel doctor <<<"

#: Flags that must never appear in a per-checkout `.bazelrc.user`: each is host
#: state, and a copy in one checkout is a copy the other worktrees do not have.
HOST_ONLY_FLAGS = ("--output_user_root", "--repo_contents_cache", "--remote_header")

#: The disk-cache GC pair. Deliberately unset on this pin — `.bazelrc` records
#: why (bazelbuild/bazel#30836, milestoned 9.3.0), and the reopen condition is a
#: version, so that is what this checks against rather than a bare presence test.
GC_FLAGS = ("--experimental_disk_cache_gc_max_size", "--experimental_disk_cache_gc_max_age")
GC_REOPEN_AT = (9, 3, 0)

OK, WARN, FAIL = "ok", "warn", "fail"

_STATUS_TEXT = {OK: "PASS", WARN: "WARN", FAIL: "FAIL"}
_STATUS_COLOUR = {OK: "\033[32m", WARN: "\033[33m", FAIL: "\033[31m"}


@dataclasses.dataclass(frozen=True, slots=True)
class Check:
    """One doctor line. `code` is what the self-test and the skill assert on;
    `detail` and `fix` are the English a human reads.

    Separate for the reason `bazel_gate_proofs.Finding` separates them: asserting
    a substring of a sentence is one of the cheapest ways to write an assertion
    that also matches the opposite outcome.
    """

    code: str
    title: str
    status: str
    detail: str
    fix: str = ""


def codes(checks: Iterable[Check], status: str | None = None) -> list[str]:
    """Sorted codes, optionally only those at one status."""
    return sorted(check.code for check in checks if status is None or check.status == status)


# ---------------------------------------------------------------------------
# rc reading — the same tokenization Bazel performs
# ---------------------------------------------------------------------------


@dataclasses.dataclass(frozen=True, slots=True)
class RcLine:
    """One rc line after Bazel's own tokenization.

    `tokens` excludes the leading command word (`build`, `startup`, `common`),
    which no check here distinguishes; `number` is kept because a fix that says
    "wrap the value on line 21" is actionable and "wrap it somewhere in
    ~/.bazelrc" is not.
    """

    number: int
    tokens: tuple[str, ...]


def rc_lines(text: str) -> list[RcLine]:
    """Every non-comment, non-blank rc line, tokenized as Bazel tokenizes it.

    Comments are whole-line only (`#` first non-space character), which is
    Bazel's own rule and matters here: `~/.bazelrc` keeps a commented-out
    *unquoted* `--remote_header` example directly above the live quoted line, and
    a regex-over-the-file detector reports that comment as a broken credential.
    """
    lines: list[RcLine] = []
    for number, raw in enumerate(text.splitlines(), start=1):
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        try:
            tokens = shlex.split(stripped)
        except ValueError:
            # An unbalanced quote. Bazel rejects the file; report the line as a
            # command with no options so the flag checks below red on it.
            tokens = stripped.split()
        if not tokens:
            continue
        lines.append(RcLine(number=number, tokens=tuple(tokens[1:])))
    return lines


def rc_flag(lines: Sequence[RcLine], name: str) -> str | None:
    """The last value given to `--<name>=`, across every command prefix.

    Last, not first: rc files accumulate and the later line wins, and the home rc
    is read after the workspace one. A `--name` with no `=` yields `""`, which is
    "present, valueless" — distinct from `None`, which is absent.
    """
    value: str | None = None
    for line in lines:
        for token in line.tokens:
            if token == name:
                value = ""
            elif token.startswith(f"{name}="):
                value = token.split("=", 1)[1]
    return value


def rc_has_flag(lines: Sequence[RcLine], name: str) -> bool:
    return any(
        token == name or token.startswith(f"{name}=") for line in lines for token in line.tokens
    )


def split_header_lines(lines: Sequence[RcLine]) -> tuple[list[RcLine], list[RcLine]]:
    """`--remote_header` lines, split into (well-formed, whitespace-split).

    Trap 1's detector. A header value is `name=value`, and every credential value
    this repository uses contains a space (`Basic <b64>`), so a line whose
    `--remote_header=` token is followed by *any* further token has had its value
    eaten by the tokenizer. That is decided from the token list rather than from
    the presence of quote characters, so the detector and Bazel cannot disagree.
    """
    good: list[RcLine] = []
    broken: list[RcLine] = []
    for line in lines:
        for index, token in enumerate(line.tokens):
            if not token.startswith("--remote_header="):
                continue
            if index + 1 < len(line.tokens):
                broken.append(line)
            else:
                good.append(line)
    return good, broken


def header_pair(line: RcLine) -> tuple[str, str] | None:
    """`(name, value)` of a well-formed `--remote_header=` token, or `None`."""
    for token in line.tokens:
        if token.startswith("--remote_header="):
            name, _, value = token.split("=", 1)[1].partition("=")
            if name and value:
                return name, value
    return None


# ---------------------------------------------------------------------------
# Check 1 — Bazel comes from the project toolchain
# ---------------------------------------------------------------------------

TOOLS_PIN = re.compile(r'^\s*bazel\s*=\s*"(?P<repo>[^"]+):(?P<version>[^":]+)"\s*$', re.MULTILINE)
LOCK_ENTRY = re.compile(r'^\s*name\s*=\s*"bazel"\s*$', re.MULTILINE)


def toolchain_pin(ocx_toml: str | None) -> str | None:
    """The version half of `ocx.toml`'s `[tools] bazel = "<repo>:<version>"`."""
    if ocx_toml is None:
        return None
    match = TOOLS_PIN.search(ocx_toml)
    return match.group("version") if match else None


def check_toolchain(
    ocx_toml: str | None,
    ocx_lock: str | None,
    bazelversion: str | None,
    version_stdout: str | None,
    version_rc: int | None,
    bazelisk_path: str | None,
) -> list[Check]:
    """`ocx.toml` pin, `ocx.lock` entry, the resolved binary, `.bazelversion`,
    and nothing shadowing any of them.

    The `.bazelversion`-vs-binary comparison is `bazel_gate_proofs.pin_drift`,
    the comparator `bazel:pin:check` already runs — imported, not restated.
    """
    checks: list[Check] = []
    pin = toolchain_pin(ocx_toml)
    if pin is None:
        checks.append(
            Check(
                "toolchain-pin",
                "bazel pinned in ocx.toml",
                FAIL,
                "ocx.toml has no `[tools] bazel = \"<repo>:<version>\"` line",
                'add `bazel = "ocx.sh/bazelbuild/bazel:<version>"` under [tools], then `ocx lock`',
            )
        )
    elif ocx_lock is None or not LOCK_ENTRY.search(ocx_lock):
        checks.append(
            Check(
                "toolchain-lock",
                "bazel locked in ocx.lock",
                FAIL,
                f"ocx.toml pins bazel {pin} and ocx.lock carries no `name = \"bazel\"` entry",
                "run `ocx lock` and commit ocx.lock — the pin resolves from the lock alone",
            )
        )
    else:
        checks.append(
            Check(
                "toolchain-pin",
                "bazel pinned in ocx.toml",
                OK,
                f"ocx.toml pins bazel {pin}, ocx.lock carries its per-platform digests",
            )
        )

    drift = pin_drift(bazelversion, version_stdout, version_rc)
    if drift:
        checks.append(
            Check(
                "toolchain-drift",
                ".bazelversion matches the resolved binary",
                FAIL,
                "; ".join(finding.message for finding in drift),
                "`ocx exec bazel -- bazel --version` must equal `.bazelversion`; "
                "reconcile ocx.toml, ocx.lock and .bazelversion, then `ocx pull`",
            )
        )
    else:
        resolved = (version_stdout or "").strip()
        detail = f"`bazel --version` is {resolved!r}, .bazelversion agrees"
        if pin is not None and pin not in resolved:
            checks.append(
                Check(
                    "toolchain-pin-drift",
                    "the running binary is the pinned one",
                    FAIL,
                    f"{detail}, but ocx.toml pins {pin}",
                    "bump .bazelversion and ocx.toml together, then `ocx pull`",
                )
            )
        else:
            checks.append(
                Check("toolchain-drift", ".bazelversion matches the resolved binary", OK, detail)
            )

    if bazelisk_path:
        checks.append(
            Check(
                "toolchain-bazelisk",
                "nothing shadows the pinned bazel",
                FAIL,
                f"bazelisk is on PATH at {bazelisk_path} — it resolves its own version and "
                "will not be the binary ocx.lock pins",
                f"remove {bazelisk_path} from PATH; every call in this repository goes through "
                "`ocx exec bazel -- <cmd>`",
            )
        )
    else:
        checks.append(
            Check(
                "toolchain-bazelisk",
                "nothing shadows the pinned bazel",
                OK,
                "no bazelisk on PATH; bazel resolves through `ocx exec bazel --`",
            )
        )
    return checks


# ---------------------------------------------------------------------------
# Check 2 — ~/.bazelrc carries the host block
# ---------------------------------------------------------------------------


def host_block(home: Path, *, jobs: int, heap_gb: int) -> str:
    """The marker-delimited stanza `--write-host-block` maintains."""
    cache = home / ".cache" / "ocx"
    return "\n".join(
        (
            BLOCK_BEGIN,
            "# Host-wide Bazel settings, written by `scripts/bazel_doctor.py --write-host-block`.",
            "# Every worktree on this machine reads this file; .bazelrc.user is per-checkout and",
            "# is deliberately NOT where host state goes. Edit between the markers or outside",
            "# them — a re-run replaces only what is between them.",
            "",
            "# Keep every Bazel artifact off a reaped /tmp tmpfs and out of the repositories.",
            f"startup --output_user_root={cache / 'bazel-root'}",
            f"common --repo_contents_cache={cache / 'bazel-repo'}",
            "",
            "# Scheduler and RAM envelope. The Bazel server schedules, it does not compile:",
            "# left alone the JVM sizes its heap off total RAM and starves rustc.",
            f"startup --host_jvm_args=-Xmx{heap_gb}g",
            f"build --jobs={jobs}",
            BLOCK_END,
            "",
        )
    )


def splice_block(text: str, block: str) -> str:
    """Replace between the markers, or append. Idempotent by construction: the
    output of `splice_block(splice_block(t, b), b)` is `splice_block(t, b)`."""
    begin = text.find(BLOCK_BEGIN)
    end = text.find(BLOCK_END)
    if begin != -1 and end != -1 and end > begin:
        tail = end + len(BLOCK_END)
        tail += 1 if text[tail : tail + 1] == "\n" else 0
        return text[:begin] + block + text[tail:]
    if text and not text.endswith("\n"):
        text += "\n"
    return text + ("\n" if text else "") + block


def under_tmp(path: str) -> bool:
    """`/tmp` is a tmpfs on the development host, and an hourly reaper walks it.
    A Bazel artifact there is RAM that disappears mid-build."""
    resolved = os.path.expanduser(path)
    return resolved == "/tmp" or resolved.startswith("/tmp/")


def check_host_rc(home_rc: str | None, user_rc: str | None) -> list[Check]:
    """`~/.bazelrc` exists and carries the four host settings, and `.bazelrc.user`
    carries none of them."""
    checks: list[Check] = []
    if home_rc is None:
        checks.append(
            Check(
                "hostrc-absent",
                "~/.bazelrc carries the host block",
                FAIL,
                "~/.bazelrc does not exist, so the output root, the RAM envelope and the "
                "cache credential are unset in every worktree on this machine",
                "`scripts/bazel_doctor.py --write-host-block` (or `/init-bazel-config`, "
                "which asks first)",
            )
        )
        return checks

    lines = rc_lines(home_rc)
    output_root = rc_flag(lines, "--output_user_root")
    missing = [
        name
        for name, present in (
            ("--output_user_root", output_root is not None),
            ("--repo_contents_cache", rc_flag(lines, "--repo_contents_cache") is not None),
            ("--host_jvm_args", rc_has_flag(lines, "--host_jvm_args")),
            ("--jobs", rc_flag(lines, "--jobs") is not None),
        )
        if not present
    ]
    if missing:
        checks.append(
            Check(
                "hostrc-incomplete",
                "~/.bazelrc carries the host block",
                FAIL,
                f"~/.bazelrc is missing {', '.join(missing)}",
                "`scripts/bazel_doctor.py --write-host-block` rewrites the block between its "
                "markers and leaves everything else alone",
            )
        )
    elif output_root is not None and under_tmp(output_root):
        checks.append(
            Check(
                "hostrc-tmp-output-root",
                "~/.bazelrc carries the host block",
                FAIL,
                f"--output_user_root={output_root} is under /tmp, which is a tmpfs here and is "
                "reaped hourly — the server's base disappears mid-build",
                "point it at ~/.cache/ocx/bazel-root: `scripts/bazel_doctor.py --write-host-block`",
            )
        )
    else:
        checks.append(
            Check(
                "hostrc",
                "~/.bazelrc carries the host block",
                OK,
                f"output root {output_root}, plus --repo_contents_cache, --host_jvm_args "
                f"and --jobs={rc_flag(lines, '--jobs')}",
            )
        )

    if user_rc is not None:
        leaked = [
            flag for flag in HOST_ONLY_FLAGS if rc_has_flag(rc_lines(user_rc), flag)
        ]
        if leaked:
            checks.append(
                Check(
                    "userrc-host-state",
                    ".bazelrc.user carries no host state",
                    FAIL,
                    f".bazelrc.user sets {', '.join(leaked)} — that is host state in a "
                    "per-checkout file, so every other worktree on this machine lacks it",
                    "move those lines into ~/.bazelrc; keep .bazelrc.user for one-checkout "
                    "experiments only",
                )
            )
    return checks


# ---------------------------------------------------------------------------
# Check 3 — the C++ link prerequisite
# ---------------------------------------------------------------------------

#: Measured on Fedora 43: `libstdc++-devel` does NOT ship `libstdc++.so`; the
#: linker script lives in `gcc-c++`. Installing the -devel package is the
#: obvious wrong answer and leaves the build failing identically.
DISTRO_FIX = {
    "fedora": "sudo dnf install gcc-c++   (NOT libstdc++-devel — measured on F43, it does not ship libstdc++.so)",
    "rhel": "sudo dnf install gcc-c++",
    "debian": "sudo apt install g++",
    "ubuntu": "sudo apt install g++",
    "arch": "sudo pacman -S gcc",
    "alpine": "sudo apk add g++",
}


def distro_ids(os_release: str | None) -> list[str]:
    """`ID` then each `ID_LIKE` word, in that order."""
    if not os_release:
        return []
    ids: list[str] = []
    for line in os_release.splitlines():
        key, _, value = line.partition("=")
        value = value.strip().strip('"')
        if key == "ID" and value:
            ids.append(value)
        elif key == "ID_LIKE" and value:
            ids.extend(value.split())
    return ids


def distro_command(os_release: str | None) -> str:
    for identifier in distro_ids(os_release):
        if identifier in DISTRO_FIX:
            return DISTRO_FIX[identifier]
    return "install your distribution's C++ compiler package (it ships libstdc++.so)"


def check_cxx(
    printed: str | None,
    exists: bool,
    farm_dir: str | None,
    farm_has_lib: bool,
    os_release: str | None,
) -> list[Check]:
    """`gcc -print-file-name=libstdc++.so` must resolve to a real file.

    gcc echoes the *bare name* back when it cannot find the file, so "resolved"
    means an absolute path that exists — a truthy string is the wrong test and
    passes on exactly the broken host this check is for. rules_rust's
    `process_wrapper` links C++ and fails without it.

    The root-free fallback is a symlink farm plus `-L<dir>`, and it is accepted
    as green: it makes the link work, which is the property. It is reported as
    the fallback so the line does not read as a working toolchain.
    """
    if printed and printed.startswith("/") and exists:
        return [
            Check(
                "cxx",
                "libstdc++.so resolves for the linker",
                OK,
                f"gcc -print-file-name=libstdc++.so -> {printed}",
            )
        ]
    if farm_dir and farm_has_lib:
        return [
            Check(
                "cxx-farm",
                "libstdc++.so resolves for the linker",
                WARN,
                f"gcc cannot find libstdc++.so (it echoed {printed!r}); the root-free symlink "
                f"farm at {farm_dir} is covering it via --linkopt=-L",
                f"to retire the farm and its two ~/.bazelrc lines: {distro_command(os_release)}",
            )
        ]
    return [
        Check(
            "cxx-missing",
            "libstdc++.so resolves for the linker",
            FAIL,
            f"gcc -print-file-name=libstdc++.so echoed {printed!r} rather than a path that "
            "exists — rules_rust's process_wrapper cannot link",
            f"{distro_command(os_release)}\n"
            "        or, without root, a symlink farm written to ~/.bazelrc:\n"
            "          mkdir -p ~/.cache/ocx/libdir && ln -sf \"$(gcc -print-file-name=libstdc++.so.6)\" ~/.cache/ocx/libdir/libstdc++.so\n"
            "          build --linkopt=-L$HOME/.cache/ocx/libdir\n"
            "          build --host_linkopt=-L$HOME/.cache/ocx/libdir",
        )
    ]


# ---------------------------------------------------------------------------
# Check 4 — the remote-cache reader credential
# ---------------------------------------------------------------------------


def probe_cache(header: tuple[str, str] | None, base_url: str, timeout: float = 20.0) -> int | str:
    """The HTTP status of `<base>/ac/<64 zeros>`, or an exception class name.

    Never logs the header. `urllib` is handed the pair unchanged and the value
    appears in no message this function can produce.
    """
    headers = {"User-Agent": USER_AGENT}
    if header is not None:
        headers[header[0]] = header[1]
    request = urllib.request.Request(f"{base_url}/ac/{ZERO_DIGEST}", headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            response.read()
            return int(response.status)
    except urllib.error.HTTPError as error:
        return int(error.code)
    except OSError as error:
        return type(error).__name__


def credential_verdict(status: int | str) -> Check:
    """A closed set, never `status != 401`.

    Measured: 200 for `/v1/ac/<zeros>` with the reader credential — the owner's
    hand-made empty `ActionResult` under the all-zero key, not a live action and
    not something bazel-remote synthesises — 404 for `/v1/cas/<zeros>`, 401 for
    no or wrong credential, and **403 for a valid credential sent with urllib's
    default User-Agent**. That 403 came back *identically* with a valid
    credential, with none, and with a deliberately wrong one: it is Cloudflare
    refusing the agent before nginx sees the request, so it settles nothing
    about the credential and gets its own FAIL saying so. `!= 401` would report
    that edge block as a working credential, which is why the green side is
    enumerated.
    """
    if status in (200, 204, 404):
        return Check(
            "cache-credential",
            "remote cache reader credential works",
            OK,
            f"HTTP {status} from /ac/<64 zeros> — the reader realm accepted the credential",
        )
    if status == 401:
        return Check(
            "cache-credential-401",
            "remote cache reader credential works",
            FAIL,
            f"HTTP 401 from {CACHE_HOST_DOC} — no credential, or the wrong one",
            "`/init-bazel-config` asks for the `dev-read` password and appends the header, "
            "or: `scripts/bazel_doctor.py --set-reader-credential` (password on stdin)",
        )
    if status == 403:
        return Check(
            "cache-credential-blocked",
            "remote cache reader credential works",
            FAIL,
            f"HTTP 403 from {CACHE_HOST_DOC} — the edge refused the request before the realm "
            "saw it, so this says nothing about the credential",
            "a 403 here is a User-Agent or WAF block, not an auth failure; retry from a "
            "different network before touching the credential",
        )
    return Check(
        "cache-credential-unknown",
        "remote cache reader credential works",
        FAIL,
        f"the probe returned {status!r}, which is neither the reader realm's 401 nor a hit",
        f"check that {CACHE_HOST_DOC} is reachable from this network",
    )


def check_credential(home_rc: str | None, base_url: str, probe=probe_cache) -> list[Check]:
    """Parse the header out of `~/.bazelrc`, refuse the whitespace-split form,
    then probe. Reads only; a writer credential is never placed locally."""
    lines = rc_lines(home_rc or "")
    good, broken = split_header_lines(lines)
    checks: list[Check] = []
    if broken:
        checks.append(
            Check(
                "credential-unquoted",
                "the credential line survives Bazel's tokenizer",
                FAIL,
                f"~/.bazelrc line {broken[-1].number} writes --remote_header= with an unquoted "
                "value; Bazel splits on whitespace and reads the base64 as a build target "
                "(`no such target '//:…'`), so every command in the workspace fails",
                f'wrap the whole value in double quotes on line {broken[-1].number}: '
                'build --remote_header="authorization=Basic <b64>"',
            )
        )
    if not good:
        checks.append(
            Check(
                "cache-credential-absent",
                "remote cache reader credential works",
                FAIL,
                "~/.bazelrc carries no --remote_header line, so every action misses the shared "
                "cache and builds from scratch",
                "`/init-bazel-config` asks for the `dev-read` password, or: "
                "`scripts/bazel_doctor.py --set-reader-credential` (password on stdin)",
            )
        )
        return checks
    checks.append(credential_verdict(probe(header_pair(good[-1]), base_url)))
    return checks


def credential_line(user: str, password: str) -> str:
    """The one correctly-quoted spelling. The password reaches this function and
    nothing else; the return value carries only its base64 encoding."""
    token = base64.b64encode(f"{user}:{password}".encode()).decode("ascii")
    return f'build --remote_header="authorization=Basic {token}"'


def set_reader_credential(text: str, user: str, password: str) -> str:
    """Replace every existing `--remote_header` line with one correct line.

    Replace rather than append: appending to a host that already has a broken
    unquoted line leaves the broken line in place, and Bazel still fails.
    """
    keep = [
        raw
        for raw in text.splitlines()
        if "--remote_header" not in raw or raw.strip().startswith("#")
    ]
    body = "\n".join(keep).rstrip("\n")
    header = (
        f"\n\n# {CACHE_HOST_DOC} reader realm ({user}). Reads only — a writer credential is "
        "never placed on a developer machine.\n"
        "# The quotes are load-bearing: Bazel's rc tokenizer splits on whitespace.\n"
    )
    return f"{body}{header}{credential_line(user, password)}\n"


def leaks(secret: str, *texts: str) -> bool:
    """True when `secret` appears in any of `texts`. The self-test's leak
    detector, and proven on a control that does contain it."""
    return any(secret and secret in text for text in texts)


# ---------------------------------------------------------------------------
# Check 5 — disk cache present, bounded, and off /tmp
# ---------------------------------------------------------------------------


def version_tuple(text: str | None) -> tuple[int, ...]:
    """`"bazel 9.2.0"` or `"9.2.0"` -> `(9, 2, 0)`; unparseable -> `()`."""
    match = re.search(r"(\d+)\.(\d+)\.(\d+)", text or "")
    return tuple(int(part) for part in match.groups()) if match else ()


def check_caches(
    disk_cache: str | None,
    disk_cache_exists: bool,
    output_root: str | None,
    gc_set: bool,
    version: tuple[int, ...],
) -> list[Check]:
    """`--disk_cache` resolved, present, off `/tmp`; the output root off `/tmp`;
    and the GC pair required only once the pin crosses the version whose absence
    is the documented reason they are off.

    The GC arm is a version comparison rather than a presence test on purpose.
    `.bazelrc` turns both flags off deliberately — collection only runs after
    `--experimental_disk_cache_gc_idle_delay` of idleness, which an ephemeral CI
    runner never reaches, and enabling them on this pin exposes the development
    host to bazelbuild/bazel#30836 (milestoned 9.3.0). A bare presence check
    would red against a decision this repository has already taken and written
    down, which trains readers to ignore the doctor.
    """
    checks: list[Check] = []
    if disk_cache is None:
        checks.append(
            Check(
                "disk-cache-absent",
                "disk cache configured and off /tmp",
                FAIL,
                "no --disk_cache in any rc file — every local rebuild recompiles from scratch",
                "the committed .bazelrc sets it; a missing value means the workspace rc was "
                "not read, so check you are running from the repository root",
            )
        )
    elif under_tmp(disk_cache):
        checks.append(
            Check(
                "disk-cache-tmp",
                "disk cache configured and off /tmp",
                FAIL,
                f"--disk_cache={disk_cache} is under /tmp — a tmpfs here, so the cache is RAM "
                "and an hourly reaper deletes it mid-build",
                "point --disk_cache at an absolute path under $HOME",
            )
        )
    elif not disk_cache_exists:
        checks.append(
            Check(
                "disk-cache-missing-dir",
                "disk cache configured and off /tmp",
                WARN,
                f"--disk_cache={disk_cache} does not exist yet; Bazel creates it on the first "
                "build, so this is only a note on a fresh host",
                f"mkdir -p {disk_cache}",
            )
        )
    else:
        checks.append(
            Check(
                "disk-cache",
                "disk cache configured and off /tmp",
                OK,
                f"--disk_cache={disk_cache}",
            )
        )

    if output_root is None:
        checks.append(
            Check(
                "output-root-absent",
                "output root off /tmp",
                FAIL,
                "no --output_user_root anywhere, so Bazel defaults it under /tmp on this host",
                "`scripts/bazel_doctor.py --write-host-block`",
            )
        )
    elif under_tmp(output_root):
        checks.append(
            Check(
                "output-root-tmp",
                "output root off /tmp",
                FAIL,
                f"--output_user_root={output_root} is under /tmp",
                "`scripts/bazel_doctor.py --write-host-block`",
            )
        )
    else:
        checks.append(Check("output-root", "output root off /tmp", OK, output_root))

    if version >= GC_REOPEN_AT and not gc_set:
        checks.append(
            Check(
                "disk-cache-unbounded",
                "disk cache bounded",
                FAIL,
                f"bazel {'.'.join(map(str, version))} is at or past "
                f"{'.'.join(map(str, GC_REOPEN_AT))}, where bazelbuild/bazel#30836 is fixed, and "
                f"neither {' nor '.join(GC_FLAGS)} is set — the cache grows without bound",
                f"set {GC_FLAGS[0]} in .bazelrc and delete the paragraph that explains why it "
                "was off",
            )
        )
    elif gc_set:
        checks.append(Check("disk-cache-gc", "disk cache bounded", OK, "GC flags set"))
    else:
        checks.append(
            Check(
                "disk-cache-gc-parked",
                "disk cache bounded",
                WARN,
                "GC is off by decision on this pin (.bazelrc: idle-delay collection never runs "
                "on CI, and #30836 is open until 9.3.0) — growth is accepted and bounded by "
                "hand",
                f"rm -rf {disk_cache} when it gets large; the flags land when the pin crosses "
                f"{'.'.join(map(str, GC_REOPEN_AT))}",
            )
        )
    return checks


# ---------------------------------------------------------------------------
# Check 6 — the maintainer path (opt-in, never automatic)
# ---------------------------------------------------------------------------

ORG = "ocx-sh"
ORG_SECRETS = ("BAZEL_CACHE_READ_AUTH", "BAZEL_CACHE_WRITE_AUTH")
ORG_REPOS = ("ocx", "rules_ocx")


def check_maintainer(role: str | None, secret_names: frozenset[str] | None) -> list[Check]:
    """Advisory in both directions. A contributor is not broken for lacking org
    admin, and a maintainer is not broken for having left the secrets alone —
    rotating an org secret is a decision, so this reports and the skill asks."""
    if role != "admin":
        return [
            Check(
                "maintainer-na",
                f"{ORG} org secrets",
                OK,
                f"not an admin of {ORG} (gh reports {role!r}); the maintainer path does not apply",
            )
        ]
    if secret_names is None:
        return [
            Check(
                "maintainer-unknown",
                f"{ORG} org secrets",
                WARN,
                f"org admin on {ORG}, and `gh secret list --org {ORG}` could not be read",
                "gh auth refresh -h github.com -s admin:org",
            )
        ]
    missing = [name for name in ORG_SECRETS if name not in secret_names]
    if missing:
        return [
            Check(
                "maintainer-secrets",
                f"{ORG} org secrets",
                WARN,
                f"org admin on {ORG}; {', '.join(missing)} not set",
                f"/init-bazel-config offers to set them: gh secret set <NAME> --org {ORG} "
                f"--visibility selected --repos {','.join(ORG_REPOS)}",
            )
        ]
    return [
        Check(
            "maintainer",
            f"{ORG} org secrets",
            OK,
            f"org admin on {ORG}; {' and '.join(ORG_SECRETS)} are set",
        )
    ]


# ---------------------------------------------------------------------------
# Check 7 (the extra one) — a monospace font for the cast GIF targets
# ---------------------------------------------------------------------------

FC_LIST_CANDIDATES = ("/usr/bin/fc-list", "/usr/sbin/fc-list", "/usr/local/bin/fc-list")


def find_fc_list(which=shutil.which, exists=os.path.exists) -> str | None:
    """Resolve `fc-list` off PATH, falling back to the usual absolute paths.

    Resolving it at all is the point: `fc-list | grep -qi mono` answers "no
    monospace font" when the binary was never found, which is byte-identical to
    the real negative — the defect class `quality-core.md` § Unchecked Green
    names. The fallback list is *dead code on this host* and the docstring used
    to claim otherwise: measured, `/usr/sbin` is a merged-usr symlink to
    `/usr/bin`, both `fc-list` spellings are the same inode, and a clean
    `PATH=/usr/bin` resolves it. It stays for a distro that has not merged
    `/usr/sbin` and keeps fontconfig off the user PATH; it is not a claim about
    this machine.
    """
    found = which("fc-list")
    if found:
        return found
    return next((path for path in FC_LIST_CANDIDATES if exists(path)), None)


def check_fonts(fc_list: str | None, families: list[str] | None) -> list[Check]:
    """`//test/doc_scripts/...` renders casts to GIF through `agg`, which needs a
    monospace family. Advisory: nothing else in the tree needs one."""
    if fc_list is None:
        return [
            Check(
                "fonts-unknown",
                "a monospace font for the cast GIF targets",
                WARN,
                "fc-list resolved nowhere — not on PATH, not at any of the usual absolute "
                "paths — so whether this host has a monospace font is unknown, which is not "
                "the same as it having none",
                "install fontconfig, or skip //test/doc_scripts/... locally",
            )
        ]
    if not families:
        return [
            Check(
                "fonts-absent",
                "a monospace font for the cast GIF targets",
                WARN,
                f"{fc_list} reports no monospace family; the cast GIF targets render blank",
                "sudo dnf install dejavu-sans-mono-fonts   (apt: fonts-dejavu-core)",
            )
        ]
    return [
        Check(
            "fonts",
            "a monospace font for the cast GIF targets",
            OK,
            f"{len(families)} monospace family/families: {', '.join(sorted(families)[:4])}",
        )
    ]


# ---------------------------------------------------------------------------
# Check 8 — no world-readable Build Event Protocol stream on disk
# ---------------------------------------------------------------------------


def is_bep_name(name: str) -> bool:
    """True for every filename a BEP has been written under in this tree.

    Wider than the `*.bep.json` glob this check was first specified with, and
    measured rather than preferred: that glob matches neither `bep.json` nor
    `test-bep.json` (the two `taskfiles/bazel.taskfile.yml` writes) nor any of
    the nine `bep-*.json` streams `scripts/bazel_hermeticity_proofs.py` leaves
    under the cache home. A matcher that matches nothing walks both roots, finds
    nothing and exits green — the clean tree that is really an unread one.

    A false positive costs a `chmod 600` on a file that did not need it, which
    is why the substring arm is not tightened further.
    """
    return name.startswith("build_event") or ("bep" in name.lower() and name.endswith(".json"))


def bep_scan(roots: Sequence[Path]) -> tuple[list[Path], list[Path], list[Path], int]:
    """`(world_readable, read, absent, files_seen)` over `roots` — and *only*
    over `roots`.

    Two named roots rather than `~`: a BEP has only ever been written inside the
    checkout (the leak this exists for) or under the ocx cache home. Walking the
    home directory would read mail, keys and every unrelated project, and cost
    minutes. `followlinks=False` keeps the walk inside them.

    Symlinks are skipped outright rather than judged. A symlink is mode 0777 on
    Linux, so reading the link's own bits reports every one of them as exposed —
    measured: 36 such links sit in bazel execroots under the cache home. The
    mode that matters is the target's, and the target is either inside a root,
    where the walk reaches it directly, or outside one, which is not this scope.

    `files_seen` is the reader's own floor. A walk that descended nowhere finds
    no exposed file either, and without a count that green cannot be told from a
    clean tree (`quality-core.md` § "A green is only as wide as what ran").

    Cost measured on this host: 0.13 s for the checkout (70,804 files), 0.7 s
    warm for `~/.cache/ocx` (231,287 files), ~16 s on a cold dentry cache.
    """
    exposed: list[Path] = []
    read_roots: list[Path] = []
    absent: list[Path] = []
    seen = 0
    for root in roots:
        if not root.is_dir():
            absent.append(root)
            continue
        read_roots.append(root)
        for dirpath, _dirnames, filenames in os.walk(root, followlinks=False):
            for name in filenames:
                seen += 1
                if not is_bep_name(name):
                    continue
                path = Path(dirpath) / name
                if path.is_symlink():
                    continue
                try:
                    mode = path.stat().st_mode
                except OSError:
                    continue
                if mode & 0o004:  # S_IROTH — the bit `chmod 600` clears
                    exposed.append(path)
    return exposed, read_roots, absent, seen


def check_bep_modes(
    exposed: Sequence[Path], read_roots: Sequence[Path], absent: Sequence[Path], seen: int
) -> list[Check]:
    """FAIL on any world-readable BEP, with the `chmod` beside it.

    A root that is not on disk is reported as *not present*, never folded into
    the green: "clean" and "never looked" are the two readings this check must
    keep apart.
    """
    if exposed:
        shown = [str(path) for path in sorted(exposed)[:3]]
        more = f" (+{len(exposed) - len(shown)} more)" if len(exposed) > len(shown) else ""
        return [
            Check(
                "bep-world-readable",
                "no world-readable Build Event Protocol stream on disk",
                FAIL,
                f"{len(exposed)} BEP file(s) are readable by every account on this host: "
                f"{', '.join(shown)}{more} — Bazel serialises the whole client environment and "
                "every rc-file flag value into a BEP, so these carry AWS_SECRET_ACCESS_KEY and "
                "the cache Authorization header in cleartext",
                f"chmod 600 {' '.join(shown)}  — or delete them, they are build scratch; and "
                "write the next one 0600 rather than at the umask, the way "
                "taskfiles/bazel.taskfile.yml now does",
            )
        ]
    if read_roots and not seen:
        return [
            Check(
                "bep-scan-empty",
                "no world-readable Build Event Protocol stream on disk",
                WARN,
                f"{', '.join(str(root) for root in read_roots)} is on disk but the walk read no "
                "files at all, so this says nothing about what is under it",
                "check the permissions on that directory — an unreadable root and a clean one "
                "produce the same finding here",
            )
        ]
    scope = ", ".join(str(root) for root in read_roots) or "no root at all"
    missing = (
        f"; not present: {', '.join(str(root) for root in absent)}" if absent else ""
    )
    return [
        Check(
            "bep-modes",
            "no world-readable Build Event Protocol stream on disk",
            OK,
            f"{seen} files read under {scope}, none of them a world-readable BEP{missing}",
        )
    ]


# ---------------------------------------------------------------------------
# Host readings
# ---------------------------------------------------------------------------


def read(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None


def run(argv: Sequence[str], timeout: float = 30.0) -> tuple[int | None, str]:
    """`(returncode, stdout)`; `(None, "")` when the binary is absent or hangs."""
    try:
        done = subprocess.run(
            list(argv),
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None, ""
    return done.returncode, done.stdout


def gather(home: Path, repo: Path) -> list[Check]:
    """Every reading this host offers, handed to the pure checks above."""
    home_rc = read(home / ".bazelrc")
    repo_rc = read(repo / ".bazelrc")
    user_rc = read(repo / ".bazelrc.user")

    version_rc, version_stdout = run(["ocx", "exec", "bazel", "--", "bazel", "--version"])
    if version_rc is None:
        version_rc, version_stdout = run(["bazel", "--version"])

    checks = check_toolchain(
        read(repo / "ocx.toml"),
        read(repo / "ocx.lock"),
        read_bazelversion(repo / ".bazelversion"),
        version_stdout if version_rc is not None else None,
        version_rc,
        shutil.which("bazelisk"),
    )
    checks += check_host_rc(home_rc, user_rc)

    _, printed = run(["gcc", "-print-file-name=libstdc++.so"])
    printed = printed.strip()
    home_lines = rc_lines(home_rc or "")
    farm = next(
        (
            token.split("-L", 1)[1]
            for line in home_lines
            for token in line.tokens
            if token.startswith(("--linkopt=-L", "--host_linkopt=-L"))
        ),
        None,
    )
    farm = os.path.expandvars(os.path.expanduser(farm)) if farm else None
    checks += check_cxx(
        printed,
        bool(printed) and printed.startswith("/") and os.path.exists(printed),
        farm,
        bool(farm) and os.path.exists(os.path.join(farm, "libstdc++.so")),
        read(Path("/etc/os-release")),
    )

    base_url = os.environ.get(CACHE_URL_ENV, DEFAULT_CACHE_URL)
    checks += check_credential(home_rc, base_url)

    merged = rc_lines((repo_rc or "") + "\n" + (home_rc or ""))
    disk_cache = rc_flag(merged, "--disk_cache")
    resolved_cache = os.path.expanduser(disk_cache) if disk_cache else None
    checks += check_caches(
        resolved_cache,
        bool(resolved_cache) and os.path.isdir(resolved_cache),
        rc_flag(merged, "--output_user_root"),
        any(rc_has_flag(merged, flag) for flag in GC_FLAGS),
        version_tuple(version_stdout),
    )

    role_rc, role = run(["gh", "api", f"user/memberships/orgs/{ORG}", "--jq", ".role"])
    role = role.strip() if role_rc == 0 else None
    names: frozenset[str] | None = None
    if role == "admin":
        listed_rc, listed = run(["gh", "secret", "list", "--org", ORG])
        if listed_rc == 0:
            names = frozenset(line.split()[0] for line in listed.splitlines() if line.split())
    checks += check_maintainer(role, names)

    fc_list = find_fc_list()
    families: list[str] | None = None
    if fc_list:
        families_rc, listed = run([fc_list, ":spacing=100", "family"])
        families = (
            sorted({line.split(",")[0] for line in listed.splitlines() if line.strip()})
            if families_rc == 0
            else []
        )
    checks += check_fonts(fc_list, families)

    checks += check_bep_modes(*bep_scan([repo, home / ".cache" / "ocx"]))
    return checks


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------


def render(checks: Sequence[Check], stream=sys.stdout) -> int:
    """Every check on its own line, red or green, with the fix beside it."""
    colour = stream.isatty()
    for check in checks:
        mark = _STATUS_TEXT[check.status]
        if colour:
            mark = f"{_STATUS_COLOUR[check.status]}{mark}\033[0m"
        print(f"[{mark}] {check.title}", file=stream)
        print(f"        {check.detail}", file=stream)
        if check.fix and check.status != OK:
            print(f"    fix: {check.fix}", file=stream)
    failed = codes(checks, FAIL)
    warned = codes(checks, WARN)
    print(
        f"\n{len(checks)} checks: {len(checks) - len(failed) - len(warned)} pass, "
        f"{len(warned)} warn, {len(failed)} fail",
        file=stream,
    )
    if failed:
        print(f"failing: {', '.join(failed)}", file=stream)
        print(
            "run `/init-bazel-config` — it asks the questions these fixes need and writes the "
            "answers to ~/.bazelrc",
            file=stream,
        )
    return 1 if failed else 0


# ---------------------------------------------------------------------------
# --self-test
# ---------------------------------------------------------------------------


def _write(path: Path, text: str) -> None:
    """Write, then prove the write landed. A mutation whose landing is not proven
    makes every result below it a result about the previous text."""
    path.write_text(text, encoding="utf-8")
    expect(path.read_text(encoding="utf-8") == text, f"the write to {path} did not land")


def prove_rc_reading() -> int:
    """The tokenizer trap, red and green — and the comment control."""
    quoted = 'build --remote_header="authorization=Basic QUJD"\n'
    good, broken = split_header_lines(rc_lines(quoted))
    expect(len(good) == 1 and not broken, "the quoted header form was not accepted")
    expect(header_pair(good[0]) == ("authorization", "Basic QUJD"), "the quoted header misparsed")

    bare = "build --remote_header=authorization=Basic QUJD\n"
    good, broken = split_header_lines(rc_lines(bare))
    expect(not good and len(broken) == 1, "the unquoted header form was not caught")
    expect(broken[0].number == 1, "the unquoted finding named the wrong line")

    # The control that matters on the real host: ~/.bazelrc keeps a commented-out
    # unquoted example directly above the live quoted line, and a regex over the
    # file text reports that comment as a broken credential.
    mixed = "# build --remote_header=authorization=Basic QUJD dev-read:password>\n" + quoted
    good, broken = split_header_lines(rc_lines(mixed))
    expect(not broken, "a commented-out example was reported as a broken credential")
    expect(len(good) == 1, "the live line was lost behind the comment")

    expect(rc_flag(rc_lines("build --jobs=4\nbuild --jobs=12\n"), "--jobs") == "12", "last wins")
    expect(rc_flag(rc_lines("build --jobs=12\n"), "--nothing") is None, "absent must be None")
    print("  rc reading: quoted accepted, whitespace-split caught, comment not mistaken for it")
    return 4


def prove_host_block(scratch: Path) -> int:
    """A fixture HOME with and without the block, plus the /tmp arm."""
    home = scratch / "home"
    home.mkdir()
    rc = home / ".bazelrc"

    expect(codes(check_host_rc(None, None), FAIL) == ["hostrc-absent"], "absent rc must red")

    block = host_block(home, jobs=12, heap_gb=2)
    _write(rc, splice_block("", block))
    first = rc.read_text(encoding="utf-8")
    expect(not codes(check_host_rc(first, None), FAIL), "the written block did not pass")

    _write(rc, splice_block(first, block))
    expect(rc.read_text(encoding="utf-8") == first, "a second --write-host-block was not a no-op")
    expect(first.count(BLOCK_BEGIN) == 1, "the block was duplicated")

    preserved = splice_block("build --curses=no\n" + first, block)
    expect("--curses=no" in preserved, "splicing dropped a line outside the markers")
    expect(preserved.count(BLOCK_END) == 1, "splicing duplicated the end marker")

    # Mutate one flag out, prove the mutation landed, red, then write the bytes
    # back and prove the restore landed. Never `git checkout --`: that restores
    # from the index, and the whole run would be about the index's text.
    mutated = "\n".join(line for line in first.splitlines() if "--jobs=" not in line) + "\n"
    _write(rc, mutated)
    expect("--jobs=" not in rc.read_text(encoding="utf-8"), "the --jobs mutation did not land")
    expect(
        codes(check_host_rc(rc.read_text(encoding="utf-8"), None), FAIL) == ["hostrc-incomplete"],
        "a host block missing --jobs did not red",
    )
    _write(rc, first)
    expect(rc.read_text(encoding="utf-8") == first, "the restore did not land")
    expect(not codes(check_host_rc(rc.read_text(encoding="utf-8"), None), FAIL), "restore not green")

    tmp_rc = first.replace(str(home / ".cache" / "ocx" / "bazel-root"), "/tmp/bazel-root")
    expect("/tmp/bazel-root" in tmp_rc, "the /tmp mutation did not land")
    expect(
        codes(check_host_rc(tmp_rc, None), FAIL) == ["hostrc-tmp-output-root"],
        "an output root under /tmp did not red",
    )

    expect(
        codes(check_host_rc(first, 'build --remote_header="a=b c"\n'), FAIL) == ["userrc-host-state"],
        "host state in .bazelrc.user did not red",
    )
    expect(
        not codes(check_host_rc(first, "# empty\nbuild --curses=no\n"), FAIL),
        "an ordinary .bazelrc.user override was refused",
    )
    print("  host block: absent red, written green, idempotent, /tmp red, .bazelrc.user red")
    return 9


def prove_probe(scratch: Path) -> int:
    """The probe against a local server whose status code this test chose.

    A proof that reached the real `bazel-cache.ocx.sh` would be green or red for
    reasons this repository does not control, which is not a proof of the code.
    """
    import http.server
    import threading

    status_box = {"code": 401}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            self.send_response(status_box["code"])
            self.send_header("Content-Length", "0")
            self.end_headers()

        def log_message(self, *args: object) -> None:
            return

    server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base = f"http://127.0.0.1:{server.server_port}/v1"
    rc_text = 'build --remote_header="authorization=Basic QUJD"\n'
    try:
        expectations = {
            200: ["cache-credential"],
            404: ["cache-credential"],
            401: ["cache-credential-401"],
            403: ["cache-credential-blocked"],
            500: ["cache-credential-unknown"],
        }
        for code, expected in expectations.items():
            status_box["code"] = code
            got = codes(check_credential(rc_text, base))
            expect(got == expected, f"HTTP {code} produced {got}, expected {expected}")
        status_box["code"] = 401
        expect(
            codes(check_credential("", base)) == ["cache-credential-absent"],
            "an rc with no --remote_header did not red before probing",
        )
        status_box["code"] = 200
        got = codes(check_credential("build --remote_header=authorization=Basic QUJD\n", base))
        expect(
            got == ["cache-credential-absent", "credential-unquoted"],
            f"the unquoted line gave {got} — it must red as unquoted AND as no usable credential",
        )
    finally:
        server.shutdown()
        server.server_close()
    expect(
        codes([credential_verdict("URLError")]) == ["cache-credential-unknown"],
        "an unreachable host was not reported as unknown",
    )
    print("  cache probe: 200/404 green, 401 red, 403 red as *blocked* (not as a non-401), 500 red")
    return 8


def prove_secret(scratch: Path) -> int:
    """The credential path, and the guarantee that the password reaches no output.

    The leak detector gets its own red: a detector that never fires is green over
    a file that prints the secret on every line.
    """
    secret = "correct-horse-battery-staple"
    home = scratch / "cred"
    home.mkdir()
    rc = home / ".bazelrc"
    _write(rc, "startup --output_user_root=/home/x/.cache/ocx/bazel-root\n")

    written = set_reader_credential(rc.read_text(encoding="utf-8"), READER_USER, secret)
    _write(rc, written)
    text = rc.read_text(encoding="utf-8")

    expect(not leaks(secret, text), "the plaintext password was written into ~/.bazelrc")
    expect(leaks(secret, f"password={secret}"), "the leak detector does not fire on a real leak")

    token = base64.b64encode(f"{READER_USER}:{secret}".encode()).decode("ascii")
    expect(f'--remote_header="authorization=Basic {token}"' in text, "the credential line is wrong")
    good, broken = split_header_lines(rc_lines(text))
    expect(len(good) == 1 and not broken, "the written line does not survive the tokenizer")
    expect("--output_user_root" in text, "writing the credential dropped the rest of the file")

    again = set_reader_credential(text, READER_USER, secret)
    expect(again.count("--remote_header") == 1, "a second write duplicated the credential line")

    broken_rc = "build --remote_header=authorization=Basic QUJD\n"
    fixed = set_reader_credential(broken_rc, READER_USER, secret)
    _, still_broken = split_header_lines(rc_lines(fixed))
    expect(not still_broken, "rewriting did not remove the pre-existing unquoted line")

    # Nothing this module prints may carry the password, and nothing may take it
    # from argv or the environment — both are world-readable through `ps -eo args`
    # and `/proc/<pid>/environ`.
    stdout = _capture_main(["--set-reader-credential", "--rc", str(rc)], stdin=secret + "\n")
    expect(not leaks(secret, stdout), f"--set-reader-credential echoed the password: {stdout!r}")
    expect("Basic" not in stdout, "the encoded credential was echoed")
    expect(leaks(secret, stdout + secret), "the output leak detector does not fire on a leak")

    options = {
        option for action in build_parser()._actions for option in action.option_strings
    }
    secret_bearing = sorted(
        option for option in options if re.search(r"password|secret|token", option)
    )
    expect(not secret_bearing, f"the parser takes a secret on argv: {secret_bearing}")
    control = argparse.ArgumentParser(add_help=False)
    control.add_argument("--password")
    expect(
        any(
            re.search(r"password|secret|token", option)
            for action in control._actions
            for option in action.option_strings
        ),
        "the argv-secret detector does not fire on a parser that takes --password",
    )
    print("  credential: encoded not echoed, quoted, idempotent, replaces a broken line, argv-free")
    return 12


def _capture_main(argv: list[str], stdin: str) -> str:
    """Run `main` with stdout, stderr and stdin redirected, and return everything
    it wrote. Both streams, because a secret printed to stderr is still printed."""
    import contextlib
    import io

    buffer = io.StringIO()
    saved = sys.stdin
    sys.stdin = io.StringIO(stdin)
    try:
        with contextlib.redirect_stdout(buffer), contextlib.redirect_stderr(buffer):
            main(argv)
    finally:
        sys.stdin = saved
    return buffer.getvalue()


def prove_pure_checks() -> int:
    """The remaining four, each red and green on inputs built here."""
    toml = '[tools]\nbazel = "ocx.sh/bazelbuild/bazel:9.2.0"\n'
    lock = 'name = "bazel"\n'
    expect(toolchain_pin(toml) == "9.2.0", "the ocx.toml pin did not parse")
    expect(toolchain_pin("[tools]\nbun = \"x:1\"\n") is None, "a missing pin was invented")
    expect(not codes(check_toolchain(toml, lock, "9.2.0", "bazel 9.2.0", 0, None), FAIL), "green")
    expect(
        codes(check_toolchain(toml, None, "9.2.0", "bazel 9.2.0", 0, None), FAIL)
        == ["toolchain-lock"],
        "an unlocked pin did not red",
    )
    expect(
        codes(check_toolchain(toml, lock, "9.1.0", "bazel 9.2.0", 0, None), FAIL)
        == ["toolchain-drift"],
        ".bazelversion drift did not red",
    )
    expect(
        codes(check_toolchain(toml, lock, "9.3.0", "bazel 9.3.0", 0, None), FAIL)
        == ["toolchain-pin-drift"],
        "a binary that is not the ocx.toml pin did not red",
    )
    expect(
        codes(check_toolchain(toml, lock, "9.2.0", "bazel 9.2.0", 0, "/usr/bin/bazelisk"), FAIL)
        == ["toolchain-bazelisk"],
        "bazelisk on PATH did not red",
    )

    fedora = 'ID=fedora\nVERSION_ID=43\n'
    expect("gcc-c++" in distro_command(fedora), "the Fedora command is wrong")
    expect("libstdc++-devel" in distro_command(fedora), "the F43 -devel trap is not stated")
    expect("apt install g++" in distro_command('ID=ubuntu\nID_LIKE=debian\n'), "debian wrong")
    expect("install your" in distro_command("ID=haiku\n"), "an unknown distro must stay generic")
    expect(not codes(check_cxx("/usr/lib64/libstdc++.so", True, None, False, fedora), FAIL), "green")
    expect(
        codes(check_cxx("libstdc++.so", False, None, False, fedora), FAIL) == ["cxx-missing"],
        "gcc echoing the bare name back did not red",
    )
    expect(
        codes(check_cxx("libstdc++.so", False, "/home/x/libdir", True, fedora), FAIL) == [],
        "the symlink farm fallback did not count as covered",
    )
    expect(
        codes(check_cxx("libstdc++.so", False, "/home/x/libdir", True, fedora), WARN) == ["cxx-farm"],
        "the farm fallback must warn, not pass silently",
    )

    home = "/home/x/.cache/ocx"
    expect(version_tuple("bazel 9.2.0") == (9, 2, 0), "the version did not parse")
    expect(version_tuple(None) == (), "an absent version must be empty, not zero")
    expect(
        not codes(check_caches(f"{home}/disk", True, f"{home}/root", False, (9, 2, 0)), FAIL),
        "the shipped configuration did not pass",
    )
    expect(
        codes(check_caches("/tmp/disk", True, f"{home}/root", False, (9, 2, 0)), FAIL)
        == ["disk-cache-tmp"],
        "a disk cache under /tmp did not red",
    )
    expect(
        codes(check_caches(f"{home}/disk", True, "/tmp/root", False, (9, 2, 0)), FAIL)
        == ["output-root-tmp"],
        "an output root under /tmp did not red",
    )
    expect(
        codes(check_caches(None, False, f"{home}/root", False, (9, 2, 0)), FAIL)
        == ["disk-cache-absent"],
        "no disk cache at all did not red",
    )
    expect(
        codes(check_caches(f"{home}/disk", True, f"{home}/root", False, (9, 3, 0)), FAIL)
        == ["disk-cache-unbounded"],
        "the GC reopen condition did not red once the pin crossed 9.3.0",
    )
    expect(
        not codes(check_caches(f"{home}/disk", True, f"{home}/root", True, (9, 3, 0)), FAIL),
        "GC flags set on 9.3.0 must be green",
    )

    expect(not codes(check_maintainer("member", None), FAIL), "a contributor must not be failed")
    expect(
        codes(check_maintainer("admin", frozenset()), WARN) == ["maintainer-secrets"],
        "an admin with no org secrets did not warn",
    )
    expect(
        not codes(check_maintainer("admin", frozenset(ORG_SECRETS)), WARN),
        "an admin with both secrets set must be green",
    )

    expect(find_fc_list(which=lambda _: None, exists=lambda p: p == "/usr/sbin/fc-list")
           == "/usr/sbin/fc-list", "fc-list was not found off PATH in /usr/sbin")
    expect(find_fc_list(which=lambda _: None, exists=lambda _: False) is None, "invented fc-list")
    expect(codes(check_fonts(None, None), WARN) == ["fonts-unknown"], "unresolvable fc-list")
    expect(codes(check_fonts("/usr/sbin/fc-list", []), WARN) == ["fonts-absent"], "no families")
    expect(not codes(check_fonts("/usr/sbin/fc-list", ["Adwaita Mono"]), WARN), "families green")
    print("  pure checks: toolchain, C++, caches, maintainer and fonts each shown red and green")
    return 29


def prove_bep_modes(scratch: Path) -> int:
    """Red on a planted 0644 stream, green at 0600, absent root named as absent.

    Real files under real roots rather than a mocked `os.walk`: the subject is a
    file mode on disk, and a mode nothing ever set is a proof about a mock.
    """
    present = scratch / "bep-root"
    (present / "target").mkdir(parents=True)
    absent = scratch / "bep-root-that-is-not-there"

    planted = [
        present / "target" / "bep.json",
        present / "target" / "test-bep.json",
        present / "build_event_json_file",
        present / "cold.bep.json",
    ]
    # The positive control for the matcher: three of the four names this tree
    # actually writes do NOT end in `.bep.json`, so the narrow glob first
    # proposed for this check would have reported a clean root over them.
    expect(
        not any(path.name.endswith(".bep.json") for path in planted[:3]),
        "the control is wrong — `*.bep.json` was supposed to miss these three",
    )
    for path in planted:
        _write(path, "{}\n")
        path.chmod(0o644)
        expect(
            path.stat().st_mode & 0o777 == 0o644,
            f"the 0644 mutation on {path} did not land, so the red below is about nothing",
        )

    exposed, read_roots, missing, seen = bep_scan([present, absent])
    expect(sorted(exposed) == sorted(planted), f"the scan found {exposed}, not the four planted")
    expect(read_roots == [present], "the root that exists was not recorded as read")
    expect(missing == [absent], "the root that does not exist was not recorded as absent")
    expect(seen >= len(planted), "the walk reported fewer files than it was handed")
    red = check_bep_modes(exposed, read_roots, missing, seen)
    expect(codes(red, FAIL) == ["bep-world-readable"], "a 0644 BEP did not red")
    expect("chmod 600" in red[0].fix, "the finding arrived without its remediation")

    for path in planted:
        path.chmod(0o600)
        expect(
            path.stat().st_mode & 0o777 == 0o600,
            f"the 0600 restore on {path} did not land, so the green below is about nothing",
        )
    exposed, read_roots, missing, seen = bep_scan([present, absent])
    expect(exposed == [], f"0600 streams were still reported exposed: {exposed}")
    green = check_bep_modes(exposed, read_roots, missing, seen)
    expect(not codes(green, FAIL) and not codes(green, WARN), "0600 did not clear the check")
    expect(
        "not present" in green[0].detail and str(absent) in green[0].detail,
        "an absent root must be named as not present, never folded into the green",
    )

    # Neither symlink leg may leave the root, and one fixture reds both ways of
    # getting that wrong. The target is a 0644 stream *outside* the root, so
    # following the file link reports it exposed, and judging the link by its own
    # bits reports it exposed too (a symlink is 0777 on Linux). Only skipping is
    # green. The directory link is the `followlinks=False` half.
    outside = scratch / "outside-bep.json"
    _write(outside, "{}\n")
    outside.chmod(0o644)
    expect(
        bep_scan([outside.parent])[0] == [outside],
        "the control is wrong — the out-of-root 0644 fixture must red when it IS in scope",
    )
    (present / "link-bep.json").symlink_to(outside)
    (present / "link-dir").symlink_to(scratch, target_is_directory=True)
    expect(bep_scan([present])[0] == [], "the walk left the root through a symlink")

    # A root on disk that the walk did not descend is not a clean tree.
    expect(
        codes(check_bep_modes([], [present], [], 0), WARN) == ["bep-scan-empty"],
        "a walk that read nothing reported clean",
    )
    expect(is_bep_name("warm.bep.json") and not is_bep_name("Cargo.toml"), "the matcher is wrong")
    print("  BEP modes: 0644 red on four real names, 0600 green, absent root named, symlink skipped")
    return 22


def self_test() -> int:
    scratch = REPO_ROOT / ".tmp"
    scratch.mkdir(exist_ok=True)
    checks = 0
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        work = Path(directory)
        checks += prove_rc_reading()
        checks += prove_host_block(work)
        checks += prove_probe(work)
        checks += prove_secret(work)
        checks += prove_bep_modes(work)
        checks += prove_pure_checks()
    print(
        f"bazel doctor self-test: {checks} checks passed — every doctor check shown red and "
        "green on fixtures built here, the probe against a local server whose status code this "
        "test chose, and the password proven absent from every stream the tool writes"
    )
    return 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    """No option anywhere takes a secret. `--set-reader-credential` reads the
    password from stdin, because argv is world-readable through `ps -eo args`
    and the environment through `/proc/<pid>/environ` (PY-SEC-03)."""
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="run every check, red/green")
    mode.add_argument("--self-test", action="store_true", help="prove every check red and green")
    mode.add_argument("--print-host-block", action="store_true", help="the ~/.bazelrc stanza")
    mode.add_argument("--write-host-block", action="store_true", help="write it, idempotently")
    mode.add_argument(
        "--set-reader-credential",
        action="store_true",
        help=f"read the {READER_USER} password from STDIN and write the header to ~/.bazelrc",
    )
    parser.add_argument("--rc", type=Path, help="the rc file to read or write (default ~/.bazelrc)")
    parser.add_argument("--jobs", type=int, default=12, help="--write-host-block: build --jobs")
    parser.add_argument("--heap-gb", type=int, default=2, help="--write-host-block: JVM -Xmx")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    home = Path.home()
    rc_path = args.rc or (home / ".bazelrc")

    if args.self_test:
        return self_test()

    if args.print_host_block:
        print(host_block(home, jobs=args.jobs, heap_gb=args.heap_gb), end="")
        return 0

    if args.write_host_block:
        block = host_block(home, jobs=args.jobs, heap_gb=args.heap_gb)
        before = read(rc_path) or ""
        after = splice_block(before, block)
        rc_path.write_text(after, encoding="utf-8")
        print(f"{rc_path}: host block {'unchanged' if before == after else 'written'}")
        return 0

    if args.set_reader_credential:
        password = sys.stdin.read().strip()
        if not password:
            print(
                f"bazel doctor: no password on stdin. Pipe the {READER_USER} password in; it is "
                "never taken on argv, which `ps -eo args` exposes",
                file=sys.stderr,
            )
            return 1
        rc_path.write_text(
            set_reader_credential(read(rc_path) or "", READER_USER, password), encoding="utf-8"
        )
        # Deliberately says nothing about the value — not its length, not a
        # prefix, not a hash. Re-probe for the verdict.
        print(f"{rc_path}: reader credential written (quoted). Re-run `task bazel:doctor`.")
        return 0

    return render(gather(home, REPO_ROOT))


if __name__ == "__main__":
    raise SystemExit(main())
