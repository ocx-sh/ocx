"""Write the CI-only Bazel cache rc file and the credential helper it names.

Run by `.github/actions/bazel-cache-rc`, never by hand. **Two credentials, and
they are not symmetric.** The read credential is a plain `--remote_header` line
— a value Bazel sends, never one it runs. The write credential cannot be,
because of BZL-CACHE-04 below, and reaches Bazel through a helper program
instead.

* **BZL-CACHE-04** — Bazel resolves and spawns a `--credential_helper` with the
  full client environment, before the sandbox, with no check on where the flag
  came from, so a read-only `bazel query //...` on a fresh clone is enough to
  execute it (bazel#30439, closed by the vendor as intended). The helper
  therefore lives under `$RUNNER_TEMP`, named by an rc file that never ships
  with the repository and is never `%workspace%`-relative.
* **BZL-CACHE-01** — every lane an untrusted contributor can trigger runs the
  cache read-only with the write credential *absent*, not merely unused. An
  empty `WRITE_AUTH` is that state, and it emits no `--credential_helper` line
  at all, which is a stronger statement than an rc file holding an empty
  credential. With *both* variables empty — the fork-PR lane, which GitHub
  gives no secrets — no rc file is written at all, and the lane reads the cache
  anonymously over `--disk_cache`. That is the documented fallback, not a
  failure: a step that fails loudly on a fork is the defect.
* **DX-81, one credential source per lane** — Bazel `add`s an `Authorization`
  header per configured source rather than replacing it, and sends all of them:
  measured against a local sink with both routes live, one cache `PUT` carried
  three. Which one an origin honours is that origin's business, and a design
  that needs to know is the defect. So a lane presents exactly one credential,
  and `main()` below refuses rather than write a second — both variables set at
  once, or a credential already configured in an rc file Bazel will read.

The credential arrives in the environment and is never passed as an argument:
`/proc/<pid>/cmdline` is readable by any process of that user, and `set -x`
traces expanded arguments.
"""

import json
import os
import pathlib
import re
import shlex
import sys
from collections.abc import Iterator

#: The one host this repository's `--remote_cache` points at. A
#: `--credential_helper` is registered per host, so this helper is never
#: consulted for anything else Bazel dials.
CACHE_HOST = "bazel-cache.ocx.sh"

#: Both names are spelled once more in `action.yml`'s cleanup step, which is the
#: one place that must delete exactly what this writes.
RC_NAME = "bazel-cache.rc"
HELPER_NAME = "bazel-cache-helper"

#: The helper's heredoc delimiter. `json.dumps` never emits a raw newline inside
#: its output — a newline in a string becomes the two characters backslash-n —
#: so the document is always a single line and can never contain this word at
#: the start of a line of its own.
DELIMITER = "OCX_BAZEL_CACHE_CREDENTIAL"

#: A `--remote_header` carrying an `Authorization`. The header name is matched
#: case-insensitively because HTTP header names are — Bazel passes the name through
#: verbatim, so `Authorization=` and `authorization=` arrive as one and the same
#: header. The optional quote is the rc tokenizer's, which `header_line` below emits.
AUTHORIZATION_HEADER = re.compile(r"--remote_header=[\"']?authorization=", re.IGNORECASE)

#: A credential helper, however spelled. The host half is deliberately not parsed: a
#: guard that reads the host is a guard that a different spelling of the same host
#: walks straight past, and there is no legitimate second helper to tell it apart from.
CREDENTIAL_HELPER = re.compile(r"--credential_helper[= ]")

#: `import` / `try-import`. An rc file's own text is an incomplete answer without it:
#: this repository's `.bazelrc` ends in `try-import %workspace%/.bazelrc.user`, which
#: is gitignored — exactly the source a scan of *tracked* files cannot see.
RC_IMPORT = re.compile(r"(?m)^[ \t]*(?:try-)?import[ \t]+(\S+)[ \t]*$")

#: An rc comment, to end of line. A flag that survives this is one Bazel is passed.
RC_COMMENT = re.compile(r"(?m)#.*$")


