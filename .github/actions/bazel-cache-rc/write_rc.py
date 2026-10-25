"""Write the CI-only Bazel cache rc file and the credential helper it names.

Run by `.github/actions/bazel-cache-rc`, never by hand. The rc file is two
lines: the helper registered for the cache host, and the upload grant.

* **BZL-CACHE-04** — Bazel resolves and spawns a `--credential_helper` with the
  full client environment, before the sandbox, with no check on where the flag
  came from, so a read-only `bazel query //...` on a fresh clone is enough to
  execute it (bazel#30439, closed by the vendor as intended). The helper
  therefore lives under `$RUNNER_TEMP`, named by an rc file that never ships
  with the repository and is never `%workspace%`-relative.
* **The grant rides with the credential** — the workspace `.bazelrc` sets
  `--remote_upload_local_results=false` and `--bazelrc` loads after it, so the
  `=true` here wins. No credential, no file, no grant: an empty `WRITE_AUTH` (a
  fork PR, to which GitHub gives no secrets) writes nothing at all.
* **One credential source** — an rc Bazel already reads that configures a cache
  credential is refused, because Bazel sends an `Authorization` header per source
  rather than the last one and the origin would pick between them.

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
#: header. The optional quote is the rc tokenizer's.
AUTHORIZATION_HEADER = re.compile(r"--remote_header=[\"']?authorization=", re.IGNORECASE)

#: A credential helper, however spelled. The host half is deliberately not parsed: a
#: guard that reads the host is a guard that a different spelling of the same host
#: walks straight past, and there is no legitimate second helper to tell it apart from.
CREDENTIAL_HELPER = re.compile(r"--credential_helper[= ]")

#: `import` / `try-import`. An rc file's own text is an incomplete answer without it:
#: this repository's `.bazelrc` ends in `try-import %workspace%/.bazelrc.user`, which
#: is gitignored — exactly the source a scan of *tracked* files cannot see. The target
#: is captured raw and parsed shell-style, since an import may be single-quoted.
RC_IMPORT = re.compile(r"(?m)^[ \t]*(?:try-)?import[ \t]+(\S.*?)[ \t]*$")

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
        for raw in RC_IMPORT.findall(RC_COMMENT.sub("", text)):
            try:
                target = shlex.split(raw)[0]
            except ValueError:
                continue  # unfollowable, so `other_credential_sources` refuses the line itself
            pending.append(path.parent / target.replace("%workspace%", str(workspace)))


def unparseable_import(line: str) -> bool:
    """An `import` whose target no shell-style parse recovers.

    Bazel keeps a `#` inside quotes and accepts a missing end-quote, so the file behind
    such a line may well be read; a guard that cannot follow it cannot clear it.
    """
    match = RC_IMPORT.match(line)
    if not match:
        return False
    try:
        shlex.split(match.group(1))
    except ValueError:
        return True
    return False


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
        if AUTHORIZATION_HEADER.search(line) or CREDENTIAL_HELPER.search(line) or unparseable_import(line)
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


def split_credential(auth: str) -> tuple[str, str]:
    """`<header>=<value>`, or exit with a diagnostic that is not the leak.

    `partition` splits on the FIRST `=`. The value is `Basic <base64>` and
    base64 carries `=` as its padding, so splitting on the last one truncates
    the credential into something that authenticates nothing.
    """
    header, separator, value = auth.partition("=")
    if not separator or not header.strip() or not value.strip():
        # The length, never the value: a diagnostic must not become the leak.
        sys.exit(
            f"bazel cache: the write credential is not `<header>=<value>` "
            f"({len(auth)} characters); expected "
            f"`authorization=Basic <base64 of user:password>`"
        )
    return header.strip(), value.strip()


def main() -> None:
    write_auth = os.environ.get("WRITE_AUTH", "")
    if not write_auth:
        # The fork-PR lane, to which GitHub gives no secrets. Only the exact empty
        # string reaches here: anything else that is not a well-formed credential
        # fails loudly in `split_credential`, because a silently credential-less
        # lane is green forever at full cost.
        print(f"bazel cache: no write credential on this lane, no {RC_NAME} written")
        return

    runner_temp = os.environ.get("RUNNER_TEMP", "")
    if not runner_temp:
        sys.exit("bazel cache: RUNNER_TEMP is unset; this action only runs on a runner")
    # Inside quotes Bazel drops each backslash and splits lines before tokenizing, so only these change the path.
    if re.search(r"['\\\n\r]", runner_temp):
        sys.exit(
            "bazel cache: RUNNER_TEMP contains a single quote, backslash or line break, "
            "which a Bazel rc line cannot carry"
        )

    configured = other_credential_sources()
    if configured:
        # Sources the calling workflow cannot see: a runner's `~/.bazelrc`, a system
        # rc, an `import`ed `.bazelrc.user`. Bazel applies all of them to the same
        # invocation, so writing a credential beside one adds a second source.
        sys.exit(
            f"bazel cache: two credential sources on one lane — a Bazel cache "
            f"credential is already configured at {', '.join(configured)}, and this "
            f"action would add a second one that every invocation reading that rc "
            f"file also sends. Remove that line, or do not write this credential. "
            f"(The line's value is deliberately not repeated here.)"
        )

    header, value = split_credential(write_auth)
    temp = pathlib.Path(runner_temp)
    helper = temp / HELPER_NAME
    rc = temp / RC_NAME
    # `json.dumps`, never a hand-assembled string: the credential is external
    # input, and a hand-rolled emitter is where escaping bugs hide past every
    # local fixture (quality-core.md § Don't Own Non-Domain Code).
    response = json.dumps({"headers": {header: [value]}})
    create(helper, helper_program(response), 0o700)
    create(
        rc,
        # Single-quoted: Bazel reads an rc line shell-style, so a path with a space would split in two.
        f"build --credential_helper={CACHE_HOST}='{helper}'\n"
        "build --remote_upload_local_results=true\n",
        0o600,
    )
    print(f"bazel cache: wrote {rc} (600) and {helper} (700)")


if __name__ == "__main__":
    main()