def rc_chain(workspace: pathlib.Path, home: pathlib.Path) -> Iterator[tuple[pathlib.Path, str]]:
    """Every rc file Bazel reads on this runner, with its text, `import`s followed.

    The system rc, the workspace rc and the home rc — the three Bazel applies without
    being asked. The `--bazelrc=` the lane passes names the file this module is about
    to write and nothing else, so it is not in the walk.

    **Not `bazel canonicalize-flags`, and not because it is dearer.** Measured on the
    pinned 9.2.0 against a workspace whose `.bazelrc`, whose `~/.bazelrc` and whose
    third `--bazelrc=` file each carried a credential: `canonicalize-flags` echoed back
    only the flags handed to it on its own command line (`--remote_header=…CMDLINE`
    alone), and with no flags given it printed nothing at all. A guard built on it is
    green over every rc-sourced duplicate there is, which is the shape of a check that
    never ran. `bazel --announce_rc` *does* report the whole chain, correctly and in
    0.1 s — by printing every credential in it to stderr, which would put the value in
    a buffer this module exists to keep it out of.
    """
    seen: set[pathlib.Path] = set()
    pending = [pathlib.Path("/etc/bazel.bazelrc"), workspace / ".bazelrc", home / ".bazelrc"]
    while pending:
        path = pending.pop(0)
        if path in seen:
            continue
        seen.add(path)
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            # Absent, a directory, or unreadable. Only a file that exists can
            # configure anything, and Bazel skips a `try-import` miss the same way.
            continue
        yield path, text
        for target in RC_IMPORT.findall(RC_COMMENT.sub("", text)):
            pending.append(path.parent / target.replace("%workspace%", str(workspace)))


def other_credential_sources() -> list[str]:
    """`<path>:<line>` for every cache credential already configured for this Bazel.

    The matched text is never echoed — a diagnostic must not become the leak — so a
    finding is a file name and a line number, which is what a human needs to remove it
    and is not itself a secret.
    """
    workspace = pathlib.Path(os.environ.get("GITHUB_WORKSPACE") or os.getcwd())
    home = pathlib.Path(os.environ.get("HOME") or "/nonexistent")
    return [
        f"{path}:{number}"
        for path, text in rc_chain(workspace, home)
        for number, line in enumerate(RC_COMMENT.sub("", text).splitlines(), start=1)
        if AUTHORIZATION_HEADER.search(line) or CREDENTIAL_HELPER.search(line)
    ]


def create(path: pathlib.Path, text: str, mode: int) -> None:
    """Create `path` fresh and owner-only, never writing through what is there.

    `$RUNNER_TEMP` is not private by construction (ADR ruling 4b line 2): an
    entry already at this path is either a leftover or, on a reused self-hosted
    runner, something an earlier job planted, and writing *through* a symlink
    would put the credential somewhere this action then cannot delete. Unlink
    first, then `O_EXCL`, so the file whose mode is set below is the file this
    call created.

    The explicit `chmod` is not redundant with the caller's `umask 077`:
    `open(2)` is umask-clipped and `chmod(2)` is not, so this is what makes the
    mode a property of the code rather than of the environment it inherited.
    """
    path.unlink(missing_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        stream.write(text)
    path.chmod(mode)


def helper_program(response: str) -> str:
    """The credential helper, as text.

    Bazel runs it as `<helper> get` with the request JSON on stdin and reads the
    response from stdout. There is one host and one credential, so the request
    is drained rather than parsed — drained rather than ignored, because a
    helper that exits before Bazel has finished writing the request turns a
    working credential into an intermittent broken pipe.
    """
    return (
        "#!/bin/sh\n"
        "# Written by .github/actions/bazel-cache-rc. Deleted with the rc file\n"
        "# it is named from, in the caller's `if: always()` cleanup step.\n"
        "cat >/dev/null\n"
        f"cat <<'{DELIMITER}'\n{response}\n{DELIMITER}\n"
    )


def split_credential(auth: str, variable: str) -> tuple[str, str]:
    """`<header>=<value>`, or exit with a diagnostic that is not the leak.

    `partition` splits on the FIRST `=`. The value is `Basic <base64>` and
    base64 carries `=` as its padding, so splitting on the last one truncates
    the credential into something that authenticates nothing.
    """
    header, separator, value = auth.partition("=")
    if not separator or not header.strip() or not value.strip():
        # The length, never the value: a diagnostic must not become the leak.
        sys.exit(
            f"bazel cache: {variable} is not `<header>=<value>` "
            f"({len(auth)} characters); expected "
            f"`authorization=Basic <base64 of user:password>`"
        )
    return header.strip(), value.strip()


def header_line(auth: str, variable: str) -> str:
    """`build --remote_header=<auth>`, quoted for Bazel's rc tokenizer.

    **The quoting is not cosmetic.** Measured on the pinned 9.2.0: an rc line
    `build --remote_header=authorization=Basic <b64>` is split on whitespace,
    so `<b64>` arrives as a *positional argument* — `bazel info` answers
    `ERROR: unknown key(s): '<b64>'` and exits 2, and a command taking targets
    answers `no such target`. Quoted, the same value arrives whole: verified
    with `--announce_rc`, which echoed `--remote_header=authorization=Basic
    <b64>` back.

    `shlex.quote` does the quoting rather than an f-string with two `"`
    characters in it, because escaping external input by hand is where the bugs
    live (quality-core.md § Don't Own Non-Domain Code). One divergence from
    POSIX was measured and is refused rather than papered over: Bazel's rc
    tokenizer honours a backslash escape *inside* single quotes, where a POSIX
    shell does not, so `A\\$B` round-trips as `A$B`. A `Basic <base64>`
    credential is drawn from `A-Za-z0-9+/=` and can never contain one; a value
    that does would be silently re-decoded into something that authenticates
    nothing, which is exactly the shape of failure that reads as a cache miss.
    """
    if set(auth) & set("\\\n\r"):
        sys.exit(
            f"bazel cache: {variable} contains a backslash or a line break "
            f"({len(auth)} characters). An rc file is line-oriented and its "
            f"tokenizer unescapes a backslash even inside single quotes, so "
            f"such a value would reach Bazel altered. Expected "
            f"`authorization=Basic <base64 of user:password>`"
        )
    split_credential(auth, variable)
    return f"build --remote_header={shlex.quote(auth)}\n"


def main() -> None:
    read_auth = os.environ.get("READ_AUTH", "")
    write_auth = os.environ.get("WRITE_AUTH", "")
    if not read_auth and not write_auth:
        # The fork-PR lane: GitHub withholds secrets from a fork-triggered run,
        # so both bindings are the empty string and this writes nothing. The
        # calling workflow binds `write-auth` to exactly `''` off the `main`
        # push for the same reason. Only that exact empty string reaches here:
        # anything else that is not a well-formed credential fails loudly below
        # rather than degrading into this branch, because a silently
        # credential-less write lane is a lane that is green forever at full
        # cost.
        print(f"bazel cache: no cache credential on this lane, no {RC_NAME} written")
        return

    if read_auth and write_auth:
        # DX-81's by-construction half, refused where the two bindings meet. The
        # caller gates them on one trusted event with one of the two negated, so
        # arriving here means that gate was loosened — and the result would be two
        # `Authorization` headers on every request, resolved by whichever one the
        # origin happens to keep.
        sys.exit(
            "bazel cache: two credential sources on one lane — the read credential and "
            "the write credential are both set. Bazel sends an Authorization header "
            "per source rather than the last one, so which credential authenticates a "
            "request would be the origin's choice. The `main` push presents the write "
            "credential alone (its user reads and writes); every other trigger "
            "presents the read credential alone."
        )

    runner_temp = os.environ.get("RUNNER_TEMP", "")
    if not runner_temp:
        sys.exit("bazel cache: RUNNER_TEMP is unset; this action only runs on a runner")

    configured = other_credential_sources()
    if configured:
        # DX-81's runtime half, over the sources the calling workflow cannot see: a
        # runner's `~/.bazelrc`, a system rc, an `import`ed `.bazelrc.user`. Bazel
        # applies all of them to the same invocation, so writing a credential beside
        # one of these is the two-header state again, reached from the other side.
        sys.exit(
            f"bazel cache: two credential sources on one lane — a Bazel cache "
            f"credential is already configured at {', '.join(configured)}, and this "
            f"action would add a second one that every invocation reading that rc "
            f"file also sends. Remove that line, or do not write this credential. "
            f"(The line's value is deliberately not repeated here.)"
        )

    temp = pathlib.Path(runner_temp)
    helper = temp / HELPER_NAME
    rc = temp / RC_NAME
    lines = []
    wrote = []

    if read_auth:
        lines.append(header_line(read_auth, "the read credential"))
        wrote.append("a --remote_header")

    if write_auth:
        header, value = split_credential(write_auth, "the write credential")
        # `json.dumps`, never a hand-assembled string: the credential is
        # external input, and a hand-rolled emitter is where escaping bugs hide
        # past every local fixture (quality-core.md § Don't Own Non-Domain Code).
        response = json.dumps({"headers": {header: [value]}})
        create(helper, helper_program(response), 0o700)
        lines.append(f"build --credential_helper={CACHE_HOST}={helper}\n")
        wrote.append(f"{helper} (700)")

    create(rc, "".join(lines), 0o600)
    print(f"bazel cache: wrote {rc} (600) holding {', '.join(wrote)}")


if __name__ == "__main__":
    main()
